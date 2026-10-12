//! infomentor-mcp: the command line of packages/infomentor-mcp/src/cli.ts.

mod client;
mod collection;
mod error;
mod html;
mod http;
mod input;
mod jar;
mod js;
mod login;
mod server;
mod session;
mod shapes;
mod signal;
mod store;
mod upstream;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use tokio::signal::unix::{SignalKind, signal};

use crate::client::{Client, Options};
use crate::error::{Fail, Result};
use crate::session::session_path;
use crate::signal::{Controller, Signal};
use crate::store::MigrateResult;
use crate::upstream::Net;

const HELP: &str = "Usage: infomentor-mcp [auth] [command] [options]

Commands:
  login              Sign in over HTTPS using privately injected secrets or a credentials file;
                     the session and that sign-in are saved in the encrypted store
  status             Verify the saved session and show how it is stored
  migrate            Move a session saved by an older version out of its plaintext file;
                     with --credentials, also store that sign-in for automatic renewal
  logout             Delete the local session and stored sign-in (does not revoke it on
                     InfoMentor)
  serve              Start the stdio MCP server (default)

Options:
  --session FILE     Older plaintext session file, read until auth migrate or the next
                     login or import; collection cursors stay beside it (default:
                     ~/.config/infomentor-mcp/session.json, or
                     ~/.infomentor-mcp/session.json when that legacy file exists)
  --credentials FILE Private JSON file with username/password (login, migrate, and renewal
                     when no sign-in is stored)
  --import FILE      login: validate and import a session on a headless machine
  --timeout SECONDS  login: maximum wait (default: 300)
  --allow-account-change
                     login: replace a saved session that belongs to another account
  --allow-setup-tools
                     serve: also register the login, setup-status, cancel, and logout tools
  -h, --help         Show help
  -v, --version      Show version

Environment: INFOMENTOR_SESSION_PATH, INFOMENTOR_CREDENTIALS_FILE,
             INFOMENTOR_USERNAME (kennitala or username), INFOMENTOR_PASSWORD

The session and the stored sign-in are saved encrypted, with the key in a separate private
file: ~/Library/Application Support/family-mcp on macOS; ~/.config/infomentor-mcp with the key
in ~/.local/share/family-mcp/keys on Linux.";

/// What the CLI prints for a failure without a reviewed message.
const FAILED: &str =
    "InfoMentor operation failed. Check the network and session-store permissions.";

#[derive(Default)]
struct Args {
    session: Option<String>,
    credentials: Option<String>,
    import: Option<String>,
    timeout: Option<String>,
    allow_account_change: bool,
    allow_setup_tools: bool,
    help: bool,
    version: bool,
    positionals: Vec<String>,
}

/// node:util `parseArgs` in strict mode with these options and positionals allowed; `None` where
/// it throws.
fn parse_args(args: impl IntoIterator<Item = String>) -> Option<Args> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--" {
            parsed.positionals.extend(args.by_ref());
        } else if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            let flag = match name {
                "help" => &mut parsed.help,
                "version" => &mut parsed.version,
                "allow-account-change" => &mut parsed.allow_account_change,
                "allow-setup-tools" => &mut parsed.allow_setup_tools,
                "session" | "credentials" | "import" | "timeout" => {
                    // The next argument is the value, unless it looks like another option.
                    let value = match inline {
                        Some(value) => value,
                        None => args
                            .next()
                            .filter(|value| !(value.len() > 1 && value.starts_with('-')))?,
                    };
                    *match name {
                        "session" => &mut parsed.session,
                        "credentials" => &mut parsed.credentials,
                        "import" => &mut parsed.import,
                        _ => &mut parsed.timeout,
                    } = Some(value);
                    continue;
                }
                _ => return None,
            };

            if inline.is_some() {
                return None;
            }
            *flag = true;
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            for short in shorts.chars() {
                match short {
                    'h' => parsed.help = true,
                    'v' => parsed.version = true,
                    _ => return None,
                }
            }
        } else {
            parsed.positionals.push(arg);
        }
    }
    Some(parsed)
}

/// `z.coerce.number().int().positive().max(3600)` of the `--timeout` text, in seconds.
fn parse_timeout(value: Option<&str>) -> Result<f64> {
    let seconds = js::number(value.unwrap_or("300"));

    if seconds.fract() != 0.0 || !(1.0..=3600.0).contains(&seconds) {
        return Err(Fail::config(
            "Timeout must be between 0 and 3600 seconds, excluding 0.",
        ));
    }
    Ok(seconds)
}

/// JavaScript truthiness of a string option: an empty value counts as none.
fn given(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|value| !value.is_empty())
}

/// The client options of `--session` and `--credentials`, resolved as `path.resolve` does.
fn options(args: &Args) -> Result<Options> {
    let resolved = |value: &Option<String>| {
        value
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| store::resolve(Path::new(value)))
            .transpose()
    };
    Ok(Options {
        session_file: resolved(&args.session)?,
        credentials_file: resolved(&args.credentials)?,
        keys: None,
    })
}

