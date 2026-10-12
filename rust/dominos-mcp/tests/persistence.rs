//! The persistence cases of tests/ts/money.test.ts, auth-integration.test.ts and interop.test.ts,
//! executed against this binary: an explicit, idempotent migration that leaves the journal where
//! it is, a migration resumed on an empty store, and a session the TypeScript package wrote
//! (fixtures/store) verified and logged out. The auth functions are the binary's `auth` commands
//! (support/auth.rs) and each read is a `serve`. Needs no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use support::auth::{login, logout, migrate, session_storage};
use support::provider::{cart, profile};
use support::{Fixture, Reply, TOKENS};

fn status(fixture: &Fixture) -> Result<Value, String> {
    fixture.call("auth_status", json!({}))
}

/// money.test.ts `snapshot`: every journal file's name and bytes.
fn snapshot(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            (name, fs::read(entry.path()).unwrap())
        })
        .collect()
}

fn no_session(result: Result<String, String>) {
    let message = result.unwrap_err();
    assert!(message.contains("No saved Domino’s session"), "{message}");
}

// money.test.ts:67
#[test]
fn migrate_and_logout_leave_quotes_and_checkouts_where_they_are() {
    let fixture = Fixture::setup(false, true, |_, _| None);
    let (home, path) = (&fixture.home, fixture.home.session());
    let checkouts = fixture.checkouts();
    fixture.call("quote_order", cart()).unwrap();
    let before = snapshot(&checkouts);
    assert_eq!(before.len(), 1);

    assert_eq!(migrate(home, &path), Ok("migrated"));
    assert!(!path.exists());
    assert_eq!(
        session_storage(home, &path).unwrap(),
        "Saved in an encrypted file."
    );
    assert_eq!(home.current()["refreshToken"], "synthetic-refresh");
    assert_eq!(home.containing(&TOKENS), Vec::<PathBuf>::new());
    assert_eq!(snapshot(&checkouts), before);
    assert_eq!(migrate(home, &path), Ok("already"));

    // A stale plaintext file planted after migration is never read, and migrate removes it.
    home.legacy(false, "planted");
    assert_eq!(status(&fixture).unwrap()["authenticated"], true);
    assert_eq!(home.current()["accessToken"], "synthetic-access");
    assert_eq!(migrate(home, &path), Ok("already-removed-legacy"));
    assert!(!path.exists());

    // Logout keeps the store deciding: a planted file is still ignored.
    logout(home, &path).unwrap();
    assert!(!path.exists());
    assert_eq!(snapshot(&checkouts), before);
    home.legacy(false, "planted");
    no_session(status(&fixture).map(|value| value.to_string()));
    no_session(session_storage(home, &path));
    assert_eq!(migrate(home, &path), Ok("already-removed-legacy"));
    assert!(fixture.upstream.seen().iter().all(|request| {
        !Value::Object(request.headers.clone())
            .to_string()
            .contains("planted")
    }));
    assert_eq!(
        fixture.upstream.sent(),
        [
            "GET /website/panta/pizzur",
            "GET /api/user/newuser",
            "GET /api/store",
            "POST /api/orders",
            "GET /api/user/newuser",
        ]
    );
    assert_eq!(snapshot(&checkouts), before);
    fixture.provider.check();
}

