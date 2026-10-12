//! abler-mcp: the command line of packages/abler-mcp/src/cli.ts.

mod api;
mod auth;
mod capture;
mod error;
mod input;
mod jar;
mod js;
mod login;
mod server;

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use family_store::{Code, read_private_file};
use serde_json::json;

use crate::api::Client;
use crate::auth::{Migrated, Slot};
use crate::error::{Fail, Result};
use crate::jar::Jar;

// Browser exports can include unrelated cookies; bound both credential input paths.
const COOKIE_EXPORT_MAX_BYTES: usize = 4 * 1024 * 1024;

const HELP: &str = "abler-mcp — unofficial read-only Abler MCP server

  abler-mcp [serve]                 Start the stdio MCP server
  abler-mcp auth login              Open a temporary browser for Abler sign-in
  abler-mcp auth capture [URL]      Capture a signed-in Chrome tab (default http://127.0.0.1:9222)
  abler-mcp auth import FILE        Import browser cookie JSON; use - for stdin
  abler-mcp auth retry-candidate    Verify and use a session whose verification failed earlier
  abler-mcp auth migrate            Move a session saved by an older version out of its plaintext file
  abler-mcp auth status             Show how the session is saved and verify it against Abler
  abler-mcp auth logout             Remove the saved session and failed-import candidates
  abler-mcp --version               Print the installed version

Login options: --timeout <seconds> (default 300), --browser <path>, --keep-browser
The temporary profile is deleted after login; --keep-browser leaves live credentials in it without a debugging endpoint.
The session is saved encrypted. ABLER_SESSION_FILE names an older version's plaintext session file.
On a headless machine, run auth import there with a browser cookie export.
";

/// What the CLI prints for a failure without a reviewed message.
const FAILED: &str = "Abler MCP failed.";

const INVALID_COMMAND: Fail = Fail::Safe("Invalid command. Run abler-mcp --help for usage.");

#[derive(Default)]
struct Args {
    help: bool,
    version: bool,
    browser: Option<String>,
    timeout: Option<String>,
    keep_browser: bool,
    positionals: Vec<String>,
}

/// node:util `parseArgs` in strict mode with these options and positionals allowed.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args> {
    let invalid = Fail::Safe("Invalid command-line options. Run abler-mcp --help for usage.");
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
                "keep-browser" => &mut parsed.keep_browser,
                "browser" | "timeout" => {
                    // The next argument is the value, unless it looks like another option.
                    let value = match inline {
                        Some(value) => value,
                        None => args
                            .next()
                            .filter(|value| !(value.len() > 1 && value.starts_with('-')))
                            .ok_or(invalid)?,
                    };
                    *match name {
                        "browser" => &mut parsed.browser,
                        _ => &mut parsed.timeout,
                    } = Some(value);
                    continue;
                }
                _ => return Err(invalid),
            };

            if inline.is_some() {
                return Err(invalid);
            }
            *flag = true;
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            for short in shorts.chars() {
                match short {
                    'h' => parsed.help = true,
                    'v' => parsed.version = true,
                    _ => return Err(invalid),
                }
            }
        } else {
            parsed.positionals.push(arg);
        }
    }
    Ok(parsed)
}

/// `Number(value)` must be a safe integer of at least 1.
fn parse_timeout(value: Option<&str>) -> Result<f64> {
    let Some(value) = value else {
        return Ok(300.0);
    };
    let text = js::trim(value);
    let radix = |prefix: [&str; 2], radix| {
        prefix
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
            .map(|digits| u64::from_str_radix(digits, radix).map_or(f64::NAN, |n| n as f64))
    };
    let seconds = radix(["0x", "0X"], 16)
        .or_else(|| radix(["0o", "0O"], 8))
        .or_else(|| radix(["0b", "0B"], 2))
        .unwrap_or_else(|| match text {
            "" => 0.0,
            // Rust also reads "inf" and "nan"; JavaScript reads neither as a whole number.
            _ if text
                .bytes()
                .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) =>
            {
                text.parse().unwrap_or(f64::NAN)
            }
            _ => f64::NAN,
        });

    if seconds.fract() != 0.0 || !(1.0..=9_007_199_254_740_991.0).contains(&seconds) {
        return Err(Fail::Safe("Provide a positive whole number for --timeout."));
    }
    Ok(seconds)
}

