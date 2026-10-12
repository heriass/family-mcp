//! The session cases of tests/ts/auth-integration.test.ts and auth.test.ts, and the binary half of
//! the package's store-failure case, executed against this binary: renewal under concurrency, a
//! 401 read, a refresh for another account, a refresh the store cannot keep, a lost key, aliased
//! session paths, and the SMS login. The auth functions are the binary's `auth` commands
//! (support/auth.rs), each read is a `serve`, and each case checks the requests the provider saw,
//! what the store and the plaintext file hold, and that no file holds a token. Needs no Bun; needs
//! the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use support::auth::{login, logout, migrate, request_code, session_storage};
use support::{Fixture, Home, PATIENCE, Reply, TOKENS};

impl Probe for Fixture {
    fn fixture(&self) -> &Fixture {
        self
    }
}

trait Probe {
    fn fixture(&self) -> &Fixture;

    fn status(&self) -> Result<Value, String> {
        self.fixture().call("auth_status", json!({}))
    }
}

/// Another process waits for the session's lock: its lock temporary sits beside the store record
/// or the plaintext file.
fn waiter(home: &Path) -> bool {
    [home.to_path_buf(), home.join("config/dominos-mcp")]
        .iter()
        .any(|folder| {
            fs::read_dir(folder).is_ok_and(|mut entries| {
                entries.any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains(".lock-tmp.")
                })
            })
        })
}

fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;

    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn no_tokens(home: &Home) {
    assert_eq!(home.containing(&TOKENS), Vec::<PathBuf>::new());
}

/// The bodies of the token requests the provider saw.
fn token_bodies(seen: &[support::Request]) -> Vec<String> {
    seen.iter()
        .filter(|request| request.path == "/api/token")
        .map(|request| request.body.clone())
        .collect()
}

// auth-integration.test.ts:18, for a session in the store and in the plaintext file.
#[test]
fn concurrent_clients_serialize_refresh_and_persist_rotated_tokens() {
    for legacy in [false, true] {
        // The first refresh is held until the other process waits for the session's lock, so
        // the two reads overlap instead of merely racing.
        let home_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let (home_of, held) = (home_slot.clone(), Arc::new(Mutex::new(false)));
        let fixture = Arc::new(Fixture::setup(true, legacy, move |_, request| {
            if request.path == "/api/token" && !std::mem::replace(&mut *held.lock().unwrap(), true)
            {
                let home = home_of.lock().unwrap().clone().unwrap();
                until("the other process waits for the session", || waiter(&home));
            }
            None
        }));
        *home_slot.lock().unwrap() = Some(fixture.home.0.clone());
        let reads: Vec<_> = (0..2)
            .map(|_| {
                let fixture = fixture.clone();
                std::thread::spawn(move || fixture.status())
            })
            .collect();

        for read in reads {
            assert_eq!(read.join().unwrap().unwrap()["authenticated"], true);
        }
        assert_eq!(fixture.provider.state().refreshes, 1, "legacy {legacy}");
        assert_eq!(
            token_bodies(&fixture.upstream.seen()),
            ["grant_type=refresh_token&refresh_token=synthetic-refresh"]
        );
        assert_eq!(fixture.home.current()["refreshToken"], "rotated-refresh");

        if legacy {
            // Before migration the plaintext file stays authoritative, refreshes included.
            let file: Value =
                serde_json::from_str(&fs::read_to_string(fixture.home.session()).unwrap()).unwrap();
            assert_eq!(file["refreshToken"], "rotated-refresh");
            assert!(!fixture.home.record().exists());
            assert!(!fixture.home.marker().exists());
            assert_eq!(
                session_storage(&fixture.home, &fixture.home.session()).unwrap(),
                "Saved in a plaintext file. Run dominos-mcp auth migrate."
            );
        } else {
            assert!(!fixture.home.session().exists());
            no_tokens(&fixture.home);
        }
        fixture.provider.check();
    }
}

// auth-integration.test.ts:52
#[test]
fn a_read_retried_after_401_refreshes_once_inside_the_same_store_hold() {
    let rejected = Arc::new(Mutex::new(0));
    let count = rejected.clone();
    let fixture = Fixture::new(move |_, request| {
        let stale = request.headers.get("authorization") == Some(&json!("bearer synthetic-access"));

        (request.path == "/api/user/newuser" && stale).then(|| {
            *count.lock().unwrap() += 1;
            Reply::text(401, "")
        })
    });
    assert_eq!(fixture.status().unwrap()["authenticated"], true);
    assert_eq!(*rejected.lock().unwrap(), 1);
    assert_eq!(fixture.provider.state().refreshes, 1);
    assert_eq!(
        fixture.upstream.sent(),
        [
            "GET /api/user/newuser",
            "POST /api/token",
            "GET /api/user/newuser"
        ]
    );
    assert_eq!(fixture.home.current()["accessToken"], "rotated-access");
    // One store write: the seed's migration was generation 1.
    let marker = fs::read_to_string(fixture.home.marker()).unwrap();
    assert!(marker.contains("\"generation\":2}"), "{marker}");
    no_tokens(&fixture.home);
    fixture.provider.check();
}

