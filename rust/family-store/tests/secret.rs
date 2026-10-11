//! The cases of packages/session-store/test/secret.test.ts that this crate's API can express.

mod common;

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::*;
use family_store::{
    Cancel, Code, Error, FakeKeyProvider, Key, KeyProvider, LocalKeyFileProvider,
    SecretRecordOptions, create_secret_key, existing_paths, read_secret_record,
    secret_store_exists, with_secret_record, with_secret_store,
};

const MAX_INTEGER: u64 = 999_999_999_999_999;

const EMPTY_LOCAL: &str = "{\"backend\":\"encrypted-file\",\"keySource\":\"local-file\",\"keyId\":\"local\",\"profile\":\"default\",\"migrated\":false,\"generation\":0}\n";

const EMPTY_TEST: &str = "{\"backend\":\"test\",\"keySource\":\"memory\",\"keyId\":\"test\",\"profile\":\"default\",\"migrated\":false,\"generation\":0}\n";

fn flip(text: &str, at: usize) -> String {
    let swap = if &text[at..=at] == "A" { "B" } else { "A" };
    format!("{}{swap}{}", &text[..at], &text[at + 1..])
}

fn pending(marker: &str, generation: u64, nonce: &str) -> String {
    marker.replace(
        "}\n",
        &format!(",\"pending\":{{\"generation\":{generation},\"nonce\":\"{nonce}\"}}}}\n"),
    )
}

fn nonce_of(record: &str) -> String {
    let start = record.find("\"nonce\":\"").unwrap() + 9;
    record[start..start + 16].to_owned()
}

fn local(directory: &std::path::Path, key: &std::path::Path) -> SecretRecordOptions {
    options_with(directory, Arc::new(LocalKeyFileProvider::new(key)), 1024)
}

fn with_keys(store: &SecretRecordOptions, keys: impl KeyProvider + 'static) -> SecretRecordOptions {
    SecretRecordOptions {
        keys: Arc::new(keys),
        ..store.clone()
    }
}

/// A transaction that fails with `expected` before its update callback runs.
fn refuses(store: &SecretRecordOptions, expected: Code) {
    let mut called = false;
    let result = with_secret_record(store, |_| {
        called = true;
        Ok::<_, Error>(Some("changed".to_owned()))
    });
    assert_eq!(code(result), Some(expected));
    assert!(!called);
}

/// Seal a record directly, for generations a test cannot reach by writing.
fn forge(store: &SecretRecordOptions, generation: u64, plaintext: &str) -> String {
    let nonce = [9; 12];
    let header = format!(
        "{{\"v\":1,\"server\":\"{}\",\"profile\":\"{}\",\"purpose\":\"{}\",\"schema\":{},\"generation\":{generation},\"keyId\":\"{}\",\"nonce\":\"{}\"}}",
        store.server,
        store.profile,
        store.purpose,
        store.schema,
        store.keys.key_id(),
        URL_SAFE_NO_PAD.encode(nonce)
    );
    let body = Aes256Gcm::new((&KEY).into())
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: header.as_bytes(),
            },
        )
        .unwrap();
    format!("{header}\n{}\n", URL_SAFE_NO_PAD.encode(body))
}

struct Locked;

impl KeyProvider for Locked {
    fn backend(&self) -> &str {
        "encrypted-file"
    }

    fn key_source(&self) -> &str {
        "local-file"
    }

    fn key_id(&self) -> &str {
        "local"
    }

    fn get_key(&self, _cancel: &Cancel) -> Result<Key, Error> {
        Err(Error::new(Code::StoreLocked, "Locked."))
    }

    fn create_key(&self, _cancel: &Cancel) -> Result<(), Error> {
        panic!("Not called.")
    }
}

