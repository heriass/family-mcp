//! kronan-mcp: the command line of packages/kronan-mcp/src/cli.ts.

mod api;
mod attempts;
mod auth;
mod error;
mod input;
mod js;
mod server;
mod shapes;
mod terminal;

use std::future::Future;
use std::process::ExitCode;
use std::sync::Arc;

use crate::api::{Client, Token};
use serde_json::Value;

use crate::error::{Fail, Result};

const HELP: &str = "kronan-mcp — unofficial Krónan MCP server (products, shopping note, basket, and confirmed orders)

  kronan-mcp [serve]                Start the stdio MCP server
  kronan-mcp auth set [FILE]        Save an access token read from FILE, or from stdin (hidden prompt on a terminal)
  kronan-mcp auth migrate           Move a token saved by an older version out of its plaintext file
  kronan-mcp auth status            Show how the token is saved and verify it against Krónan
  kronan-mcp auth logout            Forget the saved token on this computer
  kronan-mcp orders clear-attempts  Show recorded order attempts; clear them after a y/N confirmation
  kronan-mcp --version              Print the installed version

Create the access token in Krónan's settings (User or Customer group page; Auðkenni login required).
Never pass the token as a command-line argument. The token is saved encrypted, with the key in a
separate private file: ~/Library/Application Support/family-mcp on macOS; ~/.config/kronan-mcp with
the key in ~/.local/share/family-mcp/keys on Linux.
Order calls are recorded beside KRONAN_TOKEN_FILE (the plaintext token file of older versions); an
unresolved record blocks further order calls for that checkout.
Clear records only after checking your Krónan orders, never to get around an unknown outcome.
";

/// What the CLI prints for a failure without a reviewed message.
const FAILED: &str = "Krónan MCP failed.";

/// An unknown command: the CLI throws `new Error(help)`, so the help goes to stderr.
const INVALID_COMMAND: Fail = Fail::Safe(HELP);

#[derive(Debug, Default, PartialEq)]
struct Args {
    help: bool,
    version: bool,
    positionals: Vec<String>,
}

/// Bun's `parseArgs` diagnostic for an option it does not know. The CLI does not catch it, so it
/// reaches stderr as it is; it names only what was typed.
fn unknown_option(name: &str) -> String {
    format!(
        "Unknown option '{name}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"{name}\""
    )
}

/// `parseArgs({ allowPositionals: true, options: { help: -h, version: -v } })` as Bun 1.4.2
/// runs it: strict, both options boolean, `--` ends the options. `Err` is its diagnostic.
fn parse_args(args: impl IntoIterator<Item = String>) -> std::result::Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        // Lengths in UTF-16 units, as JavaScript counts them.
        let units = arg.encode_utf16().count();

        if arg == "--" {
            parsed.positionals.extend(args.by_ref());
        } else if let Some(long) = arg.strip_prefix("--") {
            // `--name=value` needs a name: the `=` must come after the third character.
            let (name, inline) = match long.split_once('=') {
                Some((name, _)) if !name.is_empty() => (name, true),
                _ => (long, false),
            };
            let (flag, both) = match name {
                "help" => (&mut parsed.help, "-h, --help"),
                "version" => (&mut parsed.version, "-v, --version"),
                _ => return Err(unknown_option(&format!("--{name}"))),
            };

            if inline {
                return Err(format!("Option '{both}' does not take an argument"));
            }
            *flag = true;
        } else if arg.starts_with('-') && units == 2 {
            match &arg[1..] {
                "h" => parsed.help = true,
                "v" => parsed.version = true,
                _ => return Err(unknown_option(&arg)),
            }
        } else if arg.starts_with('-') && units > 2 {
            // A short option group names each unknown letter without its dash.
            for short in arg[1..].chars() {
                match short {
                    'h' => parsed.help = true,
                    'v' => parsed.version = true,
                    _ => return Err(unknown_option(&short.to_string())),
                }
            }
        } else {
            parsed.positionals.push(arg);
        }
    }
    Ok(parsed)
}

