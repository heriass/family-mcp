//! The record, marker and key formats against store states the TypeScript package wrote and froze
//! (tests/fixtures/store). Records carry random nonces, so they are checked as fixed-nonce
//! vectors: the envelope is rebuilt from a record's own nonce, here in test code, and must equal
//! the file byte for byte. That holds for the frozen TypeScript records and for every record this
//! crate writes. In place of tests/interop.rs, which ran the TypeScript package beside the crate.

mod common;
mod frozen;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use family_store::{
    SecretRecordOptions, TEST_SEAM, read_secret_record, with_secret_record, with_secret_store,
};

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

#[test]
fn frozen_typescript_records_are_their_envelope_rebuilt_from_their_own_nonce() {
    let mut records = 0;

    for state in frozen::states() {
        let id = state["id"].as_str().unwrap();
        let written = frozen::strings(&state["written"]);

        for name in frozen::strings(&state["files"]) {
            if !name.ends_with("session.enc") {
                continue;
            }
            let folder = frozen::fixtures().join(id);
            let key = fs::read(folder.join(key_of(&name))).unwrap();
            let record = fs::read_to_string(folder.join(&name)).unwrap();
            let (generation, _, plaintext) = frozen::opened(&key, &record);
            // Each committed write is one generation, so a record holds the value written then.
            assert_eq!(plaintext, written[generation as usize - 1], "{id}");
            records += 1;
        }
    }
    // Not vacuous: every state with a record was checked.
    assert_eq!(records, 12);
}

/// The key file beside a record in a frozen state.
fn key_of(record: &str) -> String {
    match record.split_once("test-mcp/session.enc") {
        Some(("config/", _)) => "data/family-mcp/keys/test-mcp.default.key".to_owned(),
        Some((".config/", _)) => ".local/share/family-mcp/keys/test-mcp.default.key".to_owned(),
        Some((root, _)) => format!("{root}keys/test-mcp.default.key"),
        None => "keys/test-mcp.default.key".to_owned(),
    }
}

#[test]
fn frozen_markers_and_keys_have_the_exact_formats() {
    for state in frozen::states() {
        let id = state["id"].as_str().unwrap();
        let folder = frozen::fixtures().join(id);

        for name in frozen::strings(&state["files"]) {
            let bytes = fs::read(folder.join(&name)).unwrap();

            if name.ends_with(".key") {
                assert_eq!(bytes.len(), 32, "{id} {name}");
            } else if name.ends_with(".marker") && !id.starts_with("pending-") {
                let text = String::from_utf8(bytes).unwrap();
                let generation = text
                    .trim_end()
                    .trim_end_matches('}')
                    .rsplit(':')
                    .next()
                    .unwrap();
                assert_eq!(
                    text,
                    frozen::marker(generation.parse().unwrap()),
                    "{id} {name}"
                );
            }
        }
        assert!(
            state["modes"]
                .as_object()
                .unwrap()
                .values()
                .all(|mode| mode == "600"),
            "{id}"
        );
    }
    // The pending marker announces the next generation and the nonce of the write in flight.
    let pending =
        fs::read_to_string(frozen::fixtures().join("pending-later/records/session.enc.marker"))
            .unwrap();
    let marker1 = frozen::marker(1);
    assert!(
        pending.starts_with(marker1.trim_end().trim_end_matches('}')),
        "{pending}"
    );
    assert!(pending.contains(",\"pending\":{\"generation\":2,\"nonce\":\""));
}

#[test]
fn frozen_typescript_records_are_read_and_continued() {
    for id in ["one-write", "three-writes", "largest"] {
        let scratch = Scratch::new();
        frozen::plant(id, &scratch.0);
        let store = frozen::options(&scratch.0);
        let key = fs::read(scratch.join("keys/test-mcp.default.key")).unwrap();
        let written = frozen::strings(&frozen::state(id)["written"]);
        let last = written.last().unwrap();
        let generation = written.len() as u64;

        assert_eq!(read(&store), format!("value:{last}"), "{id}");
        // Reading changes nothing.
        assert_eq!(text(&marker_path(&store)), frozen::marker(generation));

        // The next write is one generation on, in the same envelope, under the same key.
        let next = format!("frá-rust {id} 🔑");
        assert_eq!(put(&store, &next).unwrap().as_deref(), Some(next.as_str()));
        let (record_generation, _, plaintext) = frozen::opened(&key, &text(&store.path));
        assert_eq!(
            (record_generation, plaintext.as_str()),
            (generation + 1, next.as_str())
        );
        assert_eq!(text(&marker_path(&store)), frozen::marker(generation + 1));
        assert_eq!(mode(&store.path), 0o600);
        assert_eq!(mode(&marker_path(&store)), 0o600);
        assert_eq!(mode(&scratch.join("records")), 0o700);
        assert_eq!(
            frozen::files(&scratch.0),
            [
                "keys/test-mcp.default.key",
                "records/session.enc",
                "records/session.enc.marker"
            ]
        );

        // A hold that writes twice commits two generations.
        with_secret_store(&store, |held| {
            held.write("first")?;
            held.write("second")
        })
        .unwrap();
        assert_eq!(read(&store), "value:second");
        assert_eq!(text(&marker_path(&store)), frozen::marker(generation + 3));
        frozen::opened(&key, &text(&store.path));
    }

    // The created state is a key without a record: the first write is generation 1.
    let scratch = Scratch::new();
    frozen::plant("created", &scratch.0);
    let store = frozen::options(&scratch.0);
    assert_eq!(read(&store), "error:SECRET_NOT_FOUND");
    assert!(!marker_path(&store).exists());
    put(&store, "first").unwrap();
    let key = fs::read(scratch.join("keys/test-mcp.default.key")).unwrap();
    assert_eq!(
        frozen::opened(&key, &text(&store.path)).2,
        "first".to_owned()
    );
    assert_eq!(text(&marker_path(&store)), frozen::marker(1));
}