#[test]
fn records_round_trip_as_owner_only_ciphertext_with_a_committed_marker_per_generation() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    assert_eq!(
        with_secret_record(&store, |_| Ok::<_, Error>(None)).unwrap(),
        None
    );
    assert_eq!(code(read_secret_record(&store)), Some(Code::SecretNotFound));

    assert_eq!(put(&store, SECRET).unwrap().as_deref(), Some(SECRET));
    let second = format!("{SECRET}-2");
    assert_eq!(put(&store, &second).unwrap(), Some(second.clone()));
    assert_eq!(read_secret_record(&store).unwrap(), second);
    // An update that changes nothing returns the stored value.
    assert_eq!(
        with_secret_record(&store, |_| Ok::<_, Error>(None)).unwrap(),
        Some(second.clone())
    );

    let record = text(&store.path);
    assert!(!record.contains(SECRET));
    assert!(record.starts_with("{\"v\":1,\"server\":\"test-mcp\","));
    assert!(record.lines().next().unwrap().contains("\"generation\":2,"));
    assert_eq!(
        text(&marker_path(&store)),
        "{\"backend\":\"test\",\"keySource\":\"memory\",\"keyId\":\"test\",\"profile\":\"default\",\"migrated\":true,\"generation\":2}\n"
    );
    assert_eq!(mode(&store.path), 0o600);
    assert_eq!(mode(&marker_path(&store)), 0o600);

    // Another purpose, schema or server never opens this record.
    for other in [
        SecretRecordOptions {
            purpose: "journal".into(),
            ..store.clone()
        },
        SecretRecordOptions {
            schema: 2,
            ..store.clone()
        },
        SecretRecordOptions {
            server: "other-mcp".into(),
            ..store.clone()
        },
    ] {
        assert_eq!(code(read_secret_record(&other)), Some(Code::StoreError));
    }
}

#[test]
fn one_mib_payloads_round_trip_and_size_bounds_are_enforced() {
    let scratch = Scratch::new();
    let store = SecretRecordOptions {
        max_bytes: 1_048_576,
        ..options(&scratch.0)
    };
    let payload = "x".repeat(1_048_576);
    put(&store, &payload).unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), payload);
    assert_eq!(
        code(put(&store, &format!("{payload}y"))),
        Some(Code::TooLarge)
    );
    assert_eq!(read_secret_record(&store).unwrap(), payload);
    let small = SecretRecordOptions {
        max_bytes: 1024,
        ..store.clone()
    };
    assert_eq!(code(read_secret_record(&small)), Some(Code::TooLarge));

    // A limit too large to count in bytes is no limit; the size arithmetic never wraps or panics.
    for max_bytes in [usize::MAX, usize::MAX / 4 + 1] {
        let unbounded = SecretRecordOptions {
            max_bytes,
            ..store.clone()
        };
        assert_eq!(read_secret_record(&unbounded).unwrap().len(), payload.len());
        put(&unbounded, &payload).unwrap();
    }
    assert_eq!(read_secret_record(&store).unwrap(), payload);
}

#[test]
fn header_tampering_ciphertext_tampering_and_a_wrong_key_are_one_store_error() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    put(&store, SECRET).unwrap();
    let original = text(&store.path);
    let mut lines = original.split('\n');
    let (header, body) = (lines.next().unwrap(), lines.next().unwrap());

    for tampered in [
        format!(
            "{}\n{body}\n",
            header.replace("\"schema\":1", "\"schema\":2")
        ),
        format!(
            "{}\n{body}\n",
            flip(header, header.find("\"nonce\":\"").unwrap() + 9)
        ),
        format!("{header}\n{}\n", flip(body, 3)),
        format!("{header}\n{body}A\n"),
        format!("{header}\n{body}\n\n"),
        format!("{header}\n{body}=\n"),
        format!("{header}\n{body}"),
    ] {
        write(&store.path, &tampered);
        assert_eq!(
            code(read_secret_record(&store)),
            Some(Code::StoreError),
            "{tampered}"
        );
    }
    write(&store.path, &original);
    assert_eq!(read_secret_record(&store).unwrap(), SECRET);

    let wrong_key = with_keys(&store, FakeKeyProvider::new(Some([8; 32])));
    assert_eq!(code(read_secret_record(&wrong_key)), Some(Code::StoreError));
    assert_eq!(code(put(&wrong_key, "x")), Some(Code::StoreError));
    assert_eq!(text(&store.path), original);

    let other_key_id = with_keys(&store, FakeKeyProvider::with_key_id(Some(KEY), "rotated"));
    assert_eq!(
        code(read_secret_record(&other_key_id)),
        Some(Code::StoreError)
    );
}