/// Serve until stdin ends, the connection breaks (a failed stdout write or an input line over
/// 10 MiB), or SIGINT or SIGTERM arrives, then cancel and wait for operations.
async fn serve() -> Result<()> {
    let client = Arc::new(Client::new(Token::Saved)?);

    mcp_runtime::serve_stdio(server::Kronan { client })
        .await
        .map_err(|_| Fail::Unknown)
}

/// Run blocking store or terminal work off the runtime thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(Fail::Unknown))
}

/// Krónan's `/me/` with `token`, the same authenticated read as `auth_status`.
async fn verify(token: String) -> Result<Value> {
    let client = Arc::new(Client::new(Token::Given(token))?);
    let status = client.run(Client::status).await;
    client.close().await;
    status
}

async fn auth_set(source: Option<String>) -> Result<()> {
    let given = source.clone();
    let token =
        blocking(move || auth::normalize_token(&terminal::read_token_input(given.as_deref())?))
            .await?;
    // Verify before saving, so a saved token is always one that Krónan accepted.
    verify(token.clone()).await?;

    if blocking(move || auth::save_token(&token)).await? {
        println!("The old Krónan token store could not be read without its key and was replaced.");
    }
    println!("Krónan access token verified and saved encrypted.");

    if source.is_some_and(|source| source != "-") {
        println!("Remove the source file now; it holds the same credential.");
    }
    Ok(())
}

/// Run `work` unless SIGTERM or SIGINT arrives first; then give the terminal back the mode the
/// hidden prompt changed and exit. A handled signal never takes its default action again, so
/// this covers all of `auth set`, not only the prompt.
async fn until_signalled(
    work: impl Future<Output = Result<()>>,
    saved: &terminal::Saved,
) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    let mut terminate = signal(SignalKind::terminate()).map_err(|_| Fail::Unknown)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| Fail::Unknown)?;

    tokio::select! {
        outcome = work => outcome,
        _ = terminate.recv() => saved.restore_and_exit(SIGTERM),
        _ = interrupt.recv() => saved.restore_and_exit(SIGINT),
    }
}

async fn auth_status() -> Result<()> {
    let saved = blocking(auth::load_saved_token).await?;
    println!("{}", saved.storage);
    println!("{}", verify(saved.token).await?);
    Ok(())
}

/// A human-only escape hatch: no MCP tool can clear the order-attempt record.
fn clear_order_attempts() -> Result<()> {
    let path = attempts::attempts_path()?;
    let shown = attempts::list_attempts(&path);
    let path_text = path.display();

    match &shown {
        Some(listed) if listed.is_empty() => {
            println!("No recorded order attempts in {path_text}.");
            return Ok(());
        }
        None => println!("The order-attempt record {path_text} is unreadable or unsafe."),
        Some(listed) => {
            println!("Recorded order attempts in {path_text}:");

            for attempt in listed {
                let text = |key: &str| attempt[key].as_str().unwrap_or_default().to_owned();
                println!(
                    "  {}  {}  {}  checkout {}  total {} ISK  order {}",
                    text("createdAt"),
                    text("tool"),
                    text("state"),
                    text("checkoutToken"),
                    attempt["total"],
                    attempt["orderToken"].as_str().unwrap_or("-"),
                );
            }
        }
    }
    println!(
        "Clear these only after you checked your Krónan orders. A submitting or unknown attempt may have placed an order."
    );

    if !terminal::read_answer("Clear the recorded order attempts? [y/N] ").eq_ignore_ascii_case("y")
    {
        println!("Kept the recorded order attempts.");
        return Ok(());
    }

    if !attempts::clear_attempts(&path, shown.as_deref())? {
        return Err(Fail::Safe(
            "The order-attempt record changed while you answered. Run the command again.",
        ));
    }
    println!("Cleared the recorded order attempts.");
    Ok(())
}

