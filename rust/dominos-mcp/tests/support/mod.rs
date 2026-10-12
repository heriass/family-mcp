//! What the frozen-fixture replay and the executing tests share: a scratch home laid out as the
//! parity scripts laid out theirs (tests/ts/scratch.ts), a loopback Domino’s that records every
//! request, the binary's CLI and a raw JSON-RPC client for `serve`, and the normalizations the
//! TypeScript freeze applied (tooling/ts-freeze/dominos-parity.ts). No test here needs Bun.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::fs::{self, DirBuilder};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use family_store::{
    SecretRecordOptions, StoreEnvironment, default_secret_record_path_in, key_provider_in,
    with_secret_store,
};
use serde_json::{Map, Value, json};

pub mod auth;
pub mod provider;

use provider::{Provider, cart};

pub const BINARY: &str = env!("CARGO_BIN_EXE_dominos-mcp");

/// How long one answer, one exit or one request may take before a test fails instead of hanging.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// What no output may hold: the parity scripts' secret markers and every synthetic token.
pub const LEAKS: [&str; 7] = [
    "secret-",
    "private-",
    "synthetic-secret",
    "parity-access",
    "DO-NOT-RETURN",
    "rotated-access",
    "synthetic-access",
];

/// The store's own bound (src/store.rs `SESSION_MAX_BYTES`).
pub const SESSION_MAX_BYTES: usize = 132_707;

/// fixtures.ts `TOKENS`.
pub const TOKENS: [&str; 4] = [
    "synthetic-access",
    "synthetic-refresh",
    "rotated-access",
    "rotated-refresh",
];

/// A private home for one case, removed on drop. Its path is canonical, as the freeze's was.
pub struct Home(pub PathBuf);

impl Home {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "dominos-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// The legacy session file the drop-in client passed as `DOMINOS_SESSION_FILE`.
    pub fn session(&self) -> PathBuf {
        self.path("session.json")
    }