// auth-integration.test.ts:83
#[test]
fn a_refreshed_session_for_another_account_is_rejected_and_not_saved() {
    let fixture = Fixture::setup(true, false, |_, request| {
        (request.path == "/api/token").then(|| {
            Reply::json(
                200,
                &json!({
                    "access_token": "rotated-access",
                    "refresh_token": "rotated-refresh",
                    "token_type": "bearer",
                    "username": "3545550999",
                    "expires_in": 3600
                }),
            )
        })
    });
    let files = || {
        (
            fs::read(fixture.home.record()).unwrap(),
            fs::read(fixture.home.marker()).unwrap(),
        )
    };
    let before = files();
    let refused = fixture.status().unwrap_err();
    assert!(
        refused.contains("refreshed Domino’s account differs"),
        "{refused}"
    );
    assert_eq!(files(), before);
    assert_eq!(fixture.provider.state().requests, 0);
    assert_eq!(fixture.upstream.sent(), ["POST /api/token"]);
    assert_eq!(fixture.home.current()["refreshToken"], "synthetic-refresh");
}

// auth-integration.test.ts:140
#[test]
fn a_refreshed_session_that_cannot_be_stored_poisons_the_old_record() {
    let marker_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let marker_of = marker_slot.clone();
    let fixture = Fixture::setup(true, false, move |provider, request| {
        // Domino’s consumes the refresh token, then the store refuses the pending marker.
        if request.path == "/api/token" {
            let marker = marker_of.lock().unwrap().clone().unwrap();
            fs::rename(&marker, aside(&marker)).unwrap();
            fs::create_dir_all(marker.join("blocked")).unwrap();
        }
        Some(provider.answer(request))
    });
    let marker = fixture.home.marker();
    *marker_slot.lock().unwrap() = Some(marker.clone());
    let refused = fixture.status().unwrap_err();
    assert!(
        refused.contains("did not complete, so its session is not used"),
        "{refused}"
    );
    assert_eq!(fixture.provider.state().refreshes, 1);
    assert_eq!(fixture.provider.state().requests, 1);
    assert!(!fixture.home.record().exists());

    // Once the store works again, the consumed refresh token is never offered a second time.
    fs::remove_dir_all(&marker).unwrap();
    fs::rename(aside(&marker), &marker).unwrap();
    let again = |message: Result<String, String>| {
        let message = message.unwrap_err();
        assert!(
            message.contains("did not complete")
                && message.contains("run dominos-mcp auth login again"),
            "{message}"
        );
    };
    again(fixture.status().map(|value| value.to_string()));
    again(session_storage(&fixture.home, &fixture.home.session()));
    // Login refuses too, before the SMS code is exchanged.
    let provider = fixture.provider.clone();
    again(
        login(
            &fixture.home,
            &fixture.home.session(),
            "5550123",
            "123456",
            move |request| provider.answer(request),
        )
        .map(|replaced| replaced.to_string()),
    );
    assert_eq!(fixture.provider.state().requests, 1);
    assert_eq!(fixture.upstream.sent(), ["POST /api/token"]);
    no_tokens(&fixture.home);
}

fn aside(marker: &Path) -> PathBuf {
    let mut path = marker.as_os_str().to_owned();
    path.push(".aside");
    PathBuf::from(path)
}

// auth-integration.test.ts:194
#[test]
fn a_lost_key_fails_closed_until_an_explicit_login_replaces_the_store() {
    let fixture = Fixture::plain();
    let (home, path) = (&fixture.home, fixture.home.session());
    fs::remove_file(home.key()).unwrap();
    home.legacy(false, "planted");
    let missing = |result: Result<String, String>| {
        let message = result.unwrap_err();
        assert!(message.contains("store key is missing"), "{message}");
    };
    missing(fixture.status().map(|value| value.to_string()));
    assert_eq!(fixture.provider.state().requests, 0);
    missing(session_storage(home, &path));
    missing(migrate(home, &path).map(str::to_owned));
    missing(logout(home, &path).map(|()| String::new()));
    assert!(path.exists());

    let provider = fixture.provider.clone();
    let replaced = login(home, &path, "5550123", "123456", move |request| {
        provider.answer(request)
    });
    assert_eq!(replaced, Ok(true));
    assert!(!path.exists());
    assert_eq!(fixture.status().unwrap()["authenticated"], true);
    let marker = fs::read_to_string(home.marker()).unwrap();
    assert!(marker.contains("\"generation\":1}"), "{marker}");
    fixture.provider.check();
}