#[test]
fn a_missing_key_is_store_unavailable_and_is_never_regenerated() {
    let scratch = Scratch::new();
    let key_path = scratch.join("keys").join("test-mcp.key");
    let store = local(&scratch.0, &key_path);
    assert_eq!(
        code(read_secret_record(&store)),
        Some(Code::StoreUnavailable)
    );
    assert_eq!(code(put(&store, SECRET)), Some(Code::StoreUnavailable));

    create_secret_key(&store).unwrap();
    assert_eq!(mode(&key_path), 0o600);
    assert_eq!(mode(&scratch.join("keys")), 0o700);
    assert_eq!(fs::read(&key_path).unwrap().len(), 32);
    put(&store, SECRET).unwrap();

    assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
    fs::remove_file(&key_path).unwrap();
    assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
    assert_eq!(
        code(read_secret_record(&store)),
        Some(Code::StoreUnavailable)
    );
    assert_eq!(code(put(&store, "x")), Some(Code::StoreUnavailable));
    assert!(!key_path.exists());

    // Even without a store, an existing key file is never replaced.
    let fresh = local(&scratch.join("fresh"), &key_path);
    write(&key_path, KEY);
    assert_eq!(code(create_secret_key(&fresh)), Some(Code::StoreError));
    assert_eq!(fs::read(&key_path).unwrap(), KEY);
}

#[test]
fn reset_removes_an_undecryptable_store_only_while_its_key_is_missing() {
    let scratch = Scratch::new();
    let key_path = scratch.join("keys/test-mcp.key");
    let store = local(&scratch.0, &key_path);
    let reset = |store: &SecretRecordOptions| with_secret_store(store, |held| held.reset());
    create_secret_key(&store).unwrap();
    put(&store, "secret").unwrap();

    // A readable store, or a key failure other than a missing key, is never reset.
    assert_eq!(code(reset(&store)), Some(Code::StoreError));
    assert_eq!(
        code(reset(&with_keys(&store, Locked))),
        Some(Code::StoreLocked)
    );
    assert_eq!(read_secret_record(&store).unwrap(), "secret");

    assert!(secret_store_exists(&store.path).unwrap());
    fs::remove_file(&key_path).unwrap();
    reset(&store).unwrap();
    assert!(!store.path.exists());

    // The marker is rewritten, never removed: the store still decides after a crash here.
    assert_eq!(text(&marker_path(&store)), EMPTY_LOCAL);
    assert!(secret_store_exists(&store.path).unwrap());
    assert_eq!(
        code(read_secret_record(&store)),
        Some(Code::StoreUnavailable)
    );
    reset(&store).unwrap();

    create_secret_key(&store).unwrap();
    assert_eq!(code(read_secret_record(&store)), Some(Code::SecretNotFound));
    assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
    assert_eq!(put(&store, "next").unwrap().as_deref(), Some("next"));
    assert_eq!(read_secret_record(&store).unwrap(), "next");
}

#[test]
fn a_key_is_created_only_for_an_empty_store_whose_key_is_conclusively_missing() {
    let scratch = Scratch::new();
    let key_path = scratch.join("keys").join("test-mcp.key");
    let store = local(&scratch.0, &key_path);
    let marker = marker_path(&store);
    assert!(!secret_store_exists(&store.path).unwrap());

    // A pending first write, or a marker past generation 0, is never set up again.
    for text in [
        pending(EMPTY_LOCAL, 1, "AAAAAAAAAAAAAAAA"),
        EMPTY_LOCAL.replace(
            "\"migrated\":false,\"generation\":0",
            "\"migrated\":true,\"generation\":1",
        ),
    ] {
        write(&marker, text);
        assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
        assert!(!key_path.exists());
    }

    // A generation-0 marker with a readable key is refused; with a locked key the error stays.
    write(&marker, EMPTY_LOCAL);
    write(&key_path, KEY);
    assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
    fs::remove_file(&key_path).unwrap();
    assert_eq!(
        code(create_secret_key(&with_keys(&store, Locked))),
        Some(Code::StoreLocked)
    );

    create_secret_key(&store).unwrap();
    assert_eq!(fs::read(&key_path).unwrap().len(), 32);
    assert_eq!(put(&store, SECRET).unwrap().as_deref(), Some(SECRET));
}