    /// The freeze's `environment`: the store test seam, scratch XDG folders, and the fake.
    pub fn environment(&self, origin: &str) -> Vec<(String, String)> {
        let home = self.0.display().to_string();
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.clone()),
            ("XDG_CONFIG_HOME".into(), format!("{home}/config")),
            ("XDG_DATA_HOME".into(), format!("{home}/data")),
            ("FAMILY_MCP_STORE_TEST_SEAM".into(), "1".into()),
            ("DOMINOS_TEST_ORIGIN".into(), origin.into()),
        ]
    }

    /// `DOMINOS_SESSION_FILE` set to this home's `session.json`.
    pub fn with_session(&self) -> Vec<(String, String)> {
        vec![(
            "DOMINOS_SESSION_FILE".into(),
            self.session().display().to_string(),
        )]
    }

    /// A command for the binary in this home, with nothing inherited from the test's environment.
    pub fn command(&self, origin: &str, extra: &[(String, String)], args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .envs(self.environment(origin))
            .envs(extra.iter().cloned());
        command
    }

    /// `writePrivateFile`: a file with its folders (0700) and mode 0600.
    pub fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// The store options the binary uses in this home: the default record and key file.
    pub fn store_options(&self) -> SecretRecordOptions {
        family_store::enable_test_seam();
        let environment = StoreEnvironment {
            os: std::env::consts::OS.to_owned(),
            home: Some(self.0.clone()),
            xdg_config_home: Some(self.path("config").into_os_string()),
            xdg_data_home: Some(self.path("data").into_os_string()),
            key_backend: None,
            test_seam: true,
        };
        SecretRecordOptions::new(
            default_secret_record_path_in(&environment, "dominos-mcp").unwrap(),
            "dominos-mcp",
            "default",
            "session",
            1,
            key_provider_in(&environment, "dominos-mcp", "default").unwrap(),
            SESSION_MAX_BYTES,
        )
    }

    /// The encrypted session record (tests/ts/scratch.ts `record`).
    pub fn record(&self) -> PathBuf {
        self.path("config/dominos-mcp/session.enc")
    }

    pub fn marker(&self) -> PathBuf {
        self.path("config/dominos-mcp/session.enc.marker")
    }

    /// The store's key file (scratch.ts `key`).
    pub fn key(&self) -> PathBuf {
        self.path("data/family-mcp/keys/dominos-mcp.default.key")
    }

    /// fixtures.ts `current`: the session the store holds now, or the plaintext file while the
    /// store does not decide.
    pub fn current(&self) -> Value {
        let options = self.store_options();
        let mut marker = options.path.clone().into_os_string();
        marker.push(".marker");

        if !options.path.exists() && !Path::new(&marker).exists() {
            return serde_json::from_str(&fs::read_to_string(self.session()).unwrap()).unwrap();
        }
        let text = with_secret_store(&options, |store| store.read())
            .unwrap()
            .expect("no stored session");
        serde_json::from_str(&text).unwrap()
    }

    /// scratch.ts `filesContaining`: every file under the home whose bytes hold one of `needles`.
    pub fn containing(&self, needles: &[&str]) -> Vec<PathBuf> {
        fn walk(directory: &Path, needles: &[&str], found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let info = fs::symlink_metadata(&path).unwrap();

                if info.is_dir() {
                    walk(&path, needles, found);
                } else if info.is_file() {
                    let bytes = fs::read(&path).unwrap();

                    if needles.iter().any(|needle| {
                        bytes
                            .windows(needle.len())
                            .any(|window| window == needle.as_bytes())
                    }) {
                        found.push(path);
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.0, needles, &mut found);
        found
    }

    /// tests/ts/fixtures.ts `saved(expired, prefix)` as `saveSession` writes it: the plaintext file
    /// of an earlier version, expiring in an hour (or a millisecond ago).
    pub fn legacy(&self, expired: bool, prefix: &str) {
        let expires = now() + if expired { -1 } else { 3_600_000 };
        let text = format!(
            "{{\"version\":1,\"accessToken\":\"{prefix}-access\",\"refreshToken\":\"{prefix}-refresh\",\"username\":\"3545550123\",\"expiresAt\":{expires}}}\n"
        );
        self.write("session.json", &text);
    }

    /// tests/ts/fixtures.ts `setup(expired)`: the session saved by an earlier version, then
    /// `auth migrate` (the binary's own, as the drop-in's `migrate`) moves it into the store.
    pub fn seed(&self, expired: bool) {
        self.legacy(expired, "synthetic");
        let run = cli(
            self,
            "http://127.0.0.1:9",
            &self.with_session(),
            &["auth", "migrate"],
            None,
        );
        assert_eq!(run.code, Some(0), "{run:?}");
        assert!(run.stdout.starts_with("Domino’s session moved"), "{run:?}");
    }

    /// What the store holds: `{"store": "none"}`, `{"record": text}` (`null` once logged out), or
    /// `{"error": CODE}`, normalized as the freeze recorded it.
    pub fn stored(&self, names: &mut Names) -> Value {
        let options = self.store_options();
        let mut marker = options.path.clone().into_os_string();
        marker.push(".marker");

        if !options.path.exists() && !Path::new(&marker).exists() {
            return json!({"store": "none"});
        }
        match with_secret_store(&options, |store| store.read()) {
            Ok(Some(record)) => json!({ "record": names.fresh(&record) }),
            Ok(None) => json!({ "record": null }),
            Err(error) => json!({ "error": error.code.as_str() }),
        }
    }

    /// The freeze's `files`: every path under the home with its permission bits, and the text of
    /// `session.json` and of each journal file, normalized and then sorted.
    pub fn files(&self, names: &mut Names) -> Value {
        fn walk(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                paths.push(path.strip_prefix(root).unwrap().to_path_buf());

                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    walk(root, &path, paths);
                }
            }
        }
        let mut raw = Vec::new();
        walk(&self.0, &self.0, &mut raw);
        let before = names.count();
        let mut entries: Vec<(String, Value)> = raw
            .into_iter()
            .map(|relative| {
                let full = self.0.join(&relative);
                let text = relative.display().to_string();
                let info = fs::symlink_metadata(&full).unwrap();
                let mode = format!("{:o}", info.permissions().mode() & 0o777);
                let path = names.fresh(&text);
                let value = if info.file_type().is_symlink() {
                    json!(format!(
                        "{path} -> {}",
                        fs::read_link(&full).unwrap().display()
                    ))
                } else if info.is_dir() {
                    json!(format!("{path}/ {mode}"))
                } else if text == "session.json" || text.starts_with("session.json.checkouts/") {
                    let content = String::from_utf8_lossy(&fs::read(&full).unwrap()).into_owned();
                    json!({"path": format!("{path} {mode}"), "text": names.fresh(&content)})
                } else {
                    json!(format!("{path} {mode}"))
                };
                (path, value)
            })
            .collect();
        assert_eq!(names.count(), before, "a file names an id no output named");
        entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        Value::Array(entries.into_iter().map(|(_, value)| value).collect())
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An epoch-millisecond time as JavaScript's `toISOString`.
pub fn iso(ms: i64) -> String {
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let of_era = z - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// Lays out the frozen TypeScript store state `name` (fixtures/store) in a fresh home: every file
/// with its bytes and permission bits, its folders private. Returns the home and the frozen value.
pub fn plant(name: &str) -> (Home, Value) {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/store")
        .join(format!("{name}.json"));
    let frozen: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let home = Home::new(name);

    for entry in frozen["files"].as_array().unwrap() {
        let path = home.path(entry["path"].as_str().unwrap());
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        let bytes = match entry.get("text") {
            Some(text) => text.as_str().unwrap().as_bytes().to_vec(),
            None => hex(entry["hex"].as_str().unwrap()),
        };
        fs::write(&path, bytes).unwrap();
        let mode = u32::from_str_radix(entry["mode"].as_str().unwrap(), 8).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
    (home, frozen)
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// The first exchange for `path` (query ignored) in the frozen parity case `name`.
pub fn frozen_exchange(name: &str, path: &str) -> Value {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parity")
        .join(format!("{name}.json"));
    let frozen: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    frozen["homes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|home| home["steps"].as_array().unwrap())
        .flat_map(|step| {
            let mut exchanges: Vec<Value> =
                step["exchanges"].as_array().cloned().unwrap_or_default();
            for result in step["results"].as_array().into_iter().flatten() {
                exchanges.extend(result["exchanges"].as_array().unwrap().iter().cloned());
            }
            exchanges
        })
        .find(|exchange| {
            exchange["request"]["path"]
                .as_str()
                .unwrap()
                .split('?')
                .next()
                == Some(path)
        })
        .unwrap_or_else(|| panic!("no frozen exchange for {path} in {name}"))
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The freeze's `Names`, per home: `<home>`; `<uuid-N>` for a random v4 id by first appearance
/// (the parity scripts' fixed `10000000-0000-4000-8000-…` ids keep their text); `<now±Nm>` for an
/// epoch-ms integer and `<iso±Nm>` for an ISO time with milliseconds within a day of now.
pub struct Names {
    home: String,
    seen: Vec<String>,
}

impl Names {
    pub fn new(home: &Home) -> Self {
        Self {
            home: home.0.display().to_string(),
            seen: Vec::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.seen.len()
    }

    pub fn fresh(&mut self, text: &str) -> String {
        let text = text.replace(&self.home, "<home>");
        let text = self.ids(&text);
        let now = now();
        let text = times(&text, now);
        isos(&text, now)
    }

    fn ids(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;

        while !rest.is_empty() {
            if rest.len() >= 36 && rest.is_char_boundary(36) && uuid_v4(&rest[..36]) {
                let id = &rest[..36];

                if id.starts_with("10000000-0000-4000-8000-") {
                    out.push_str(id);
                } else {
                    if !self.seen.iter().any(|seen| seen == id) {
                        self.seen.push(id.to_owned());
                    }
                    let number = self.seen.iter().position(|seen| seen == id).unwrap() + 1;
                    out.push_str(&format!("<uuid-{number}>"));
                }
                rest = &rest[36..];
                continue;
            }
            let next = rest.chars().next().unwrap();
            out.push(next);
            rest = &rest[next.len_utf8()..];
        }
        out
    }
}

/// JavaScript's `Math.round` of whole minutes from now, signed as the freeze wrote it.
fn minutes(time: i64, now: i64) -> String {
    let offset = ((time - now) as f64 / 60_000.0 + 0.5).floor() as i64;
    match offset < 0 {
        true => format!("{offset}m"),
        false => format!("+{offset}m"),
    }
}

/// A run of exactly 13 digits starting with 1, not next to a `.`, within a day of now.
fn times(text: &str, now: i64) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut copied = 0;

    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let start = at;

        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        let before = (start > 0).then(|| bytes[start - 1]);
        let after = bytes.get(at).copied();

        if at - start == 13
            && bytes[start] == b'1'
            && before != Some(b'.')
            && after != Some(b'.')
            && let Ok(time) = text[start..at].parse::<i64>()
            && (time - now).abs() < 86_400_000
        {
            out.push_str(&text[copied..start]);
            out.push_str(&format!("<now{}>", minutes(time, now)));
            copied = at;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` within a day of now.
fn isos(text: &str, now: i64) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while !rest.is_empty() {
        if rest.len() >= 24
            && rest.is_char_boundary(24)
            && let Some(time) = iso_millis(&rest[..24])
        {
            match (time - now).abs() < 86_400_000 {
                true => out.push_str(&format!("<iso{}>", minutes(time, now))),
                false => out.push_str(&rest[..24]),
            }
            rest = &rest[24..];
            continue;
        }
        let next = rest.chars().next().unwrap();
        out.push(next);
        rest = &rest[next.len_utf8()..];
    }
    out
}

fn uuid_v4(text: &str) -> bool {
    let bytes = text.as_bytes();
    let hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, &byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => b"89ab".contains(&byte),
            _ => hex(byte),
        })
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` as milliseconds since the epoch.
fn iso_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:dd.dddZ";

    if bytes.len() != 24
        || !bytes.iter().zip(shape).all(|(&byte, &want)| match want {
            b'd' => byte.is_ascii_digit(),
            _ => byte == want,
        })
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text[range].parse::<i64>().unwrap();
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    // Days from civil (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let of_era_day = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_era_day - 719_468;
    Some(
        ((days * 24 + number(11..13)) * 60 + number(14..16)) * 60_000
            + number(17..19) * 1000
            + number(20..23),
    )
}

/// Fails if `text` holds anything the parity scripts asserted never escapes.
pub fn no_leak(text: &str, at: &str) {
    for leak in LEAKS {
        assert!(!text.contains(leak), "{at}: an output holds {leak}: {text}");
    }
}

/// One request as the fake saw it: the four headers the freeze kept, by lowercase name.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Map<String, Value>,
    pub body: String,
}

impl Request {
    pub fn json(&self) -> Value {
        empty_length(
            json!({"method": self.method, "path": self.path, "headers": self.headers, "body": self.body}),
        )
    }

    /// `METHOD /path` without the query.
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.path.split('?').next().unwrap())
    }
}

/// A request with an empty body has no length to compare: Bun's fetch sends `content-length: 0`
/// for an empty POST, and the binary's client sends none. Any other length is compared.
fn empty_length(mut request: Value) -> Value {
    if request["body"] == "" && request["headers"]["content-length"] == "0" {
        request["headers"]
            .as_object_mut()
            .unwrap()
            .remove("content-length");
    }
    request
}

/// What the fake answers.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json;charset=utf-8".into(),
            )],
            body: body.to_string().into_bytes(),
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// No answer: the connection closes once the request was read, as a drop-in's failed fetch
    /// destroys its socket.
    pub fn dropped() -> Self {
        Self::empty(0)
    }
}

type Handler = Box<dyn Fn(&Request) -> Reply + Send + Sync>;

/// A loopback Domino’s (`/api/`), its website (`/website/`) and Adyen (`/adyen/`). Every request is
/// recorded in arrival order and answered by the handler. Each answer closes its connection.
pub struct Upstream {
    pub origin: String,
    seen: Arc<Mutex<Vec<Request>>>,
    abandoned: Arc<Mutex<Vec<String>>>,
}

impl Upstream {
    pub fn new(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let abandoned = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(Box::new(handler));
        let (record, gone) = (seen.clone(), abandoned.clone());

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (record, gone, handler) = (record.clone(), gone.clone(), handler.clone());
                std::thread::spawn(move || serve(stream, &record, &gone, &handler));
            }
        });
        Self {
            origin,
            seen,
            abandoned,
        }
    }

    /// `METHOD /path` of each request whose client closed its connection before the answer: an
    /// aborted fetch.
    pub fn abandoned(&self) -> Vec<String> {
        self.abandoned.lock().unwrap().clone()
    }

    /// Every request so far, in arrival order.
    pub fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// `METHOD /path` per request, without the query.
    pub fn sent(&self) -> Vec<String> {
        self.seen().iter().map(Request::line).collect()
    }

    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn serve(
    stream: TcpStream,
    seen: &Mutex<Vec<Request>>,
    abandoned: &Mutex<Vec<String>>,
    handler: &Handler,
) {
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();

    while !head.ends_with(b"\r\n\r\n") {
        let mut line = Vec::new();

        if reader.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
            return;
        }
        head.extend(line);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split(' ');
    let method = start.next().unwrap_or_default().to_owned();
    let path = start.next().unwrap_or_default().to_owned();

    // A loopback port scanner's `GET /` is never Domino’s.
    if !["/api/", "/website/", "/adyen/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        let _ = reader
            .get_mut()
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let mut headers = Map::new();
    let mut length = 0;

    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();

        if name == "content-length" {
            length = value.parse().unwrap_or(0);
        }
        if ["authorization", "accept", "content-type", "content-length"].contains(&name.as_str())
            && !headers.contains_key(&name)
        {
            headers.insert(name, Value::String(value));
        }
    }
    let mut body = vec![0; length];

    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request = Request {
        method,
        path,
        headers,
        body: String::from_utf8(body).unwrap(),
    };
    seen.lock().unwrap().push(request.clone());
    let reply = handler(&request);
    // A client that gave up while the answer was prepared has closed its side already.
    let stream = reader.get_mut();
    stream.set_nonblocking(true).unwrap();

    if matches!(stream.read(&mut [0u8; 1]), Ok(0)) {
        abandoned.lock().unwrap().push(request.line());
    }
    stream.set_nonblocking(false).unwrap();

    if reply.status == 0 {
        let _ = reader.get_mut().shutdown(std::net::Shutdown::Both);
        return;
    }
    let mut out = format!("HTTP/1.1 {} Status\r\nConnection: close\r\n", reply.status);

    for (name, value) in &reply.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if !matches!(reply.status, 204 | 304) {
        out.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    out.push_str("\r\n");
    let stream = reader.get_mut();
    // The binary stops reading an oversized answer; a write that fails then is expected.
    let _ = stream
        .write_all(out.as_bytes())
        .and_then(|()| stream.write_all(&reply.body));
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Read to the end, so closing never resets a connection whose answer is still in flight.
    let _ = std::io::copy(reader.get_mut(), &mut std::io::sink());
}

/// A finished CLI run.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The binary's CLI in `home`, with `stdin` (none: an empty, closed input).
pub fn cli(
    home: &Home,
    origin: &str,
    extra: &[(String, String)],
    args: &[&str],
    stdin: Option<&str>,
) -> Run {
    let mut child = home
        .command(origin, extra, args)
        .stdin(match stdin {
            None => Stdio::null(),
            Some(_) => Stdio::piped(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    if let Some(text) = stdin {
        let mut input = child.stdin.take().unwrap();
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = input.write_all(text.as_bytes());
        });
    }
    let output = child.wait_with_output().unwrap();
    Run {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// `dominos-mcp serve` over raw newline-delimited JSON-RPC, one request at a time.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<Vec<u8>>>,
    next: u64,
}

/// The answer to one request, as the line the server wrote, and the notifications before it.
#[derive(Debug, Clone)]
pub struct Answer {
    pub notifications: Vec<String>,
    pub line: String,
}

impl Answer {
    pub fn value(&self) -> Value {
        serde_json::from_str(&self.line).unwrap()
    }

    /// A tool result's structured content, or its error text (the drop-in client's view).
    pub fn outcome(&self) -> Result<Value, String> {
        let value = self.value();
        let result = &value["result"];

        match result["isError"] == true {
            true => Err(result["content"][0]["text"].as_str().unwrap().to_owned()),
            false => Ok(result["structuredContent"].clone()),
        }
    }
}

/// The protocol version and client the freeze sent.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "dominos-parity", "version": "1.0.0"}
    })
}

