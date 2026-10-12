//! The drop-in auth functions as the CLI commands tests/ts/rust-kronan.ts ran for them: `loadToken`
//! and `loadSavedToken` are `auth status` (its first line, and the token it sends to `/me/`),
//! `saveToken` is `auth set -`, `migrateToken` is `auth migrate`, `logoutToken` is `auth logout`,
//! and `normalizeToken` is `auth set -` against a Krónan that rejects every token. A drop-in key
//! file is a data home of its own.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{Home, Reply, Upstream, cli};

pub const TOKEN: &str = "synthetic-token-0123456789";

/// A closed port: a command that must stay offline fails if it tries to reach Krónan.
pub const NOWHERE: &str = "http://127.0.0.1:9";

pub const NO_TOKEN: &str = "No saved Krónan access token";

pub const PLAINTEXT: &str = "Saved in a plaintext file. Run kronan-mcp auth migrate.";

pub const ENCRYPTED: &str = "Saved in an encrypted file.";

pub const UNREADABLE: &str = "Cannot read the Krónan token file";

pub const INVALID_TOKEN: &str = "Invalid Krónan access token";

pub const STORE_ERROR: &str = "Cannot use the Krónan token store";

pub const OVERLAPS: &str = "KRONAN_TOKEN_FILE overlaps the encrypted Krónan token store";

/// `auth migrate`'s three answers, as rust-kronan.ts `migrated` maps them.
pub const MIGRATED: &str =
    "Krónan access token moved to the encrypted store; the plaintext file was removed.\n";

pub const ALREADY: &str = "Already migrated.\n";

pub const ALREADY_REMOVED_LEGACY: &str =
    "Already migrated. Removed a leftover plaintext token file.\n";

/// The extra environment of one drop-in case: the legacy token file and, for a drop-in `KeyFile`,
/// its data home.
#[derive(Clone, Default)]
pub struct Env(pub Vec<(String, String)>);

impl Env {
    pub fn legacy(path: &Path) -> Self {
        Self(vec![(
            "KRONAN_TOKEN_FILE".into(),
            path.display().to_string(),
        )])
    }

    pub fn keys(&self, data_home: &Path) -> Self {
        let mut extra = self.0.clone();
        extra.push(("XDG_DATA_HOME".into(), data_home.display().to_string()));
        Self(extra)
    }
}

/// One CLI command: its standard output, or the trimmed diagnostic a failure prints, as
/// rust-kronan.ts `cli` threw it.
pub fn run(
    home: &Home,
    env: &Env,
    upstream: Option<&Upstream>,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<String, String> {
    let origin = upstream.map_or(NOWHERE, |upstream| upstream.origin.as_str());
    let run = cli(home, origin, &env.0, args, stdin);

    match run.code {
        Some(0) => Ok(run.stdout),
        _ => Err(run.stderr.trim().to_owned()),
    }
}

/// Krónan's `/me/` as the CLI verifies a token.
pub fn account() -> Upstream {
    Upstream::new(|_| Reply::json(200, &json!({"type": "user", "name": "Synthetic account"})))
}

/// The tokens `upstream` was sent, in order.
pub fn tokens(upstream: &Upstream) -> Vec<String> {
    upstream
        .seen()
        .iter()
        .map(|request| {
            let header = request.headers["authorization"].as_str().unwrap();
            header.strip_prefix("AccessToken ").unwrap().to_owned()
        })
        .collect()
}

/// rust-kronan.ts `loadSavedToken`: `auth status`'s first line and the token it sent.
pub fn load_saved(home: &Home, env: &Env) -> Result<(String, String), String> {
    let upstream = account();
    let out = run(home, env, Some(&upstream), &["auth", "status"], None);

    if out.is_err() {
        // A refused token is refused before any request.
        assert_eq!(upstream.count(), 0, "{out:?}");
    }
    let storage = out?.split('\n').next().unwrap().to_owned();
    let sent = tokens(&upstream);
    assert_eq!(sent.len(), 1, "auth status sends one request");
    Ok((sent[0].clone(), storage))
}

pub fn load(home: &Home, env: &Env) -> Result<String, String> {
    load_saved(home, env).map(|(token, _)| token)
}

/// rust-kronan.ts `saveToken`: true if a store whose key was lost was replaced.
pub fn save(home: &Home, env: &Env, token: &str) -> Result<bool, String> {
    let out = run(
        home,
        env,
        Some(&account()),
        &["auth", "set", "-"],
        Some(token),
    )?;
    Ok(out.contains("could not be read without its key and was replaced"))
}

pub fn migrate(home: &Home, env: &Env) -> Result<String, String> {
    let out = run(home, env, None, &["auth", "migrate"], None)?;
    assert!(
        [MIGRATED, ALREADY, ALREADY_REMOVED_LEGACY].contains(&out.as_str()),
        "unexpected migrate output: {out}"
    );
    Ok(out)
}

pub fn logout(home: &Home, env: &Env) -> Result<(), String> {
    run(home, env, None, &["auth", "logout"], None).map(|_| ())
}

/// rust-kronan.ts `normalizeToken`: `auth set -` against a Krónan that rejects every token, so the
/// token the binary normalized is the one it sends and nothing is saved.
pub fn normalize(home: &Home, raw: &str) -> Result<String, String> {
    let rejecting = Upstream::new(|_| Reply::empty(401));
    let out = run(
        home,
        &Env::default(),
        Some(&rejecting),
        &["auth", "set", "-"],
        Some(raw),
    );
    assert!(out.is_err(), "a rejected token was saved");

    match tokens(&rejecting).as_slice() {
        [token] => Ok(token.clone()),
        [] => Err(out.unwrap_err()),
        more => panic!("one verification only: {more:?}"),
    }
}

/// tests/ts writeLegacyToken: `{version: 1, token}` and a newline, owner-only.
pub fn write_legacy(home: &Home, relative: &str, token: &str) {
    home.write(
        relative,
        &format!("{}\n", json!({"version": 1, "token": token})),
        None,
    );
}

pub fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// The default record, beside the legacy file the binary reads by default.
pub const RECORD: &str = ".config/kronan-mcp/session.enc";

pub const MARKER: &str = ".config/kronan-mcp/session.enc.marker";

pub const KEY: &str = "family-mcp/keys/kronan-mcp.default.key";

/// A drop-in `KeyFile`: a data home of its own, holding `key` when given.
pub fn key_file(home: &Home, name: &str, key: Option<&[u8]>) -> PathBuf {
    let data = home.path(name);
    DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&data)
        .unwrap();

    if let Some(key) = key {
        let path = data.join(KEY);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(&path, key).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    data
}

pub fn bytes(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok()
}
