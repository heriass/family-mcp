#![allow(dead_code)]

//! The store states the TypeScript package wrote, frozen under tests/fixtures/store (see its
//! PROVENANCE.md), and the envelope those records follow, rebuilt here in test code only.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use family_store::{LocalKeyFileProvider, SecretRecordOptions};
use serde_json::Value;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store")
}

pub fn states() -> Vec<Value> {
    let text = fs::read_to_string(fixtures().join("states.json")).unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

pub fn state(id: &str) -> Value {
    states()
        .into_iter()
        .find(|state| state["id"] == id)
        .unwrap_or_else(|| panic!("no frozen state {id}"))
}

pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// Copy state `id` into `root` as the package left it: files 0600, folders 0700. Git keeps
/// neither mode, so the copy restores them.
pub fn plant(id: &str, root: &Path) {
    for name in strings(&state(id)["files"]) {
        let to = root.join(&name);
        let mut folder = to.parent().unwrap().to_path_buf();
        fs::create_dir_all(&folder).unwrap();

        while folder.starts_with(root) && folder != root {
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
            folder = folder.parent().unwrap().to_path_buf();
        }
        fs::copy(fixtures().join(id).join(&name), &to).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// The options the generator used for the explicit-path states (tests/common `options_with`).
pub fn options(root: &Path) -> SecretRecordOptions {
    SecretRecordOptions::new(
        root.join("records").join("session.enc"),
        "test-mcp",
        "default",
        "session",
        1,
        Arc::new(LocalKeyFileProvider::new(
            root.join("keys").join("test-mcp.default.key"),
        )),
        1024,
    )
}

/// Every file under `root`, relative and sorted, as the generator lists them.
pub fn files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, folder: &Path, found: &mut Vec<String>) {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();

            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

/// The record header of `test-mcp`'s `session` store at `generation` with `nonce`, in the key
/// order both languages authenticate.
pub fn header(generation: u64, nonce: &str) -> String {
    format!(
        r#"{{"v":1,"server":"test-mcp","profile":"default","purpose":"session","schema":1,"generation":{generation},"keyId":"local","nonce":"{nonce}"}}"#
    )
}

/// The committed marker at `generation` (a store with a key file, migrated once written).
pub fn marker(generation: u64) -> String {
    format!(
        "{{\"backend\":\"encrypted-file\",\"keySource\":\"local-file\",\"keyId\":\"local\",\"profile\":\"default\",\"migrated\":{},\"generation\":{generation}}}\n",
        generation > 0
    )
}

/// A record file sealed with a chosen nonce: header, newline, unpadded base64url of ciphertext
/// and tag, newline. A fixed-nonce vector, built only here; the store always draws its nonce.
pub fn sealed(key: &[u8], generation: u64, nonce: &[u8; 12], plaintext: &str) -> String {
    let header = header(generation, &URL_SAFE_NO_PAD.encode(nonce));
    let body = Aes256Gcm::new_from_slice(key)
        .unwrap()
        .encrypt(
            &Nonce::from(*nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: header.as_bytes(),
            },
        )
        .unwrap();
    format!("{header}\n{}\n", URL_SAFE_NO_PAD.encode(body))
}

/// The generation, nonce and plaintext of a record file, checked against the envelope rebuilt
/// from them byte for byte. Panics on anything else.
pub fn opened(key: &[u8], record: &str) -> (u64, [u8; 12], String) {
    let (header_line, _) = record.split_once('\n').unwrap();
    let generation: u64 = between(header_line, "\"generation\":", ",")
        .parse()
        .unwrap();
    let nonce_text = between(header_line, "\"nonce\":\"", "\"");
    let nonce: [u8; 12] = URL_SAFE_NO_PAD
        .decode(nonce_text)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(header_line, header(generation, nonce_text), "{record}");
    let body = record.trim_end().split_once('\n').unwrap().1;
    let plaintext = Aes256Gcm::new_from_slice(key)
        .unwrap()
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &URL_SAFE_NO_PAD.decode(body).unwrap(),
                aad: header_line.as_bytes(),
            },
        )
        .unwrap();
    let plaintext = String::from_utf8(plaintext).unwrap();
    assert_eq!(record, sealed(key, generation, &nonce, &plaintext));
    (generation, nonce, plaintext)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap() + start.len();
    let to = from + text[from..].find(end).unwrap();
    &text[from..to]
}