// auth-integration.test.ts:249
#[test]
fn a_session_path_that_aliases_the_store_its_key_or_the_checkouts_is_refused() {
    let fixture = Fixture::plain();
    let home = &fixture.home;
    let (record, key) = (home.record(), home.key());
    let store = home.path("config/dominos-mcp");
    let aliased = home.path("aliased");
    fs::create_dir(&aliased).unwrap();
    symlink(&store, aliased.join("session.checkouts")).unwrap();
    symlink(&store, home.path("linked")).unwrap();
    // Checkouts planted under a record temporary's exact name: refused, and never swept.
    let temporary = PathBuf::from(format!(
        "{}.{}.tmp",
        record.display(),
        "0b5e7c3a-1d2f-4a6b-8c9d-0e1f2a3b4c5d"
    ));
    let planted = temporary.join("session.json.checkouts/quote.json");
    fs::create_dir_all(planted.parent().unwrap()).unwrap();
    fs::write(&planted, "kept").unwrap();
    fs::File::open(&temporary)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_accessed(SystemTime::UNIX_EPOCH)
                .set_modified(SystemTime::UNIX_EPOCH),
        )
        .unwrap();
    symlink(&temporary, aliased.join("temporary.checkouts")).unwrap();
    let files = || {
        (
            fs::read(&record).unwrap(),
            fs::read(home.marker()).unwrap(),
            fs::read(&key).unwrap(),
        )
    };
    let before = files();
    let mut marker = record.as_os_str().to_owned();
    marker.push(".marker");
    let paths: Vec<PathBuf> = vec![
        record.clone(),
        PathBuf::from(marker),
        PathBuf::from(format!("{}.lock", record.display())),
        PathBuf::from(format!("{}.tmp", record.display())),
        store.join("session"),
        home.path("linked/session.enc"),
        store.join("nested/../session.enc"),
        key.clone(),
        store.clone(),
        aliased.join("session"),
        temporary.join("session.json"),
        aliased.join("temporary"),
        key.parent()
            .unwrap()
            .join("dominos-mcp.default.key.lock/session.json"),
    ];

    for path in &paths {
        let provider = fixture.provider.clone();
        let outcomes = [
            login(home, path, "5550123", "123456", move |request| {
                provider.answer(request)
            })
            .map(|_| String::new()),
            migrate(home, path).map(str::to_owned),
            logout(home, path).map(|()| String::new()),
        ];

        for outcome in outcomes {
            let message = outcome.unwrap_err();
            assert!(
                message.contains("DOMINOS_SESSION_FILE overlaps"),
                "{}: {message}",
                path.display()
            );
        }
        assert_eq!(files(), before, "{}", path.display());
    }
    assert_eq!(fixture.provider.state().requests, 0);
    assert_eq!(fixture.status().unwrap()["authenticated"], true);
    assert_eq!(fs::read_to_string(&planted).unwrap(), "kept");
    fixture.provider.check();
}