#[test]
fn with_secret_store_holds_one_lock_across_setup_reads_and_writes() {
    let scratch = Scratch::new();
    let store = with_keys(&options(&scratch.0), FakeKeyProvider::new(None));

    let result = with_secret_store(&store, |held| {
        assert!(!held.exists()?);
        held.create_key()?;
        assert_eq!(held.update(|_| Ok::<_, Error>(None))?, None);
        assert!(!held.exists()?);
        let stored = held.update(|_| Ok::<_, Error>(Some(SECRET.to_owned())))?;
        assert_eq!(stored.as_deref(), Some(SECRET));

        // Another caller waits for this hold instead of interleaving.
        let other = SecretRecordOptions {
            wait: Duration::ZERO,
            ..store.clone()
        };
        assert_eq!(code(put(&other, "other")), Some(Code::Busy));

        held.exists()
    });
    assert!(result.unwrap());
    assert_eq!(read_secret_record(&store).unwrap(), SECRET);
}

#[test]
fn with_secret_store_commits_every_write_in_one_hold_as_its_own_generation() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);

    with_secret_store(&store, |held| {
        assert_eq!(held.read()?, None);
        held.write("one")?;
        assert!(text(&marker).contains("\"generation\":1}"));
        held.write("two")?;
        assert_eq!(held.read()?.as_deref(), Some("two"));
        held.write("three")?;
        assert!(text(&marker).contains("\"generation\":3}"));
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), "three");

    // A write without a read first loads the store itself.
    with_secret_store(&store, |held| held.write("four")).unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), "four");
    assert!(text(&marker).contains("\"generation\":4}"));
}

struct Flaky(AtomicU32);

impl KeyProvider for Flaky {
    fn backend(&self) -> &str {
        "test"
    }

    fn key_source(&self) -> &str {
        "memory"
    }

    fn key_id(&self) -> &str {
        "test"
    }

    fn get_key(&self, _cancel: &Cancel) -> Result<Key, Error> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(Error::new(Code::StoreLocked, "synthetic refusal"));
        }
        Ok(Key::new(KEY))
    }

    fn create_key(&self, _cancel: &Cancel) -> Result<(), Error> {
        Ok(())
    }
}