impl Mcp {
    /// Start the server and initialize it, returning the initialize answer.
    pub fn start(home: &Home, origin: &str, extra: &[(String, String)]) -> (Self, Answer) {
        let mut child = home
            .command(origin, extra, &["serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = channel();

        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };

                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = stderr.clone();

        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];

            while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                sink.lock().unwrap().extend_from_slice(&buffer[..read]);
            }
        });
        let stdin = child.stdin.take();
        let mut mcp = Self {
            child,
            stdin,
            lines,
            stderr,
            next: 0,
        };
        let initialized = mcp.request("initialize", initialize_params());
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        (mcp, initialized)
    }

    /// Write one message without waiting for an answer.
    pub fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send one request and wait for its answer: the next message that carries an id.
    pub fn request(&mut self, method: &str, params: Value) -> Answer {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut notifications = Vec::new();

        loop {
            let line = self
                .lines
                .recv_timeout(PATIENCE)
                .unwrap_or_else(|error| panic!("no answer to {method}: {error}"));

            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(message["jsonrpc"], "2.0", "{line}");

            match message.get("id") {
                None => notifications.push(line),
                Some(answered) => {
                    assert_eq!(answered, &json!(id), "{line}");
                    return Answer {
                        notifications,
                        line,
                    };
                }
            }
        }
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Answer {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// Send a tool call without waiting for its answer; its request id.
    pub fn begin(&mut self, name: &str, arguments: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}}));
        id
    }

    /// The next line the server writes, or `None` once its output closed.
    pub fn next_line(&self) -> Option<String> {
        match self.lines.recv_timeout(PATIENCE) {
            Ok(line) => Some(line),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            Err(error) => panic!("serve wrote nothing: {error}"),
        }
    }

    /// SIGKILL, and wait for the exit.
    pub fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    /// Close standard input and wait for the exit: the exit code and what went to stderr.
    pub fn close(mut self) -> (Option<i32>, String) {
        drop(self.stdin.take());
        let status =
            wait(&mut self.child, PATIENCE).expect("serve did not exit after its input closed");
        let stderr = String::from_utf8(self.stderr.lock().unwrap().clone()).unwrap();
        (status.code(), stderr)
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn wait(child: &mut Child, patience: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + patience;

    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One tool call in its own `serve`, as the drop-in client made each call: the call's outcome.
pub fn call(
    home: &Home,
    origin: &str,
    extra: &[(String, String)],
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let (mut mcp, _) = Mcp::start(home, origin, extra);
    let answer = mcp.call(name, arguments);
    let (code, _) = mcp.close();
    assert_eq!(code, Some(0), "{name}: serve exit");
    answer.outcome()
}

/// The `pay_saved_card` input for `checkout` with its own total, confirmed.
pub fn payment(checkout: &Value) -> Value {
    json!({
        "checkoutId": checkout["checkoutId"],
        "cardId": "card_1",
        "expectedTotal": checkout["total"],
        "confirm": true
    })
}

/// fixtures.ts `setup(expired, legacy)`: a session in a fresh home (migrated into the store unless
/// `legacy`), the provider, and a fake that asks `first` before the provider.
pub struct Fixture {
    pub home: Home,
    pub provider: Provider,
    pub upstream: Upstream,
}

impl Fixture {
    pub fn new(
        first: impl Fn(&Provider, &Request) -> Option<Reply> + Send + Sync + 'static,
    ) -> Self {
        Self::setup(false, false, first)
    }

    pub fn setup(
        expired: bool,
        legacy: bool,
        first: impl Fn(&Provider, &Request) -> Option<Reply> + Send + Sync + 'static,
    ) -> Self {
        let home = Home::new("fixture");

        match legacy {
            true => home.legacy(expired, "synthetic"),
            false => home.seed(expired),
        }
        Self::in_home(home, first)
    }

    /// The provider and its fake for a home already laid out.
    pub fn in_home(
        home: Home,
        first: impl Fn(&Provider, &Request) -> Option<Reply> + Send + Sync + 'static,
    ) -> Self {
        let provider = Provider::default();
        let answer = provider.clone();
        let upstream = Upstream::new(move |request| {
            first(&answer, request).unwrap_or_else(|| answer.answer(request))
        });
        Self {
            home,
            provider,
            upstream,
        }
    }

    pub fn plain() -> Self {
        Self::new(|_, _| None)
    }

    /// One tool call in its own `serve`: its structured result or its error text. No output holds
    /// a secret.
    pub fn call(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let (mut mcp, _) = Mcp::start(&self.home, &self.upstream.origin, &self.home.with_session());
        let answer = mcp.call(name, arguments);
        let (code, stderr) = mcp.close();
        assert_eq!(code, Some(0), "{name}: {stderr}");
        no_leak(&answer.line, name);
        answer.outcome()
    }

    pub fn quote(&self) -> Value {
        self.call("quote_order", cart()).unwrap()
    }

    pub fn create(&self, quote: &Value) -> Result<Value, String> {
        self.call(
            "create_checkout",
            json!({"quoteId": quote["quoteId"], "expectedTotal": quote["total"]}),
        )
    }

    pub fn pay(&self, checkout: &Value) -> Result<Value, String> {
        self.call("pay_saved_card", payment(checkout))
    }

    /// What the fake saw since the last call to `sent`.
    pub fn sent(&self, since: &mut usize) -> Vec<String> {
        let all = self.upstream.sent();
        let new = all[*since..].to_vec();
        *since = all.len();
        new
    }

    pub fn checkouts(&self) -> PathBuf {
        self.home.path("session.json.checkouts")
    }

    /// The journal record of `id`.
    pub fn journal(&self, id: &Value) -> Value {
        let file = self
            .checkouts()
            .join(format!("{}.json", id.as_str().unwrap()));
        serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
    }

    /// The journal folder's entries: one record per quote and checkout, nothing else.
    pub fn left(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.checkouts())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    pub fn records(&self, ids: &[&Value]) -> Vec<String> {
        let mut names: Vec<String> = ids
            .iter()
            .map(|id| format!("{}.json", id.as_str().unwrap()))
            .collect();
        names.sort();
        names
    }
}

/// A frozen value with its packed runs expanded: the freeze wrote each run of 1024 or more of one
/// character inside a string as NUL, the character, `*`, the count, NUL.
pub fn unpack(value: Value) -> Value {
    match value {
        Value::String(text) if text.contains('\0') => {
            let mut out = String::new();
            let mut parts = text.split('\0');
            out.push_str(parts.next().unwrap());

            while let Some(run) = parts.next() {
                let (unit, count) = run.split_once('*').unwrap();
                out.push_str(&unit.repeat(count.parse().unwrap()));
                out.push_str(parts.next().unwrap());
            }
            Value::String(out)
        }
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, item)| (key, unpack(item)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(unpack).collect()),
        other => other,
    }
}

/// Expected exchanges, served in order: each request must equal the next one, or the fake records
/// the difference, answers 599 and the test fails.
#[derive(Clone, Default)]
pub struct Script(Arc<Mutex<ScriptState>>);

#[derive(Default)]
struct ScriptState {
    queue: VecDeque<(Value, Reply)>,
    failures: Vec<String>,
}

impl Script {
    /// A fake that serves this script.
    pub fn upstream(&self) -> Upstream {
        let script = self.clone();
        Upstream::new(move |request| script.answer(request))
    }

    fn answer(&self, request: &Request) -> Reply {
        let mut state = self.0.lock().unwrap();

        match state.queue.pop_front() {
            Some((expected, reply)) if expected == request.json() => reply,
            Some((expected, _)) => {
                state
                    .failures
                    .push(format!("expected {expected}\n     got {}", request.json()));
                Reply::empty(599)
            }
            None => {
                state
                    .failures
                    .push(format!("unexpected request {}", request.json()));
                Reply::empty(599)
            }
        }
    }

    /// Serve `exchanges` (frozen `{request, response}` values) next.
    pub fn expect(&self, exchanges: &Value) {
        let mut state = self.0.lock().unwrap();

        for exchange in exchanges.as_array().unwrap() {
            let response = &exchange["response"];
            let reply = Reply {
                status: response["status"].as_u64().unwrap() as u16,
                headers: response["headers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
                    .collect(),
                body: response["body"].as_str().unwrap().as_bytes().to_vec(),
            };
            state
                .queue
                .push_back((empty_length(exchange["request"].clone()), reply));
        }
    }

    /// Fails unless every expected exchange was served and nothing else was asked.
    pub fn finish(&self, context: &str) {
        let mut state = self.0.lock().unwrap();
        let failures = std::mem::take(&mut state.failures);
        let left = state.queue.len();
        state.queue.clear();
        assert!(failures.is_empty(), "{context}:\n{}", failures.join("\n"));
        assert_eq!(left, 0, "{context}: {left} expected requests never came");
    }
}
