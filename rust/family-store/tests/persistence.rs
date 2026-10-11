//! Every frozen store state (tests/fixtures/store) as the TypeScript package left it, read and
//! then written once more by this crate, with the outcome the package itself had on the same
//! files: the value or error, the marker it recovered to, which files exist. Then paths,
//! permissions and negative cases on those states. In place of the pending-marker and
//! read-and-continue halves of tests/interop.rs.

mod common;
mod frozen;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use common::*;
use family_store::{SecretRecordOptions, read_secret_record};
use serde_json::Value;

fn shown(result: Result<Option<String>, family_store::Error>) -> String {
    match result {
        Ok(Some(value)) => format!("value:{value}"),
        Ok(None) => "none".to_owned(),
        Err(error) => format!("error:{}", error.code.as_str()),
    }
}

fn read(store: &SecretRecordOptions) -> String {
    shown(read_secret_record(store).map(Some))
}

fn marker_text(store: &SecretRecordOptions) -> Value {
    fs::read_to_string(marker_path(store)).map_or(Value::Null, Value::String)
}

#[test]
fn every_frozen_state_reads_and_continues_as_the_typescript_package_did() {
    let mut checked = 0;

    for state in frozen::states() {
        if state["layout"] != "explicit" {
            continue;
        }
        let id = state["id"].as_str().unwrap();
        let expected = &state["typescript"];
        let scratch = Scratch::new();
        frozen::plant(id, &scratch.0);
        let store = frozen::options(&scratch.0);
        let key = fs::read(scratch.join("keys/test-mcp.default.key")).unwrap();
        let record_before = fs::read(&store.path).ok();

        assert_eq!(read(&store), expected["read"], "{id}: read");
        assert_eq!(
            marker_text(&store),
            expected["markerAfterRead"],
            "{id}: marker after read"
        );
        assert_eq!(
            store.path.exists(),
            expected["recordAfterRead"],
            "{id}: record after read"
        );
        // Recovery rewrites only the marker; a record is never touched by a read.
        assert_eq!(fs::read(&store.path).ok(), record_before, "{id}");

        assert_eq!(
            shown(put(&store, "continued")),
            expected["write"],
            "{id}: write"
        );
        assert_eq!(
            marker_text(&store),
            expected["markerAfterWrite"],
            "{id}: marker after write"
        );
        assert_eq!(read(&store), expected["readAfterWrite"], "{id}: read again");
        assert_eq!(
            frozen::files(&scratch.0),
            frozen::strings(&expected["files"]),
            "{id}: files"
        );

        if expected["write"] == "value:continued" {
            assert_eq!(frozen::opened(&key, &text(&store.path)).2, "continued");
            assert_eq!(mode(&store.path), 0o600, "{id}");
            assert_eq!(mode(&marker_path(&store)), 0o600, "{id}");
        } else {
            // A store that cannot be trusted is left exactly as it was found.
            assert_eq!(fs::read(&store.path).ok(), record_before, "{id}");
        }
        checked += 1;
    }
    // Not vacuous: every explicit-path state ran.
    assert_eq!(checked, 12);
}

#[test]
fn frozen_pending_marker_states_recover_by_where_the_write_stopped() {
    // These outcomes are also in states.json; spelled out here, they cannot drift with it.
    for (id, outcome, marker) in [
        (
            "pending-first",
            "error:SECRET_NOT_FOUND",
            Some(frozen::marker(0)),
        ),
        ("pending-later", "value:one", Some(frozen::marker(1))),
        ("pending-landed", "value:two", Some(frozen::marker(2))),
        ("pending-foreign", "error:STORE_WRITE_UNCERTAIN", None),
        (
            "marker-behind",
            "error:STORE_WRITE_UNCERTAIN",
            Some(frozen::marker(1)),
        ),
        (
            "record-behind",
            "error:STORE_WRITE_UNCERTAIN",
            Some(frozen::marker(2)),
        ),
        ("lone-record", "error:STORE_WRITE_UNCERTAIN", None),
        (
            "marker-only",
            "error:STORE_WRITE_UNCERTAIN",
            Some(frozen::marker(1)),
        ),
    ] {
        let scratch = Scratch::new();
        frozen::plant(id, &scratch.0);
        let store = frozen::options(&scratch.0);
        let before = fs::read_to_string(marker_path(&store)).ok();
        assert_eq!(read(&store), outcome, "{id}");

        match marker {
            Some(marker) => assert_eq!(text(&marker_path(&store)), marker, "{id}"),
            // Without a decision, the marker is not rewritten (or still absent).
            None => assert_eq!(fs::read_to_string(marker_path(&store)).ok(), before, "{id}"),
        }
    }
}