#[test]
fn a_with_secret_store_handle_stops_working_after_a_failed_write() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);

    with_secret_store(&store, |held| {
        assert_eq!(held.read()?, None);
        // The record path cannot be replaced, so the write fails after its pending marker.
        fs::create_dir_all(store.path.join("blocked")).unwrap();
        assert_eq!(code(held.write("lost")), Some(Code::StoreWriteUncertain));
        assert_eq!(code(held.read()), Some(Code::StoreWriteUncertain));
        assert_eq!(code(held.write("again")), Some(Code::StoreWriteUncertain));
        assert_eq!(code(held.exists()), Some(Code::StoreWriteUncertain));
        assert_eq!(code(held.create_key()), Some(Code::StoreWriteUncertain));
        Ok::<_, Error>(())
    })
    .unwrap();

    // The next hold finds the write never committed and drops it.
    fs::remove_dir_all(&store.path).unwrap();
    assert!(text(&marker).contains("\"pending\""));
    assert_eq!(with_secret_store(&store, |held| held.read()).unwrap(), None);
    assert_eq!(text(&marker), EMPTY_TEST);

    // A write refused before any file changed keeps its code and still ends the handle.
    let tiny = SecretRecordOptions {
        max_bytes: 4,
        ..store.clone()
    };
    with_secret_store(&tiny, |held| {
        assert_eq!(code(held.write("too large")), Some(Code::TooLarge));
        assert_eq!(code(held.write("ok")), Some(Code::TooLarge));
        Ok::<_, Error>(())
    })
    .unwrap();

    // A write that fails while opening the store ends the handle too, though the key then works.
    let flaky = Arc::new(Flaky(AtomicU32::new(0)));
    let refused = SecretRecordOptions {
        keys: flaky.clone(),
        ..store.clone()
    };
    with_secret_store(&refused, |held| {
        assert_eq!(code(held.write("first")), Some(Code::StoreLocked));
        assert_eq!(code(held.write("second")), Some(Code::StoreLocked));
        assert_eq!(code(held.read()), Some(Code::StoreLocked));
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(flaky.0.load(Ordering::SeqCst), 1);
    assert_eq!(code(read_secret_record(&store)), Some(Code::SecretNotFound));
}

#[test]
fn key_files_must_be_single_link_owner_only_regular_files_of_exactly_32_bytes() {
    let scratch = Scratch::new();
    let key_path = scratch.join("keys").join("test-mcp.key");
    let store = local(&scratch.0, &key_path);
    write(&key_path, KEY);
    put(&store, SECRET).unwrap();

    let alias = scratch.join("alias.key");
    symlink(&key_path, &alias).unwrap();
    assert_eq!(
        code(read_secret_record(&local(&scratch.0, &alias))),
        Some(Code::UnsafeFile)
    );
    fs::remove_file(&alias).unwrap();

    fs::hard_link(&key_path, &alias).unwrap();
    assert_eq!(code(read_secret_record(&store)), Some(Code::UnsafeFile));
    fs::remove_file(&alias).unwrap();

    for mode in [0o640, 0o604, 0o620] {
        fs::set_permissions(&key_path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(code(read_secret_record(&store)), Some(Code::UnsafeFile));
    }
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), SECRET);

    for bad in [KEY[1..].to_vec(), vec![0; 33], "07".repeat(32).into_bytes()] {
        write(&key_path, bad);
        assert_eq!(code(read_secret_record(&store)), Some(Code::StoreError));
    }
}

#[test]
fn an_interrupted_write_is_committed_or_dropped_by_where_it_stopped() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);
    put(&store, "one").unwrap();
    let record1 = text(&store.path);
    let marker1 = text(&marker);
    let marker2 = marker1.replace("\"generation\":1", "\"generation\":2");
    put(&store, "two").unwrap();
    let record2 = text(&store.path);
    let nonce2 = nonce_of(&record2);

    // Crash after the record rename, before the marker commit: the candidate is committed.
    write(&marker, pending(&marker1, 2, &nonce2));
    assert_eq!(read_secret_record(&store).unwrap(), "two");
    assert_eq!(text(&marker), marker2);

    // Crash before the record rename: the write never committed, so generation 1 is kept.
    write(&store.path, &record1);
    write(&marker, pending(&marker1, 2, &nonce2));
    assert_eq!(read_secret_record(&store).unwrap(), "one");
    assert_eq!(text(&marker), marker1);
    put(&store, "three").unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), "three");
    assert_eq!(text(&marker), marker2);
    put(&store, "four").unwrap();
    let record3 = text(&store.path);

    let uncertain = [
        // The exact announced candidate, but pending moves backward or skips a generation.
        (&record1, pending(&marker2, 1, &nonce_of(&record1))),
        (&record3, pending(&marker1, 3, &nonce_of(&record3))),
        // A record at the pending generation that is not the announced candidate.
        (&record2, pending(&marker1, 2, "AAAAAAAAAAAAAAAA")),
        // A pending generation that is not the next one, or a record at neither generation.
        (&record2, pending(&marker1, 3, &nonce2)),
        (&record1, pending(&marker2, 3, &nonce2)),
        // A stale marker, or a marker ahead of a rolled-back record.
        (&record2, marker1.clone()),
        (&record1, marker2.clone()),
    ];

    for (record, state) in &uncertain {
        write(&store.path, record);
        write(&marker, state);
        assert_eq!(
            code(read_secret_record(&store)),
            Some(Code::StoreWriteUncertain)
        );
        refuses(&store, Code::StoreWriteUncertain);
        assert_eq!(&text(&store.path), *record);
        assert_eq!(&text(&marker), state);
    }

    fs::remove_file(&marker).unwrap();
    assert_eq!(
        code(read_secret_record(&store)),
        Some(Code::StoreWriteUncertain)
    );

    write(&marker, &marker1);
    fs::remove_file(&store.path).unwrap();
    assert_eq!(
        code(read_secret_record(&store)),
        Some(Code::StoreWriteUncertain)
    );
    assert_eq!(code(put(&store, "four")), Some(Code::StoreWriteUncertain));
    assert!(!store.path.exists());

    write(
        &marker,
        marker1.replace("\"keyId\":\"test\"", "\"keyId\":\"other\""),
    );
    assert_eq!(code(read_secret_record(&store)), Some(Code::StoreError));

    for malformed in [
        "{\"backend\":\"test\"}\n".to_owned(),
        marker1.trim_end().to_owned(),
        format!("{marker1}\n"),
        marker1.replace("\"generation\":1", "\"generation\":01"),
        marker1.replace("\"generation\":1", "\"generation\":1234567890123456"),
        marker1.replace("\"profile\":\"default\"", "\"profile\":\"-default\""),
    ] {
        write(&marker, &malformed);
        assert_eq!(
            code(read_secret_record(&store)),
            Some(Code::StoreError),
            "{malformed}"
        );
    }
}

