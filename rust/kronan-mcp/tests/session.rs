//! The token-store cases of tests/ts/integration.test.ts, executed against this binary without Bun:
//! each drop-in auth function is the CLI command it ran (support/auth.rs). Needs the `test-origin`
//! feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use family_store::with_secret_store;
use serde_json::json;
use support::auth::*;
use support::{Home, Mcp, PATIENCE, cli};

#[test]
fn legacy_token_files_are_read_only_when_private_and_well_formed() {
    let home = Home::new("legacy-files");
    let file = |name: &str| home.path(&format!("tokens/{name}"));
    write_legacy(&home, "tokens/session.json", TOKEN);
    assert_eq!(
        load(&home, &Env::legacy(&file("session.json"))),
        Ok(TOKEN.to_owned())
    );

    home.write(
        "tokens/loose.json",
        &json!({"version": 1, "token": TOKEN}).to_string(),
        Some(0o644),
    );
    let loose = load(&home, &Env::legacy(&file("loose.json"))).unwrap_err();
    assert!(loose.contains(UNREADABLE), "{loose}");
    symlink(file("session.json"), file("link.json")).unwrap();
    let linked = load(&home, &Env::legacy(&file("link.json"))).unwrap_err();
    assert!(linked.contains(UNREADABLE), "{linked}");

    home.write(
        "tokens/invalid.json",
        "{\"version\":2,\"token\":\"synthetic-token-0123456789\"}",
        None,
    );
    let invalid = load(&home, &Env::legacy(&file("invalid.json"))).unwrap_err();
    assert!(invalid.contains("Invalid Krónan token file"), "{invalid}");
    let missing = load(&home, &Env::legacy(&file("missing.json"))).unwrap_err();
    assert!(missing.contains(NO_TOKEN), "{missing}");

    // The binary normalizes what `auth set` reads; nothing is saved by a rejected token.
    let empty = Home::new("normalize");
    assert_eq!(
        normalize(&empty, &format!("  {TOKEN} \n")),
        Ok(TOKEN.to_owned())
    );

    for raw in [
        "",
        "short",
        "synthetic\r\ntoken",
        "synthetic-tökén",
        &"x".repeat(4097),
    ] {
        let refused = normalize(&empty, raw).unwrap_err();
        assert!(refused.contains(INVALID_TOKEN), "{raw:?}: {refused}");
    }
    assert!(absent(&empty.path(".config")), "nothing was saved");

    assert_eq!(
        logout(&home, &Env::legacy(&file("already-missing.json"))),
        Ok(())
    );
}

#[test]
fn migrate_moves_the_legacy_token_once_and_then_only_the_store_is_read() {
    let home = Home::new("migrate");
    let legacy = home.path("tokens/session.json");
    let env = Env::legacy(&legacy);
    let attempts = home.path("tokens/session.json.order-attempts.json");
    let other = "synthetic-token-planted-later";

    let none = load(&home, &env).unwrap_err();
    assert!(
        none.contains("No saved Krónan access token. Run kronan-mcp auth set"),
        "{none}"
    );
    let unmigrated = migrate(&home, &env).unwrap_err();
    assert!(unmigrated.contains(NO_TOKEN), "{unmigrated}");

    write_legacy(&home, "tokens/session.json", TOKEN);
    home.write(
        "tokens/session.json.order-attempts.json",
        "journal bytes stay as they are",
        None,
    );
    assert_eq!(attempts_path(&home, &env), attempts);

    assert_eq!(
        load_saved(&home, &env),
        Ok((TOKEN.to_owned(), PLAINTEXT.to_owned()))
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(MIGRATED));
    assert!(absent(&legacy));
    assert!(!String::from_utf8_lossy(&fs::read(home.path(RECORD)).unwrap()).contains(TOKEN));
    assert!(
        !fs::read_to_string(home.path(MARKER))
            .unwrap()
            .contains(TOKEN)
    );
    assert_eq!(
        load_saved(&home, &env),
        Ok((TOKEN.to_owned(), ENCRYPTED.to_owned()))
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY));

    // A legacy file that reappears after migration is never read; migrate removes it.
    write_legacy(&home, "tokens/session.json", other);
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY_REMOVED_LEGACY));
    assert!(absent(&legacy));

    write_legacy(&home, "tokens/session.json", other);
    assert_eq!(logout(&home, &env), Ok(()));
    assert!(absent(&legacy));
    assert!(home.path(RECORD).is_file());
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));
    write_legacy(&home, "tokens/session.json", other);
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY_REMOVED_LEGACY));

    assert_eq!(save(&home, &env, TOKEN), Ok(false));
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));

    // The order-attempt journal keeps its path and bytes through all of it.
    assert_eq!(attempts_path(&home, &env), attempts);
    assert_eq!(
        fs::read_to_string(&attempts).unwrap(),
        "journal bytes stay as they are"
    );
}