#[test]
fn tampered_frozen_records_fail_closed() {
    let scratch = Scratch::new();
    frozen::plant("one-write", &scratch.0);
    let store = frozen::options(&scratch.0);
    let original = text(&store.path);
    let (header, body) = original.trim_end().split_once('\n').unwrap();
    let swap = |text: &str, at: usize| {
        let other = if &text[at..=at] == "A" { "B" } else { "A" };
        format!("{}{other}{}", &text[..at], &text[at + 1..])
    };
    let nonce_at = header.find("\"nonce\":\"").unwrap() + 9;

    for tampered in [
        format!(
            "{}\n{body}\n",
            header.replace("\"generation\":1", "\"generation\":2")
        ),
        format!(
            "{}\n{body}\n",
            header.replace("\"schema\":1", "\"schema\":2")
        ),
        format!("{}\n{body}\n", header.replace("test-mcp", "evil-mcp")),
        format!(
            "{}\n{body}\n",
            header.replace("\"purpose\":\"session\"", "\"purpose\":\"journal\"")
        ),
        format!(
            "{}\n{body}\n",
            header.replace("\"keyId\":\"local\"", "\"keyId\":\"other\"")
        ),
        format!("{}\n{body}\n", swap(header, nonce_at)),
        format!("{}\n{body}\n", header.replace("{\"v\":1,", "{ \"v\":1,")),
        format!("{header}\n{}\n", swap(body, 0)),
        format!("{header}\n{}\n", swap(body, body.len() - 2)),
        format!("{header}\n{}\n", &body[..body.len() - 4]),
        format!("{header}\n{body}A\n"),
        format!("{header}\n{body}=\n"),
        format!("{header}\n{body}\n\n"),
        format!("{header}\n{body}"),
        format!("{header}\n\n"),
    ] {
        write(&store.path, &tampered);
        assert_eq!(read(&store), "error:STORE_ERROR", "{tampered}");
        // A refused record is never overwritten.
        assert_eq!(shown(put(&store, "x")), "error:STORE_ERROR", "{tampered}");
        assert_eq!(text(&store.path), tampered);
        assert_eq!(text(&marker_path(&store)), frozen::marker(1));
    }

    // The frozen record under another key of the right length is a wrong key.
    write(&store.path, &original);
    assert_eq!(read(&store), "value:from-ts-1");
    write(&scratch.join("keys/test-mcp.default.key"), [8; 32]);
    assert_eq!(read(&store), "error:STORE_ERROR");
    assert_eq!(text(&store.path), original);
}

const LAYOUT_ROOT: &str = "FAMILY_STORE_TEST_LAYOUT_OPERATION";

/// `read` or `write VALUE` on `test-mcp`'s default layout in this process's environment. Run by
/// the layout test in a child whose HOME holds a frozen layout.
#[test]
#[ignore = "run by a_frozen_default_layout_is_read_in_place"]
fn frozen_layout_operation() {
    let operation = std::env::var(LAYOUT_ROOT).unwrap();
    let store = SecretRecordOptions::new(
        family_store::default_secret_record_path("test-mcp").unwrap(),
        "test-mcp",
        "default",
        "session",
        1,
        family_store::default_key_provider("test-mcp", "default").unwrap(),
        1024,
    );
    let result = match operation.split_once(' ') {
        Some(("write", value)) => with_secret_record(&store, |_| {
            Ok::<_, family_store::Error>(Some(value.to_owned()))
        }),
        _ => read_secret_record(&store).map(Some),
    };
    println!("result:{}", shown(result));
}

fn in_layout(home: &Path, seam: bool, operation: &str) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "frozen_layout_operation",
            "--ignored",
            "--nocapture",
        ])
        .env(LAYOUT_ROOT, operation)
        .env("HOME", home)
        .env_remove(TEST_SEAM)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove(family_store::TEST_TMUTIL)
        .env_remove(family_store::TEST_LS)
        .stdin(Stdio::null());

    if seam {
        command
            .env(TEST_SEAM, "1")
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"));
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("result:"))
        .unwrap_or_else(|| panic!("no result: {stdout}"))
        .to_owned()
}

#[test]
fn a_frozen_default_layout_is_read_in_place() {
    let platform = if cfg!(target_os = "macos") {
        "layout-macos"
    } else {
        "layout-linux"
    };

    for (id, seam) in [("layout-seam", true), (platform, false)] {
        let home = Scratch::new();
        frozen::plant(id, &home.0);
        let before = frozen::files(&home.0);
        assert_eq!(
            in_layout(&home.0, seam, "read"),
            "value:from-ts-layout",
            "{id}"
        );
        assert_eq!(
            in_layout(&home.0, seam, "write from-rust-layout"),
            "value:from-rust-layout",
            "{id}"
        );
        assert_eq!(
            in_layout(&home.0, seam, "read"),
            "value:from-rust-layout",
            "{id}"
        );
        // The same files, in place: nothing moved to another layout.
        assert_eq!(frozen::files(&home.0), before, "{id}");

        for name in &before {
            assert_eq!(mode(&home.join(name)), 0o600, "{id} {name}");
            assert_eq!(
                mode(home.join(name).parent().unwrap()),
                0o700,
                "{id} {name}"
            );
        }
        let record = before.iter().find(|name| name.ends_with(".enc")).unwrap();
        let key = fs::read(home.join(&key_of(record))).unwrap();
        assert_eq!(
            frozen::opened(&key, &text(&home.join(record))).2,
            "from-rust-layout".to_owned()
        );
    }
}