/// Serve until stdin ends, the connection breaks (a failed stdout write or an input line over
/// 10 MiB), or SIGINT or SIGTERM arrives, then cancel and wait for operations.
async fn serve(allow_setup_tools: bool, options: Options) -> Result<()> {
    let client = Client::new(options).ok_or(Fail::Unknown)?;
    mcp_runtime::serve_stdio(server::InfoMentor::new(allow_setup_tools, client))
        .await
        .map_err(|_| Fail::Unknown)
}

/// SIGINT and SIGTERM as the TypeScript CLI's `process.once` listeners see them: one listener per
/// kind, each running `first` (once in all: it only aborts) on its kind's first arrival. A kind's
/// listener is then gone, so that kind's second arrival ends the process at once, as the default
/// action would, with the status a shell shows for it (128 + the signal number). A SIGINT then a
/// SIGTERM is two first arrivals and ends nothing.
fn on_signals(first: impl FnOnce() + Send + 'static) -> Result<()> {
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| Fail::Unknown)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|_| Fail::Unknown)?;

    tokio::spawn(async move {
        let mut first = Some(first);
        let (mut interrupted, mut terminated) = (false, false);

        loop {
            let (kind, seen) = tokio::select! {
                _ = interrupt.recv() => (SignalKind::interrupt(), &mut interrupted),
                _ = terminate.recv() => (SignalKind::terminate(), &mut terminated),
            };

            if std::mem::replace(seen, true) {
                std::process::exit(128 + kind.as_raw_value());
            }

            if let Some(first) = first.take() {
                first();
            }
        }
    });
    Ok(())
}

/// A signal that the first SIGINT or SIGTERM aborts, so a command ends as cancelled instead of
/// being killed; a second one of the same kind ends the process.
fn cancel_on_signals() -> Result<Signal> {
    let controller = Controller::default();
    let signal = controller.signal();
    on_signals(move || controller.abort())?;
    Ok(signal)
}

