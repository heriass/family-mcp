//! The token store across crash points and across the TypeScript-to-binary handover, executed
//! against this binary without Bun: integration.test.ts:481 stops the store at each crash point
//! through family_store, and the interop.test.ts store rows plant what the TypeScript package left
//! on disk (fixtures/store, frozen by tooling/ts-freeze/kronan-store.ts) for the binary to read.
//! The binary's own writes are read back by the binary; nothing reads them with TypeScript after the
//! TypeScript package is gone. Needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;

use family_store::{create_secret_key, with_secret_store};
use support::auth::*;
use support::{Home, plant, store_state};

const NEXT: &str = "synthetic-token-next-4242";

const KEY_MISSING: &str = "The Krónan store key is missing";

#[test]
fn a_store_with_a_marker_never_falls_back_to_the_legacy_file_after_any_crash_point() {
    let home = Home::new("crash");
    let legacy = home.path("tokens/session.json");
    let env = Env::legacy(&legacy);
    let record = home.store_options();
    let no_token = "No saved Krónan access token. Run kronan-mcp auth set first.";

    assert_eq!(save(&home, &env, TOKEN), Ok(false));
    write_legacy(
        &home,
        "tokens/session.json",
        "synthetic-token-stale-plaintext",
    );
    fs::remove_file(home.path(".local/share").join(KEY)).unwrap();

    // Crash after the reset: a fresh marker, no record, no key.
    with_secret_store(&record, |store| store.reset()).unwrap();
    let reset = load(&home, &env).unwrap_err();
    assert!(reset.contains(KEY_MISSING), "{reset}");
    assert!(legacy.is_file());
    // Beyond the drop-in case: migrate never resets a decided store whose key is missing, so the
    // stale plaintext token is not imported over it.
    let migrating = migrate(&home, &env).unwrap_err();
    assert!(migrating.contains(KEY_MISSING), "{migrating}");
    assert!(legacy.is_file());
    assert!(absent(&home.path(".local/share").join(KEY)));

    // Crash after the new key was created, before the write.
    create_secret_key(&record).unwrap();
    assert_eq!(load_saved(&home, &env), Err(no_token.to_owned()));
    assert_eq!(load(&home, &env), Err(no_token.to_owned()));
    assert_eq!(logout(&home, &env), Ok(()));
    assert!(absent(&legacy));
    assert_eq!(load(&home, &env), Err(no_token.to_owned()));

    // A first write that stopped after its pending marker, with a legacy token present.
    fs::remove_file(home.path(RECORD)).unwrap();
    home.write(
        MARKER,
        "{\"backend\":\"encrypted-file\",\"keySource\":\"local-file\",\"keyId\":\"local\",\"profile\":\"default\",\"migrated\":false,\"generation\":0,\"pending\":{\"generation\":1,\"nonce\":\"AAAAAAAAAAAAAAAA\"}}\n",
        None,
    );
    write_legacy(&home, "tokens/session.json", TOKEN);
    assert_eq!(load(&home, &env), Err(no_token.to_owned()));
    assert_eq!(load_saved(&home, &env), Err(no_token.to_owned()));

    // Only the explicit migration resumes it.
    assert_eq!(migrate(&home, &env).as_deref(), Ok(MIGRATED));
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));
    assert!(absent(&legacy));
}

/// The TypeScript side's result for a frozen state, and its files as the binary finds them.
fn planted(name: &str) -> (Home, serde_json::Value) {
    let home = Home::new(name);
    plant(&home, name);
    (home, store_state(name)["result"].clone())
}

#[test]
fn a_token_saved_migrated_or_removed_is_what_the_next_process_reads() {
    let env = Env::default();

    // A token the TypeScript package saved is the binary's, and the binary saves over it.
    let (home, result) = planted("saved");
    assert_eq!(result, false);
    assert_eq!(
        load_saved(&home, &env),
        Ok((TOKEN.to_owned(), ENCRYPTED.to_owned()))
    );
    assert_eq!(save(&home, &env, NEXT), Ok(false));
    assert_eq!(
        load_saved(&home, &env),
        Ok((NEXT.to_owned(), ENCRYPTED.to_owned()))
    );
    assert_eq!(logout(&home, &env), Ok(()));
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));

    // A logout the TypeScript package stored: a legacy file never counts once the store decided.
    let (home, _) = planted("logged-out");
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));
    write_legacy(&home, ".config/kronan-mcp/session.json", TOKEN);
    assert!(load(&home, &env).unwrap_err().contains(NO_TOKEN));
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY_REMOVED_LEGACY));
    assert!(absent(&home.path(".config/kronan-mcp/session.json")));
    assert_eq!(save(&home, &env, TOKEN), Ok(false));
    assert_eq!(load(&home, &env), Ok(TOKEN.to_owned()));
}

#[test]
fn a_frozen_typescript_migration_is_the_store_record_the_binary_reads() {
    let env = Env::default();

    // A legacy token the binary migrates is the store record it reads.
    let home = Home::new("binary-migrates");
    let legacy = home.path(".config/kronan-mcp/session.json");
    write_legacy(&home, ".config/kronan-mcp/session.json", TOKEN);
    assert_eq!(
        load_saved(&home, &env),
        Ok((TOKEN.to_owned(), PLAINTEXT.to_owned()))
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(MIGRATED));
    assert!(absent(&legacy));
    assert_eq!(
        load_saved(&home, &env),
        Ok((TOKEN.to_owned(), ENCRYPTED.to_owned()))
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY));

    // One the TypeScript package migrated is the binary's too, with the key it created.
    let (home, result) = planted("migrated");
    assert_eq!(result, "migrated");
    assert!(absent(&home.path(".config/kronan-mcp/session.json")));
    assert_eq!(
        load_saved(&home, &env),
        Ok((NEXT.to_owned(), ENCRYPTED.to_owned()))
    );
    assert_eq!(migrate(&home, &env).as_deref(), Ok(ALREADY));
    let key = home.path(".local/share").join(KEY);
    assert_eq!(fs::read(&key).unwrap().len(), 32);
}