/// rust-kronan.ts `attemptsPath`: the record `orders clear-attempts` names; end of input keeps it.
fn attempts_path(home: &Home, env: &Env) -> PathBuf {
    let out = cli(home, NOWHERE, &env.0, &["orders", "clear-attempts"], None).stdout;
    let first = out.split('\n').next().unwrap();
    let named = [
        ("No recorded order attempts in ", "."),
        ("Recorded order attempts in ", ":"),
        ("The order-attempt record ", " is unreadable or unsafe."),
    ]
    .iter()
    .find_map(|(head, tail)| first.strip_prefix(head)?.strip_suffix(tail))
    .unwrap_or_else(|| panic!("the binary named no order-attempt record: {out}"));
    PathBuf::from(named.replace("<home>", &home.0.display().to_string()))
}

#[test]
fn store_key_failures_fail_closed_with_fixed_messages_and_never_fall_back() {
    let home = Home::new("key-failures");
    let legacy = home.path("tokens/session.json");
    let env = Env::legacy(&legacy);
    let key = home.path(".local/share").join(KEY);
    let snapshot = || {
        [
            bytes(&home.path(RECORD)),
            bytes(&home.path(MARKER)),
            bytes(&key),
        ]
    };

    assert_eq!(save(&home, &env, TOKEN), Ok(false));
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let before = snapshot();
    assert!(before.iter().all(Option::is_some));
    write_legacy(
        &home,
        "tokens/session.json",
        "synthetic-token-stale-plaintext",
    );
    let mut messages = Vec::new();

    // STORE_ERROR: a key file of the wrong length. UNSAFE_FILE, STORE_BACKEND_RETIRED and
    // STORE_LOCKED from an injected provider cannot reach the binary; src/auth.rs tests them.
    let failing = env.keys(&key_file(&home, "short-key", Some(&[0; 31])));
    let refusals = [
        load(&home, &failing).unwrap_err(),
        save(&home, &failing, TOKEN).unwrap_err(),
        migrate(&home, &failing).unwrap_err(),
        logout(&home, &failing).unwrap_err(),
    ];

    for message in refusals {
        assert!(message.contains(STORE_ERROR), "{message}");
        messages.push(message);
    }
    // Nothing was reset, rewritten, or read from the plaintext file instead.
    assert_eq!(snapshot(), before);
    assert!(legacy.is_file());

    // A wrong key is an authentication failure, not a missing key: auth set refuses too.
    let wrong = env.keys(&key_file(&home, "wrong-key", Some(&[9; 32])));
    assert!(load(&home, &wrong).unwrap_err().contains(STORE_ERROR));
    assert!(
        save(&home, &wrong, TOKEN)
            .unwrap_err()
            .contains(STORE_ERROR)
    );
    assert_eq!(snapshot(), before);

    // A record without its marker is an interrupted write; its token is not used.
    let marker = fs::read(home.path(MARKER)).unwrap();
    fs::remove_file(home.path(MARKER)).unwrap();
    let interrupted = load(&home, &env).unwrap_err();
    assert!(
        interrupted.contains("did not complete, so its token is not used"),
        "{interrupted}"
    );
    fs::write(home.path(MARKER), &marker).unwrap();
    fs::set_permissions(home.path(MARKER), fs::Permissions::from_mode(0o600)).unwrap();

    // A deleted key fails closed everywhere except an explicit new auth set.
    fs::remove_file(&key).unwrap();
    let missing =
        "The Krónan store key is missing. Run kronan-mcp auth set to save the token again.";
    let upstream = account();
    let (mut mcp, _) = Mcp::start(&home, &upstream.origin, &env.0);
    let answer = mcp.call("auth_status", json!({})).answer;
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    let status = answer["result"]["content"][0]["text"].as_str().unwrap();
    assert!(status.ends_with(missing), "{status}");
    assert_eq!(upstream.count(), 0);

    let refusals = [
        load_saved(&home, &env).unwrap_err(),
        migrate(&home, &env).unwrap_err(),
        logout(&home, &env).unwrap_err(),
    ];

    for message in refusals {
        assert!(message.ends_with(missing), "{message}");
        messages.push(message);
    }
    assert!(absent(&key));
    assert!(home.path(RECORD).is_file());
    assert_eq!(save(&home, &env, TOKEN), Ok(true));
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));
    assert!(absent(&legacy));

    for message in messages {
        assert!(!message.contains(TOKEN), "{message}");
        assert!(
            !message.contains(&home.0.display().to_string()),
            "{message}"
        );
        assert!(!message.contains("<home>"), "{message}");
    }
}