#[test]
fn a_first_write_that_stopped_before_its_record_leaves_an_empty_store() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);

    write(&marker, pending(EMPTY_TEST, 1, "AAAAAAAAAAAAAAAA"));
    assert_eq!(code(read_secret_record(&store)), Some(Code::SecretNotFound));
    assert_eq!(text(&marker), EMPTY_TEST);
    put(&store, "one").unwrap();
    assert_eq!(read_secret_record(&store).unwrap(), "one");
    assert_eq!(
        text(&marker),
        EMPTY_TEST.replace(
            "\"migrated\":false,\"generation\":0",
            "\"migrated\":true,\"generation\":1"
        )
    );
}

#[test]
fn error_messages_contain_no_path_key_or_secret() {
    let scratch = Scratch::new();
    let key_path = scratch.join("keys").join("test-mcp.key");
    let store = local(&scratch.0, &key_path);
    let mut errors = Vec::new();

    errors.extend(read_secret_record(&store).err());
    write(&key_path, KEY);
    errors.extend(read_secret_record(&store).err());
    put(&store, SECRET).unwrap();
    errors.extend(put(&store, &SECRET.repeat(100)).err());
    let zero = SecretRecordOptions {
        keys: Arc::new(LocalKeyFileProvider::new(scratch.join("zero.key"))),
        ..store.clone()
    };
    write(&scratch.join("zero.key"), [0; 32]);
    errors.extend(read_secret_record(&zero).err());
    write(&marker_path(&store), "{}\n");
    errors.extend(read_secret_record(&store).err());
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    errors.extend(read_secret_record(&store).err());

    assert_eq!(
        errors.iter().map(|error| error.code).collect::<Vec<_>>(),
        [
            Code::StoreUnavailable,
            Code::SecretNotFound,
            Code::TooLarge,
            Code::StoreError,
            Code::StoreError,
            Code::UnsafeFile,
        ]
    );

    for error in &errors {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(scratch.0.to_str().unwrap()), "{shown}");
        assert!(!shown.contains("session.enc"), "{shown}");
        assert!(!shown.contains("test-mcp.key"), "{shown}");
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(!shown.contains(&"07".repeat(32)), "{shown}");
    }
}

