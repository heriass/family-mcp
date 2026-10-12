//! tests/ts/rust-dominos.ts's CLI-backed auth functions: each is the binary's `auth` command with
//! `DOMINOS_SESSION_FILE` set to the given path, and an error is its stderr without the prompts.
use std::path::Path;

use super::{Home, Reply, Request, Run, Upstream, cli};

fn run(
    home: &Home,
    path: &Path,
    args: &[&str],
    stdin: Option<&str>,
    upstream: Option<&Upstream>,
) -> Run {
    let session = vec![(
        "DOMINOS_SESSION_FILE".to_owned(),
        path.display().to_string(),
    )];
    let origin = upstream.map_or("http://127.0.0.1:9", |upstream| upstream.origin.as_str());
    cli(home, origin, &session, args, stdin)
}

fn outcome(run: Run) -> Result<String, String> {
    match run.code {
        Some(0) => Ok(run.stdout),
        _ => Err(run
            .stderr
            .strip_prefix("Icelandic phone number (input hidden): ")
            .unwrap_or(&run.stderr)
            .trim_start_matches("\nSMS sent. Six-digit code (input hidden): ")
            .trim()
            .to_owned()),
    }
}

/// `login(phone, pin, path, request)`: the CLI asks for a code first; the drop-in answers that
/// request itself, so `request` sees only the exchange and the account read. True when an earlier
/// session was replaced.
pub fn login(
    home: &Home,
    path: &Path,
    phone: &str,
    pin: &str,
    request: impl Fn(&Request) -> Reply + Send + Sync + 'static,
) -> Result<bool, String> {
    let upstream = Upstream::new(move |seen| match seen.path.contains("/sendPin") {
        true => Reply::text(200, ""),
        false => request(seen),
    });
    let stdin = format!("{phone}\n{pin}\n");
    outcome(run(
        home,
        path,
        &["auth", "login"],
        Some(&stdin),
        Some(&upstream),
    ))
    .map(|stdout| stdout.contains("was replaced."))
}

/// `requestCode(phone, request)`: a login that ends after the code request; its own session path.
pub fn request_code(
    home: &Home,
    phone: &str,
    request: impl Fn(&Request) -> Reply + Send + Sync + 'static,
) -> Result<(), String> {
    let upstream = Upstream::new(request);
    let path = home.path("config/dominos-code.json");
    match outcome(run(
        home,
        &path,
        &["auth", "login"],
        Some(&format!("{phone}\n")),
        Some(&upstream),
    )) {
        Err(message) if message == "Sign-in cancelled." => Ok(()),
        Err(message) => Err(message),
        Ok(_) => Ok(()),
    }
}

/// `migrate(path)`: `migrated`, `already-removed-legacy` or `already`.
pub fn migrate(home: &Home, path: &Path) -> Result<&'static str, String> {
    let stdout = outcome(run(home, path, &["auth", "migrate"], None, None))?;

    if stdout.starts_with("Domino’s session moved") {
        Ok("migrated")
    } else if stdout.starts_with("Already migrated. Removed") {
        Ok("already-removed-legacy")
    } else {
        assert_eq!(stdout, "Already migrated.\n");
        Ok("already")
    }
}

pub fn logout(home: &Home, path: &Path) -> Result<(), String> {
    outcome(run(home, path, &["auth", "logout"], None, None)).map(drop)
}

/// `sessionStorage(path)`: the first line of `auth status`; the drop-in answers its verification.
pub fn session_storage(home: &Home, path: &Path) -> Result<String, String> {
    let upstream = Upstream::new(|_| {
        Reply::json(
            200,
            &serde_json::json!({"id": 1, "name": null, "phoneNumber": "", "email": null}),
        )
    });
    outcome(run(home, path, &["auth", "status"], None, Some(&upstream)))
        .map(|stdout| stdout.lines().next().unwrap_or_default().to_owned())
}