async fn main_async(args: Args) -> Result<ExitCode> {
    if args.help {
        println!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }

    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }

    // The store is checked before anything serves or touches it; help and version never do.
    if !blocking(auth::check_store_at_startup).await? {
        return Ok(ExitCode::FAILURE);
    }
    let positionals: Vec<&str> = args.positionals.iter().map(String::as_str).collect();
    let command = positionals.first().copied().unwrap_or("serve");

    if command == "serve" && positionals.len() <= 1 {
        return serve().await.map(|()| ExitCode::SUCCESS);
    }

    if command == "orders"
        && positionals.get(1) == Some(&"clear-attempts")
        && positionals.len() == 2
    {
        return blocking(clear_order_attempts)
            .await
            .map(|()| ExitCode::SUCCESS);
    }

    if command != "auth" || positionals.len() > 3 {
        return Err(INVALID_COMMAND);
    }

    match (positionals.get(1).copied(), positionals.get(2)) {
        (Some("set"), source) => {
            let source = source.map(|source| (*source).to_owned());

            match terminal::prompt_mode(source.as_deref()) {
                Some(saved) => until_signalled(auth_set(source), &saved).await?,
                None => auth_set(source).await?,
            }
        }
        (Some("migrate"), None) => println!(
            "{}",
            match blocking(auth::migrate_token).await? {
                auth::Migrated::Moved => {
                    "Krónan access token moved to the encrypted store; the plaintext file was removed."
                }
                auth::Migrated::Already => "Already migrated.",
                auth::Migrated::AlreadyRemovedLegacy => {
                    "Already migrated. Removed a leftover plaintext token file."
                }
            }
        ),
        (Some("status"), None) => auth_status().await?,
        (Some("logout"), None) => {
            blocking(auth::logout_token).await?;
            println!(
                "Saved Krónan access token removed from this computer. The token itself stays valid until revoked in Krónan settings."
            );
        }
        _ => return Err(INVALID_COMMAND),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args = match parse_args(
        std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned()),
    ) {
        Ok(args) => args,
        Err(diagnostic) => {
            eprintln!("{diagnostic}");
            return ExitCode::FAILURE;
        }
    };
    // current_thread: one stdio client and I/O-bound work. Store work runs on blocking threads,
    // which wait for requests while this thread drives them.
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Fail::Unknown)
        .and_then(|runtime| {
            let outcome = runtime.block_on(main_async(args));
            // Every operation has ended; only the stdin reader may still block, so do not wait.
            runtime.shutdown_background();
            outcome
        });

    match outcome {
        Ok(code) => code,
        Err(fail) => {
            // Only reviewed diagnostics cross the terminal boundary; library messages may hold secrets.
            eprintln!("{}", mcp_runtime::cli_text(fail, FAILED));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> std::result::Result<Args, String> {
        parse_args(list.iter().map(|arg| (*arg).to_owned()))
    }

    fn text(list: &[&str]) -> String {
        args(list).unwrap_err()
    }

    #[test]
    fn options_parse_like_bun_parse_args() {
        let parsed = args(&["auth", "-hv", "-", "--", "--help", "-x"]).unwrap();
        assert!(parsed.help && parsed.version);
        assert_eq!(parsed.positionals, ["auth", "-", "--help", "-x"]);

        let unknown = |name: &str| {
            format!(
                "Unknown option '{name}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"{name}\""
            )
        };
        // Texts measured with Bun 1.4.2 running packages/kronan-mcp/src/cli.ts.
        for (list, name) in [
            (&["--nope"][..], "--nope"),
            (&["--nope=1"], "--nope"),
            (&["--="], "--="),
            (&["---x"], "---x"),
            (&["-x"], "-x"),
            (&["-é"], "-é"),
            (&["-vx"], "x"),
            (&["-h=1"], "="),
            (&["--no-help"], "--no-help"),
            (&["-h", "--HELP"], "--HELP"),
        ] {
            assert_eq!(text(list), unknown(name), "{list:?}");
        }
        assert_eq!(
            text(&["--help=1"]),
            "Option '-h, --help' does not take an argument"
        );
        assert_eq!(
            text(&["--version="]),
            "Option '-v, --version' does not take an argument"
        );
    }
}