#[test]
fn an_existing_empty_key_file_is_malformed_and_never_filled_in() {
    let scratch = Scratch::new();
    let key_path = scratch.join("k.key");
    write(&key_path, "");
    let store = local(&scratch.0, &key_path);
    assert_eq!(code(read_secret_record(&store)), Some(Code::StoreError));
    assert_eq!(code(create_secret_key(&store)), Some(Code::StoreError));
    assert_eq!(fs::read(&key_path).unwrap().len(), 0);
}

#[test]
fn a_migrated_marker_without_a_record_is_never_an_empty_store() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);
    let base = "{\"backend\":\"test\",\"keySource\":\"memory\",\"keyId\":\"test\",\"profile\":\"default\",";

    for state in [
        format!(
            "{base}\"migrated\":true,\"generation\":0,\"pending\":{{\"generation\":1,\"nonce\":\"AAAAAAAAAAAAAAAA\"}}}}\n"
        ),
        format!("{base}\"migrated\":true,\"generation\":0}}\n"),
        format!("{base}\"migrated\":false,\"generation\":1}}\n"),
    ] {
        write(&marker, &state);
        assert_eq!(
            code(read_secret_record(&store)),
            Some(Code::StoreWriteUncertain)
        );
        refuses(&store, Code::StoreWriteUncertain);
        assert_eq!(text(&marker), state);
        assert!(!store.path.exists());
    }
}

#[test]
fn schemas_and_generations_stay_in_the_range_the_reader_parses() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);
    let marker = marker_path(&store);

    for schema in [0, MAX_INTEGER + 1, u64::MAX] {
        let wide = SecretRecordOptions {
            schema,
            ..store.clone()
        };
        refuses(&wide, Code::InvalidArgument);
    }
    assert!(!scratch.join("records").exists());

    let widest = SecretRecordOptions {
        schema: MAX_INTEGER,
        ..store.clone()
    };
    put(&widest, SECRET).unwrap();
    assert_eq!(read_secret_record(&widest).unwrap(), SECRET);

    // The last generation is written and read back; a write past it is refused before update.
    let before = format!(
        "{{\"backend\":\"test\",\"keySource\":\"memory\",\"keyId\":\"test\",\"profile\":\"default\",\"migrated\":true,\"generation\":{}}}\n",
        MAX_INTEGER - 1
    );
    write(&store.path, forge(&store, MAX_INTEGER - 1, SECRET));
    write(&marker, &before);
    assert_eq!(put(&store, "last").unwrap().as_deref(), Some("last"));
    let last = text(&store.path);
    let last_marker = text(&marker);
    assert_eq!(
        last_marker,
        before.replace(&(MAX_INTEGER - 1).to_string(), &MAX_INTEGER.to_string())
    );

    refuses(&store, Code::StoreError);
    assert_eq!(text(&store.path), last);
    assert_eq!(text(&marker), last_marker);
}

#[test]
fn store_names_are_validated() {
    let scratch = Scratch::new();
    let store = options(&scratch.0);

    for name in ["", "-w", "test mcp", "../x", &"a".repeat(65), "naïve"] {
        let bad = SecretRecordOptions {
            profile: name.to_owned(),
            ..store.clone()
        };
        refuses(&bad, Code::InvalidArgument);
    }
    let longest = SecretRecordOptions {
        profile: "a".repeat(64),
        ..store.clone()
    };
    put(&longest, SECRET).unwrap();
    assert_eq!(read_secret_record(&longest).unwrap(), SECRET);
}