fn read_import(argument: &str) -> Result<String> {
    let too_large = Fail::Safe("Cookie JSON input exceeds the 4 MiB limit.");

    if argument == "-" {
        let mut raw = Vec::new();
        std::io::stdin()
            .take(COOKIE_EXPORT_MAX_BYTES as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| Fail::Unknown)?;

        if raw.len() > COOKIE_EXPORT_MAX_BYTES {
            return Err(too_large);
        }
        return Ok(String::from_utf8_lossy(&raw).into_owned());
    }
    read_private_file(Path::new(argument), COOKIE_EXPORT_MAX_BYTES).map_err(|error| match error.code {
        Code::TooLarge => too_large,
        _ => Fail::Safe(
            "Cannot read the cookie JSON file. Use a regular file that you own with owner-only permissions (chmod 600 on Unix), not a symlink or hard link.",
        ),
    })
}

/// Rotations during verification land in the candidate, which is promoted only if it works.
fn verify_candidate() -> Result<()> {
    Client::new(auth::session_path()?, Slot::Candidate, None)?
        .status(true)
        .map(drop)
}

fn save_verified(jar: &Jar) -> Result<()> {
    if auth::save_verified_session(jar, verify_candidate, &auth::session_path()?, None)? {
        println!("The old Abler session store could not be read without its key and was replaced.");
    }
    println!("Abler session saved and verified in the encrypted store.");
    Ok(())
}

/// The `auth` commands. Blocks.
fn administer(args: Args) -> Result<()> {
    let positionals: Vec<&str> = args.positionals.iter().map(String::as_str).collect();
    let (action, argument) = (positionals.get(1).copied(), positionals.get(2).copied());
    // JavaScript's `!argument`: an empty argument counts as none.
    let no_argument = argument.is_none_or(str::is_empty);

    if action != Some("login")
        && (args.browser.is_some() || args.timeout.is_some() || args.keep_browser)
    {
        return Err(Fail::Safe(
            "The browser, timeout, and keep-browser options are only valid with auth login.",
        ));
    }

    let jar = match action {
        Some("login") => {
            if !no_argument {
                return Err(INVALID_COMMAND);
            }
            let timeout = parse_timeout(args.timeout.as_deref())?;
            // An option that is given, even empty, replaces ABLER_BROWSER.
            let browser = args.browser.or_else(|| {
                std::env::var_os("ABLER_BROWSER").map(|path| path.to_string_lossy().into_owned())
            });
            login::login_in_browser(browser.as_deref(), timeout, args.keep_browser)?
        }
        Some("capture") => capture::capture_cookies(
            argument
                .filter(|argument| !argument.is_empty())
                .unwrap_or("http://127.0.0.1:9222"),
        )?,
        Some("import") => {
            let argument = argument
                .filter(|argument| !argument.is_empty())
                .ok_or(Fail::Safe("Provide a cookie JSON file, or - for stdin."))?;
            let raw = read_import(argument)?;

            js::parse(raw.as_bytes())
                .and_then(|value| Jar::import(&value).ok())
                .ok_or(Fail::Safe(
                    "Import failed: provide valid browser cookie JSON containing an unexpired Abler refreshToken.",
                ))?
        }
        Some("status") if no_argument => {
            let legacy = auth::session_path()?;
            let storage = auth::session_storage(&legacy, None)?;
            let mut status =
                Client::new(legacy, Slot::Current, None)?.status(api::status_forces_refresh())?;
            status["storage"] = json!(storage);
            println!("{status}");
            return Ok(());
        }
        Some("retry-candidate") if no_argument => {
            auth::retry_candidate(verify_candidate, &auth::session_path()?, None)?;
            println!("Abler session verified and saved in the encrypted store.");
            return Ok(());
        }
        Some("migrate") if no_argument => {
            println!(
                "{}",
                match auth::migrate_session(&auth::session_path()?, None)? {
                    Migrated::Moved => {
                        "Abler session moved to the encrypted store; the plaintext files were removed."
                    }
                    Migrated::Candidate => {
                        "A failed-import candidate moved to the encrypted store; the plaintext files were removed. Run abler-mcp auth retry-candidate to verify and use it."
                    }
                    Migrated::Already => "Already migrated.",
                    Migrated::AlreadyRemovedLegacy => {
                        "Already migrated. Removed leftover plaintext session files."
                    }
                }
            );
            return Ok(());
        }
        Some("logout") if no_argument => {
            auth::logout_session(&auth::session_path()?, None)?;
            println!(
                "Local Abler session and failed-import candidates removed. This does not sign out other devices."
            );
            return Ok(());
        }
        _ => return Err(INVALID_COMMAND),
    };
    save_verified(&jar)
}