// auth-integration.test.ts:220
#[test]
fn migrate_resumes_on_an_empty_store_with_a_marker_and_refuses_without_its_key() {
    let fixture = Fixture::plain();
    let (home, path) = (&fixture.home, fixture.home.session());

    // A login after a lost key resets the store, then the code exchange fails: marker, no record.
    fs::remove_file(home.key()).unwrap();
    assert!(login(home, &path, "5550123", "123456", |_| Reply::text(400, "")).is_err());
    assert!(!home.record().exists());
    let key = fs::read(home.key()).unwrap();
    fs::remove_file(home.key()).unwrap();
    home.legacy(false, "planted");
    let marker = fs::read(home.marker()).unwrap();

    let refused = migrate(home, &path).unwrap_err();
    assert!(refused.contains("store key is missing"), "{refused}");
    assert!(!home.key().exists());
    assert_eq!(fs::read(home.marker()).unwrap(), marker);
    assert!(path.exists());

    fs::write(home.key(), &key).unwrap();
    fs::set_permissions(home.key(), fs::Permissions::from_mode(0o600)).unwrap();
    no_session(status(&fixture).map(|value| value.to_string()));
    assert_eq!(migrate(home, &path), Ok("migrated"));
    assert_eq!(home.current()["accessToken"], "planted-access");
    assert!(!path.exists());
    assert_eq!(fixture.upstream.count(), 0);
}

// interop.test.ts:23 (the TypeScript-writer half; the Rust-writer half is the golden envelope in
// rust/family-store/tests/golden.rs)
#[test]
fn a_frozen_typescript_session_is_verified_and_logged_out_by_the_binary() {
    let (home, frozen) = support::plant("signed-in");
    assert_eq!(frozen["result"], json!({"replaced": false}));
    let fixture = Fixture::in_home(home, |_, request| {
        let verified = request.path == "/api/user/newuser"
            && request.headers.get("authorization") == Some(&json!("bearer interop-access"));
        verified.then(|| Reply::json(200, &profile()))
    });
    let (home, path) = (&fixture.home, fixture.home.session());
    assert!(!path.exists());
    assert_eq!(status(&fixture).unwrap()["authenticated"], true);
    assert_eq!(home.current()["refreshToken"], "interop-refresh");
    assert_eq!(
        home.containing(&["interop-access", "interop-refresh"]),
        Vec::<PathBuf>::new()
    );
    logout(home, &path).unwrap();
    no_session(status(&fixture).map(|value| value.to_string()));
    assert_eq!(fixture.upstream.sent(), ["GET /api/user/newuser"]);
    fixture.provider.check();
}

/// auth-integration.test.ts `worst(token)`: the longest JSON a schema-valid session can have, as
/// JavaScript writes it.
fn worst(token_length: usize, username_length: usize) -> String {
    format!(
        "{{\"version\":1,\"accessToken\":\"{token}\",\"refreshToken\":\"{token}\",\"username\":\"{name}\",\"expiresAt\":-1.7976931348623157e+308}}",
        token = "\\\"".repeat(token_length),
        name = "\\u0001".repeat(username_length),
    )
}

// auth-integration.test.ts:123. The largest schema-valid session fits the store: the binary
// migrates it from the plaintext file and reads it back whole; one byte more is refused untouched.
#[test]
fn the_largest_schema_valid_session_fits_the_store() {
    // Only the plaintext file: the store has neither record nor marker yet.
    let fixture = Fixture::setup(false, true, |_, _| None);
    let (home, path) = (&fixture.home, fixture.home.session());
    let largest = worst(32768, 256);
    assert_eq!(largest.len(), support::SESSION_MAX_BYTES);

    for (name, oversized) in [
        ("token", worst(32769, 256)),
        ("username", worst(32768, 257)),
    ] {
        home.write("session.json", &oversized);
        assert_eq!(
            migrate(home, &path),
            Err("Cannot read the Domino’s session. Use a private regular file owned by you, or sign in again.".to_owned()),
            "{name}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), oversized, "{name}");
        assert!(!home.record().exists(), "{name}");
        assert!(!home.marker().exists(), "{name}");
    }
    // As saveSession wrote it: the JSON and a newline, one byte over the session bound.
    home.write("session.json", &format!("{largest}\n"));
    assert_eq!(migrate(home, &path), Ok("migrated"));
    assert!(!path.exists());
    let expected: Value = serde_json::from_str(&largest).unwrap();
    assert_eq!(home.current(), expected);
    assert_eq!(fixture.upstream.count(), 0);
}