// auth.test.ts:10
#[test]
fn sms_login_uses_the_observed_endpoints_and_stores_verified_credentials_encrypted() {
    let home = Home::new("sms");
    let path = home.session();
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let request = {
        let (calls, failures) = (calls.clone(), failures.clone());
        move |request: &support::Request| {
            let (route, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
            calls.lock().unwrap().push(route.to_owned());

            if route.ends_with("/sendPin") {
                if query != "phoneNumber=3545550123" {
                    failures.lock().unwrap().push(query.to_owned());
                }
                return Reply::text(200, "");
            }
            if route.ends_with("/token") {
                if request.body
                    != "grant_type=password&username=3545550123&password=123456&authentication_type=sms"
                {
                    failures.lock().unwrap().push(request.body.clone());
                }
                return Reply::json(
                    200,
                    &json!({
                        "access_token": "synthetic-access",
                        "refresh_token": "synthetic-refresh",
                        "token_type": "bearer",
                        "expires_in": 3600,
                        "username": "3545550123"
                    }),
                );
            }
            if request.headers.get("authorization") != Some(&json!("bearer synthetic-access")) {
                failures
                    .lock()
                    .unwrap()
                    .push(format!("{:?}", request.headers));
            }
            Reply::json(200, &json!({"id": 1}))
        }
    };
    let first = request.clone();
    request_code(&home, "+354 555-0123", move |seen| first(seen)).unwrap();
    assert_eq!(
        login(&home, &path, "5550123", "123456", request.clone()),
        Ok(false)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        ["/api/login/sendPin", "/api/token", "/api/user/newuser"]
    );
    assert!(!path.exists());

    for file in [home.record(), home.marker(), home.key()] {
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", file.display());
    }
    assert_eq!(home.current()["refreshToken"], "synthetic-refresh");
    assert_eq!(
        session_storage(&home, &path).unwrap(),
        "Saved in an encrypted file."
    );
    assert_eq!(
        home.containing(&["synthetic-access", "synthetic-refresh"]),
        Vec::<PathBuf>::new()
    );
    // phoneNumber refuses what is not a phone number, before any request.
    let none = Arc::new(Mutex::new(0));
    let count = none.clone();
    let refused = login(
        &home,
        &path,
        "https://example.invalid",
        "123456",
        move |_| {
            *count.lock().unwrap() += 1;
            Reply::empty(599)
        },
    );
    assert!(refused.is_err(), "{refused:?}");
    let short = login(&home, &path, "5550123", "12345", request).unwrap_err();
    assert!(short.contains("six digits"), "{short}");
    assert_eq!(*none.lock().unwrap(), 0);
    assert_eq!(*failures.lock().unwrap(), Vec::<String>::new());
}

/// The binary half of packages/dominos-mcp/test/integration.test.ts:685. The package injected
/// failing key providers; here each failure is real: a key file with the wrong mode, a missing key,
/// the wrong key, and a record without its marker. Each refuses `status`, `migrate` and `logout`
/// with its fixed text, changes no file, never falls back to the planted plaintext session, sends
/// nothing, and names neither the home nor a token.
#[test]
fn store_failures_change_no_file_and_never_fall_back() {
    let fixture = Fixture::setup(false, true, |_, _| None);
    let (home, path) = (&fixture.home, fixture.home.session());
    assert_eq!(migrate(home, &path), Ok("migrated"));
    home.legacy(false, "planted");
    let key = fs::read(home.key()).unwrap();
    let files = || {
        (
            fs::read(home.record()).unwrap(),
            fs::read(home.marker()).ok(),
            fs::read(&path).unwrap(),
        )
    };
    let missing_text =
        "The Domino’s store key is missing. Run dominos-mcp auth login to sign in again.";
    let damaged_text = "Cannot use the Domino’s session store. Its files or key are damaged, unsafe, or not readable.";
    let before = files();
    // An unsafe key file is found by the startup check, so every command refuses before it runs,
    // with the store's own text and the path, and exits 1 (startup.rs owns the full texts).
    fs::set_permissions(home.key(), fs::Permissions::from_mode(0o644)).unwrap();
    let session = vec![(
        "DOMINOS_SESSION_FILE".to_owned(),
        path.display().to_string(),
    )];

    for args in [&["serve"][..], &["auth", "migrate"], &["auth", "logout"]] {
        let run = support::cli(home, &fixture.upstream.origin, &session, args, None);
        assert_eq!(run.code, Some(1), "{args:?}: {}", run.stderr);
        assert!(
            run.stderr.starts_with("dominos-mcp: cannot start. "),
            "{args:?}: {}",
            run.stderr
        );
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(!run.stderr.contains("planted"), "{}", run.stderr);
        assert_eq!(files(), before, "{args:?}");
    }
    fs::set_permissions(home.key(), fs::Permissions::from_mode(0o600)).unwrap();
    let cases: [(&str, &dyn Fn(), &str); 2] = [
        (
            "key missing",
            &|| fs::remove_file(home.key()).unwrap(),
            missing_text,
        ),
        (
            "another key",
            &|| fs::write(home.key(), [8u8; 32]).unwrap(),
            damaged_text,
        ),
    ];

    for (name, break_store, text) in cases {
        break_store();
        let outcomes = [
            fixture.status().map(|value| value.to_string()),
            migrate(home, &path).map(str::to_owned),
            logout(home, &path).map(|()| String::new()),
        ];

        for outcome in outcomes {
            assert_eq!(outcome, Err(text.to_owned()), "{name}");
        }
        assert_eq!(files(), before, "{name}");
        // Restore the key as it was.
        fs::write(home.key(), &key).unwrap();
        fs::set_permissions(home.key(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    // A record without its marker is refused without a fallback.
    fs::remove_file(home.marker()).unwrap();
    let before = files();
    let uncertain = fixture.status().unwrap_err();
    assert!(
        uncertain.starts_with("The last write to the Domino’s session store did not complete, so its session is not used."),
        "{uncertain}"
    );
    assert_eq!(files(), before);
    assert_eq!(fixture.upstream.count(), 0);

    for message in [
        uncertain,
        migrate(home, &path).unwrap_err(),
        logout(home, &path).unwrap_err(),
    ] {
        assert!(
            !message.contains(&home.0.display().to_string()),
            "{message}"
        );
        assert!(!message.contains("planted"), "{message}");

        for token in TOKENS {
            assert!(!message.contains(token), "{message}");
        }
    }
    assert_eq!(files(), before);
}