#[test]
fn a_legacy_path_that_aliases_the_store_or_its_files_is_refused() {
    let home = Home::new("aliases");
    let store = home.path(".config/kronan-mcp");
    let key = home.path(".local/share").join(KEY);
    let alias = home.path("alias");
    assert_eq!(
        save(&home, &Env::legacy(&home.path("session.json")), TOKEN),
        Ok(false)
    );
    fs::remove_file(&key).unwrap();
    symlink(&store, &alias).unwrap();
    let snapshot = || {
        [
            bytes(&home.path(RECORD)),
            bytes(&home.path(MARKER)),
            bytes(&key),
        ]
    };
    let before = snapshot();
    let key_lock = key
        .parent()
        .unwrap()
        .join(".")
        .join("kronan-mcp.default.key.lock");

    for legacy in [
        home.path(RECORD),
        home.path(MARKER),
        home.path(".config/kronan-mcp/session.enc.lock"),
        home.path(".config/kronan-mcp/session.enc.0123.tmp"),
        store.join("session"),
        alias.join("session.enc"),
        alias.join("session.enc.marker"),
        store.join("nested").join("..").join("session.enc"),
        key.clone(),
        key_lock,
    ] {
        // An unresolved order journal beside the configured path keeps its bytes.
        let mut journal = legacy.clone().into_os_string();
        journal.push(".order-attempts.json");
        let journal = PathBuf::from(journal);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(journal.parent().unwrap())
            .unwrap();
        fs::write(&journal, "unresolved journal").unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        let env = Env::legacy(&legacy);

        for refused in [
            save(&home, &env, TOKEN).map(|_| ()),
            migrate(&home, &env).map(|_| ()),
            logout(&home, &env),
        ] {
            let message = refused.unwrap_err();
            assert!(
                message.contains(OVERLAPS),
                "{}: {message}",
                legacy.display()
            );
        }
        assert_eq!(fs::read_to_string(&journal).unwrap(), "unresolved journal");
        fs::remove_file(&journal).unwrap();
        assert_eq!(snapshot(), before, "{}", legacy.display());
    }
}

/// Waits (bounded) until the process `pid` waits for the lock on `name` in `directory`: its lock
/// temporary is published there.
fn waits_for(directory: &Path, name: &str, pid: u32) {
    let prefix = format!("{name}.lock-tmp.{pid}-");
    let deadline = Instant::now() + PATIENCE;

    while !fs::read_dir(directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&prefix)
    }) {
        assert!(Instant::now() < deadline, "{pid} never waited for {name}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// The drop-in case paused a TypeScript migration inside its locks. Here the binary's own migration
// holds the legacy file's lock while it waits for the store's, which this test holds; logout then
// waits for the legacy file's lock until the migration has finished.
#[test]
fn logout_waits_for_an_in_flight_migration() {
    let home = Home::new("interleave");
    let legacy = home.path("tokens/session.json");
    let env = Env::legacy(&legacy);
    write_legacy(&home, "tokens/session.json", TOKEN);
    let spawn = |args: &[&str]| {
        home.command(NOWHERE, &env.0, args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let options = home.store_options();
    let store = home.path(".config/kronan-mcp");
    let (migrating, logout) = (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));

    with_secret_store(&options, |_| {
        let migration = spawn(&["auth", "migrate"]);
        waits_for(&store, "session.enc", migration.id());
        let waiting = spawn(&["auth", "logout"]);
        waits_for(&home.path("tokens"), "session.json", waiting.id());
        *migrating.lock().unwrap() = Some(migration);
        *logout.lock().unwrap() = Some(waiting);

        // Longer than the lock's longest poll interval: logout must still be waiting.
        std::thread::sleep(Duration::from_millis(600));
        let mut waiting = logout.lock().unwrap();
        assert!(
            waiting.as_mut().unwrap().try_wait().unwrap().is_none(),
            "logout did not wait"
        );
        Ok::<_, family_store::Error>(())
    })
    .unwrap();

    let migration = migrating.lock().unwrap().take().unwrap();
    let migrated = migration.wait_with_output().unwrap();
    assert_eq!(migrated.status.code(), Some(0));
    assert_eq!(String::from_utf8(migrated.stdout).unwrap(), MIGRATED);
    let logged_out = logout
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert_eq!(
        logged_out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&logged_out.stderr)
    );
    assert!(absent(&legacy));
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));
}