/// Serve until stdin ends, the connection breaks (a failed stdout write or an input line over
/// 10 MiB), or SIGINT or SIGTERM arrives, then cancel and wait for operations.
async fn serve() -> Result<()> {
    let client = Arc::new(Client::new(auth::session_path()?, Slot::Current, None)?);

    mcp_runtime::serve_stdio(server::Abler { client })
        .await
        .map_err(|_| Fail::Unknown)
}

async fn main_async() -> Result<ExitCode> {
    let args = parse_args(
        std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned()),
    )?;

    if args.help {
        println!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }

    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }

    // The store is checked before anything serves or touches it; help and version never do.
    if !tokio::task::spawn_blocking(auth::check_store_at_startup)
        .await
        .unwrap_or(Err(Fail::Unknown))?
    {
        return Ok(ExitCode::FAILURE);
    }
    let command = args.positionals.first().map_or("serve", String::as_str);

    if command == "serve" && args.positionals.len() <= 1 {
        return serve().await.map(|()| ExitCode::SUCCESS);
    }

    if command != "auth" || args.positionals.len() > 3 {
        return Err(INVALID_COMMAND);
    }
    tokio::task::spawn_blocking(move || administer(args))
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
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args> {
        parse_args(list.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn options_parse_like_node_parse_args() {
        let parsed = args(&[
            "auth",
            "-hv",
            "--timeout",
            "-",
            "--browser=--x",
            "--",
            "--help",
        ])
        .unwrap();
        assert!(parsed.help && parsed.version && !parsed.keep_browser);
        assert_eq!(parsed.timeout.as_deref(), Some("-"));
        assert_eq!(parsed.browser.as_deref(), Some("--x"));
        assert_eq!(parsed.positionals, ["auth", "--help"]);

        for invalid in [
            &["--timeout"][..],
            &["--timeout", "-5"],
            &["--help=1"],
            &["--nope"],
            &["-x"],
            &["-h=1"],
        ] {
            assert!(args(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(args(&["-"]).unwrap().positionals, ["-"]);
    }

    #[test]
    fn timeouts_read_like_javascript_numbers() {
        for (text, seconds) in [
            ("  5 ", 5.0),
            ("0x10", 16.0),
            ("1e3", 1000.0),
            ("+7", 7.0),
            ("5.", 5.0),
        ] {
            assert_eq!(parse_timeout(Some(text)).unwrap(), seconds, "{text}");
        }

        for text in [
            "",
            "0",
            "1.5",
            "-3",
            "inf",
            "nan",
            "1e400",
            "0x",
            "9007199254740992",
        ] {
            assert!(parse_timeout(Some(text)).is_err(), "{text}");
        }
        assert_eq!(parse_timeout(None).unwrap(), 300.0);
    }
}