// Each change rewrites the header and the matching marker without re-encrypting, so only the
// authenticated data can reject it.
#[test]
fn every_record_header_field_is_authenticated_not_only_compared() {
    for (from, to, in_marker) in [
        ("\"generation\":1", "\"generation\":2", true),
        ("\"server\":\"test-mcp\"", "\"server\":\"other-mcp\"", false),
        ("\"profile\":\"default\"", "\"profile\":\"other\"", true),
        ("\"purpose\":\"session\"", "\"purpose\":\"journal\"", false),
        ("\"schema\":1", "\"schema\":2", false),
        ("\"keyId\":\"test\"", "\"keyId\":\"rotated\"", true),
    ] {
        let scratch = Scratch::new();
        let original = options(&scratch.0);
        let marker_file = marker_path(&original);
        put(&original, SECRET).unwrap();
        let stored = text(&original.path);
        let (header, body) = stored.trim_end().split_once('\n').unwrap();
        let record = format!("{}\n{body}\n", header.replace(from, to));
        let marker = text(&marker_file);
        let changed = if in_marker {
            marker.replace(from, to)
        } else {
            marker
        };
        assert!(!record.contains(from));
        write(&original.path, &record);
        write(&marker_file, &changed);

        let pick = |field: &str, kept: &str| {
            let value = to.split(':').nth(1).unwrap().trim_matches('"');
            if from.starts_with(&format!("\"{field}\"")) {
                value.to_owned()
            } else {
                kept.to_owned()
            }
        };
        let store = SecretRecordOptions {
            server: pick("server", "test-mcp"),
            profile: pick("profile", "default"),
            purpose: pick("purpose", "session"),
            schema: pick("schema", "1").parse().unwrap(),
            keys: Arc::new(FakeKeyProvider::with_key_id(
                Some(KEY),
                &pick("keyId", "test"),
            )),
            ..original.clone()
        };

        assert_eq!(
            code(read_secret_record(&store)),
            Some(Code::StoreError),
            "{from}"
        );
        refuses(&store, Code::StoreError);
        assert_eq!(text(&original.path), record);
        assert_eq!(text(&marker_file), changed);
    }
}

const RETIRED: &str = "This store is a leftover of an earlier test build that kept its key in the macOS Keychain, which is no longer used. Remove session.enc and session.enc.marker from the server's folder in ~/Library/Application Support/family-mcp, then sign in again.";

fn retired<T: std::fmt::Debug>(result: Result<T, Error>) {
    let error = result.expect_err("retired");
    assert_eq!(error.code, Code::StoreBackendRetired);
    assert_eq!(error.message, RETIRED);
}

#[test]
fn a_store_set_up_with_the_retired_keychain_accessor_is_refused_before_any_key_or_reset() {
    let scratch = Scratch::new();
    let keys = Arc::new(LocalKeyFileProvider::new(scratch.join("keys/test-mcp.key")));
    let store = options_with(&scratch.0, keys.clone(), 1024);
    let marker = marker_path(&store);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(scratch.join("records"))
        .unwrap();
    write(
        &marker,
        "{\"backend\":\"encrypted-file\",\"keySource\":\"keychain-accessor\",\"keyId\":\"keychain\",\"profile\":\"default\",\"migrated\":true,\"generation\":1}\n",
    );
    write(&store.path, forge(&store, 1, SECRET));
    let before = [fs::read(&store.path).unwrap(), fs::read(&marker).unwrap()];

    // Without a key file, as that build leaves it, and with one: the marker decides first.
    for with_key in [false, true] {
        if with_key {
            keys.create_key(&Cancel::default()).unwrap();
        }
        retired(read_secret_record(&store));
        refuses(&store, Code::StoreBackendRetired);
        retired(with_secret_store(&store, |held| held.reset()));
        retired(create_secret_key(&store));
        retired(with_secret_store(&store, |held| held.check_key()));
        assert_eq!(
            [fs::read(&store.path).unwrap(), fs::read(&marker).unwrap()],
            before
        );
    }
}

#[test]
fn files_of_a_retired_layout_make_the_store_decide_without_being_read() {
    let scratch = Scratch::new();
    let old = scratch.join("old/session.enc.marker");
    let store = SecretRecordOptions {
        retired: vec![scratch.join("missing"), old.clone()],
        ..options(&scratch.0)
    };
    let held_exists = || with_secret_store(&store, |held| held.exists()).unwrap();

    assert!(!held_exists());
    fs::create_dir(scratch.join("old")).unwrap();
    // Unreadable and malformed: existence is all that is checked.
    fs::write(&old, "not a marker").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(held_exists());
    assert_eq!(with_secret_store(&store, |held| held.read()).unwrap(), None);
    assert_eq!(existing_paths(&store.retired).unwrap(), [old]);
}