#[test]
fn a_legacy_file_that_cannot_be_removed_is_a_fixed_message_after_the_save() {
    let home = Home::new("cleanup");
    let legacy = home.path("tokens/session.json");
    DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(legacy.join("occupied"))
        .unwrap();
    let env = Env::legacy(&legacy);

    assert_eq!(
        save(&home, &env, TOKEN),
        Err("Saved in the encrypted store, but the old plaintext token file could not be removed. Remove it by hand.".to_owned())
    );
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));
    assert!(legacy.join("occupied").is_dir());
}

/// Every file's text under `home`, so a plaintext copy anywhere fails the test.
fn contents(home: &Home) -> String {
    fn walk(directory: &Path, texts: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let info = fs::symlink_metadata(&path).unwrap();

            if info.is_dir() {
                walk(&path, texts);
            } else if info.is_file() {
                texts.push(String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned());
            }
        }
    }
    let mut texts = Vec::new();
    walk(&home.0, &mut texts);
    texts.join("\n")
}

#[test]
fn cli_migrate_status_and_set_leave_no_plaintext_token() {
    let home = Home::new("default-paths");
    let next = "synthetic-token-after-migration";
    // The default paths: an empty KRONAN_TOKEN_FILE is unset.
    let env = Env(vec![("KRONAN_TOKEN_FILE".into(), String::new())]);
    let legacy = home.path(".config/kronan-mcp/session.json");
    write_legacy(&home, ".config/kronan-mcp/session.json", TOKEN);

    let upstream = account();
    let before = run(&home, &env, Some(&upstream), &["auth", "status"], None).unwrap();
    assert_eq!(before.split('\n').next(), Some(PLAINTEXT));
    assert_eq!(tokens(&upstream), [TOKEN]);

    assert_eq!(migrate(&home, &env).as_deref(), Ok(MIGRATED));
    assert!(absent(&legacy));
    assert!(!contents(&home).contains(TOKEN));
    assert!(home.path(RECORD).is_file());
    assert_eq!(
        fs::metadata(home.path(".local/share").join(KEY))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY));

    let after = cli(&home, &upstream.origin, &env.0, &["auth", "status"], None);
    assert_eq!(after.code, Some(0));
    assert_eq!(after.stdout.split('\n').next(), Some(ENCRYPTED));
    assert!(!format!("{}{}", after.stdout, after.stderr).contains("<home>"));

    let set = cli(
        &home,
        &upstream.origin,
        &env.0,
        &["auth", "set", "-"],
        Some(&format!("{next}\n")),
    );
    assert_eq!(set.code, Some(0), "{}", set.stderr);
    assert_eq!(tokens(&upstream).last().map(String::as_str), Some(next));
    let stored = contents(&home);
    assert!(!stored.contains(TOKEN));
    assert!(!stored.contains(next));

    assert_eq!(logout(&home, &env), Ok(()));
    let status = cli(&home, NOWHERE, &env.0, &["auth", "status"], None);
    assert_eq!(status.code, Some(1));
    assert!(status.stderr.contains(NO_TOKEN), "{}", status.stderr);
}