/// `status`: verify the saved session; a missing or expired one exits 1 with the next step.
async fn status(options: Options) -> Result<ExitCode> {
    let signal = cancel_on_signals()?;
    let client = Arc::new(Client::new(options).ok_or(Fail::Unknown)?);
    let status = client.session_status(signal).await;
    client.close().await;
    let status = status?;

    match status["authenticated"].as_bool() {
        Some(true) => {
            let storage = status["storage"].as_str().unwrap_or_default();
            eprintln!(
                "{}",
                format!("InfoMentor session is active. {storage}").trim()
            );
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!(
                "{}",
                status["nextStep"]
                    .as_str()
                    .unwrap_or("Sign in with infomentor_login.")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `logout`: through a client, as the TypeScript CLI does, so it cancels and waits like the tool.
async fn logout(options: Options) -> Result<()> {
    // Logout takes no signal, so the first SIGINT or SIGTERM changes nothing, as in TypeScript.
    on_signals(|| {})?;
    let client = Client::new(options).ok_or(Fail::Unknown)?;
    let outcome = client.logout().await;
    client.close().await;
    outcome?;
    eprintln!("Local InfoMentor session and stored sign-in removed.");
    Ok(())
}

/// The session commands. Blocks.
fn command(name: &str, args: &Args, options: &Options, net: &Net, signal: &Signal) -> Result<()> {
    match name {
        "login" => {
            if let Some(file) = args.import.as_deref().filter(|file| !file.is_empty()) {
                login::import_session(
                    net,
                    options,
                    Path::new(file),
                    signal,
                    args.allow_account_change,
                )?;
                eprintln!("Session imported and verified.");
                return Ok(());
            }
            let seconds = parse_timeout(args.timeout.as_deref())?;
            let file = login::login(
                net,
                options,
                signal,
                args.allow_account_change,
                seconds * 1000.0,
            )?;
            eprintln!("Signed in. Session saved in the encrypted store.");

            if let Some(file) = file {
                eprintln!(
                    "{}",
                    store::delete_credentials_advice(args.credentials.as_deref().unwrap_or(&file))
                );
            }
            Ok(())
        }
        "migrate" => {
            let legacy = session_path(options.session_file.as_deref())?;
            let migrated = store::migrate(
                &legacy,
                options.credentials_file.as_deref(),
                options.keys.clone(),
            )?;

            if migrated == MigrateResult::Migrated {
                eprintln!(
                    "Moved the InfoMentor session into the encrypted store and removed its file."
                );

                if let Some(file) = args.credentials.as_deref().filter(|file| !file.is_empty()) {
                    eprintln!("{}", store::delete_credentials_advice(file));
                }
                return Ok(());
            }
            eprintln!(
                "{}",
                match migrated {
                    MigrateResult::Already =>
                        "The InfoMentor session is already in the encrypted store.",
                    _ => {
                        "The InfoMentor session is already in the encrypted store; removed the leftover plaintext file."
                    }
                }
            );

            if given(&args.credentials) {
                eprintln!(
                    "Your sign-in was not stored. Run infomentor-mcp login --credentials FILE to store it."
                );
            }
            Ok(())
        }
        _ => Err(Fail::config("Unknown command. Run infomentor-mcp --help.")),
    }
}

async fn main_async() -> Result<ExitCode> {
    let raw: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let Some(args) = parse_args(raw.iter().cloned()) else {
        // Retired: a stale password form can submit to a replacement loopback listener.
        let local_form = raw
            .iter()
            .any(|arg| arg == "--local-form" || arg.starts_with("--local-form="));
        return Err(Fail::config(match local_form {
            true => {
                "--local-form has been removed. Use --credentials with a private JSON file or privately inject INFOMENTOR_USERNAME and INFOMENTOR_PASSWORD."
            }
            false => "Invalid arguments. Run infomentor-mcp --help.",
        }));
    };

    if args.help {
        println!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }

    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }

    // A test build refuses to start without a local fake upstream.
    #[cfg(feature = "test-origin")]
    upstream::test_origin();

    // The store is checked before anything serves or touches it; help and version never do.
    if !tokio::task::spawn_blocking(store::check_store_at_startup)
        .await
        .unwrap_or(Err(Fail::Unknown))?
    {
        return Ok(ExitCode::FAILURE);
    }

    // `auth login` and `login` are the same command.
    let auth = args
        .positionals
        .first()
        .is_some_and(|first| first == "auth");
    let words = &args.positionals[usize::from(auth)..];

    if words.len() > 1 || (auth && words.is_empty()) {
        return Err(Fail::config(
            "Unexpected arguments. Run infomentor-mcp --help.",
        ));
    }
    let name = words.first().map_or("serve", String::as_str).to_owned();

    if name != "login" && (given(&args.import) || given(&args.timeout) || args.allow_account_change)
    {
        return Err(Fail::config("Login options only apply to login."));
    }

    if name != "serve" && args.allow_setup_tools {
        return Err(Fail::config("Server options only apply to serve."));
    }

    if given(&args.import) && given(&args.credentials) {
        return Err(Fail::config("Choose session import or login, not both."));
    }

    if name == "serve" {
        return serve(args.allow_setup_tools, options(&args)?)
            .await
            .map(|()| ExitCode::SUCCESS);
    }

    if name == "status" {
        return status(options(&args)?).await;
    }

    if name == "logout" {
        return logout(options(&args)?).await.map(|()| ExitCode::SUCCESS);
    }
    let options = options(&args)?;
    let net = Net::new().ok_or(Fail::Unknown)?;
    // Migrate takes no signal either, so its first SIGINT or SIGTERM changes nothing.
    let signal = cancel_on_signals()?;
    tokio::task::spawn_blocking(move || command(&name, &args, &options, &net, &signal))
        .await
        .unwrap_or(Err(Fail::Unknown))
        .map(|()| ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    // current_thread: one stdio client and I/O-bound work. Store work runs on blocking threads,
    // which wait for requests while this thread drives them.
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Fail::Unknown)
        .and_then(|runtime| {
            let outcome = runtime.block_on(main_async());
            // Every operation has ended; only the stdin reader may still block, so do not wait.
            runtime.shutdown_background();
            outcome
        });

    match outcome {
        Ok(code) => code,
        Err(fail) => {
            // Only reviewed diagnostics cross the terminal boundary; library messages may hold secrets.
            eprintln!("{}", mcp_runtime::cli_text(fail, FAILED));
            #[cfg(feature = "test-origin")]
            error::record(fail);

            match fail.is(error::Code::Cancelled) {
                true => ExitCode::from(130),
                false => ExitCode::FAILURE,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Option<Args> {
        parse_args(list.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn options_parse_like_node_parse_args() {
        let parsed = args(&[
            "auth",
            "-hv",
            "--timeout",
            "-",
            "--import=--x",
            "--credentials",
            "",
            "--allow-setup-tools",
            "--",
            "--help",
        ])
        .unwrap();
        assert!(parsed.help && parsed.version && parsed.allow_setup_tools);
        assert!(!parsed.allow_account_change);
        assert_eq!(parsed.timeout.as_deref(), Some("-"));
        assert_eq!(parsed.import.as_deref(), Some("--x"));
        assert_eq!(parsed.credentials.as_deref(), Some(""));
        assert_eq!(parsed.positionals, ["auth", "--help"]);

        for invalid in [
            &["--timeout"][..],
            &["--session", "-5"],
            &["--help=1"],
            &["--allow-setup-tools="],
            &["--local-form"],
            &["-x"],
            &["-h=1"],
        ] {
            assert!(args(invalid).is_none(), "{invalid:?}");
        }
        assert_eq!(args(&["-"]).unwrap().positionals, ["-"]);
    }

    #[test]
    fn timeouts_read_like_zod_coerced_numbers() {
        for (text, seconds) in [
            ("  5 ", 5.0),
            ("0x10", 16.0),
            ("3.6e3", 3600.0),
            ("+7", 7.0),
        ] {
            assert_eq!(parse_timeout(Some(text)).unwrap(), seconds, "{text}");
        }

        for text in ["", "0", "1.5", "-3", "3601", "inf", "Infinity", "nan", "0x"] {
            assert!(parse_timeout(Some(text)).is_err(), "{text}");
        }
        assert_eq!(parse_timeout(None).unwrap(), 300.0);
    }
}