#[test]
fn unsafe_permissions_and_links_on_a_frozen_store_are_refused_untouched() {
    // A record, marker or key that other users can open.
    for name in [
        "records/session.enc",
        "records/session.enc.marker",
        "keys/test-mcp.default.key",
    ] {
        let scratch = Scratch::new();
        frozen::plant("one-write", &scratch.0);
        let store = frozen::options(&scratch.0);
        let path = scratch.join(name);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let files = frozen::files(&scratch.0);
        let read_outcome = read(&store);
        assert!(read_outcome.starts_with("error:"), "{name}: {read_outcome}");
        assert!(
            shown(put(&store, "x")).starts_with("error:"),
            "{name}: write"
        );
        assert_eq!(mode(&path), 0o644, "{name}");
        assert_eq!(frozen::files(&scratch.0), files, "{name}");
        assert_eq!(
            text(&store.path),
            fs::read_to_string(frozen::fixtures().join("one-write/records/session.enc")).unwrap()
        );
    }

    // Store folders with group or other bits.
    for folder in ["records", "keys"] {
        let scratch = Scratch::new();
        frozen::plant("one-write", &scratch.0);
        let store = frozen::options(&scratch.0);
        fs::set_permissions(scratch.join(folder), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read(&store).starts_with("error:"), "{folder}");
        assert!(shown(put(&store, "x")).starts_with("error:"), "{folder}");
        assert_eq!(mode(&scratch.join(folder)), 0o755, "{folder}");
    }

    // A record that is a symbolic link to the frozen record, or has a second name.
    let scratch = Scratch::new();
    frozen::plant("one-write", &scratch.0);
    let store = frozen::options(&scratch.0);
    let aside = scratch.join("aside.enc");
    fs::rename(&store.path, &aside).unwrap();
    symlink(&aside, &store.path).unwrap();
    assert!(read(&store).starts_with("error:"));
    assert!(shown(put(&store, "x")).starts_with("error:"));
    assert!(
        fs::symlink_metadata(&store.path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&store.path).unwrap();
    fs::hard_link(&aside, &store.path).unwrap();
    assert!(read(&store).starts_with("error:"));
    assert!(shown(put(&store, "x")).starts_with("error:"));
    assert_eq!(
        fs::metadata(&store.path).unwrap().len(),
        fs::metadata(&aside).unwrap().len()
    );

    // A key of the wrong length, or none: never replaced, never regenerated.
    for key in [&[7u8; 31][..], &[7u8; 33][..], &[][..]] {
        let scratch = Scratch::new();
        frozen::plant("one-write", &scratch.0);
        let store = frozen::options(&scratch.0);
        write(&scratch.join("keys/test-mcp.default.key"), key);
        assert_eq!(read(&store), "error:STORE_ERROR", "{} bytes", key.len());
        assert_eq!(
            shown(put(&store, "x")),
            "error:STORE_ERROR",
            "{} bytes",
            key.len()
        );
        assert_eq!(
            fs::read(scratch.join("keys/test-mcp.default.key")).unwrap(),
            key
        );
    }
    let scratch = Scratch::new();
    frozen::plant("one-write", &scratch.0);
    let store = frozen::options(&scratch.0);
    fs::remove_file(scratch.join("keys/test-mcp.default.key")).unwrap();
    assert_eq!(read(&store), "error:STORE_UNAVAILABLE");
    assert_eq!(shown(put(&store, "x")), "error:STORE_UNAVAILABLE");
    assert!(!scratch.join("keys/test-mcp.default.key").exists());
}
