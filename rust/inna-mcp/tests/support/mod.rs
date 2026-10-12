//! What the frozen-fixture replay and the executing tests share: a scratch home laid out as
//! tests/ts/parity.ts laid out its own, a loopback Inna that records every request, a scripted mode
//! that serves recorded exchanges in order, the binary's CLI and a raw JSON-RPC client for `serve`,
//! and the normalizations the TypeScript freeze applied (tooling/ts-freeze/inna-parity.ts). No test
//! here needs Bun.
#![allow(dead_code)]

pub mod browser;
pub mod case;
pub mod fake;
pub mod provider;

use std::collections::VecDeque;
use std::fs::{self, DirBuilder};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use family_store::{
    SecretRecordOptions, StoreEnvironment, default_secret_record_path_in, key_provider_in,
    with_secret_store,
};
use serde_json::{Map, Value, json};

pub const BINARY: &str = env!("CARGO_BIN_EXE_inna-mcp");

/// How long one answer, one exit or one request may take before a test fails instead of hanging.
/// The header that marks a stalled reply; it is never sent.
const STALL: &str = "x-test-stall";

pub const PATIENCE: Duration = Duration::from_secs(60);

/// What no output may hold: what fake-inna.ts marks as never to be returned (the access list's
/// login URL token and national id), and every synthetic cookie value.
pub const LEAKS: [&str; 6] = [
    "DO-NOT-RETURN",
    "9999999999",
    "synthetic-session",
    "synthetic-xsrf",
    "synthetic-rotated",
    "synthetic-switched",
];

/// parity.ts's clock: 2040-01-02T12:00:00Z.
pub const NOW: i64 = 2_209_118_400_000;

/// The renewal the binary may add after a GET answered 401 (parity.ts's one allowance).
pub const RENEWAL: &str = "https://nam.inna.is/api/UserData/GetLoggedInUser";

/// The store's own record bound (src/store.rs `RECORD_MAX_BYTES`).
pub const RECORD_MAX_BYTES: usize = 1_682_444;

/// fake-inna.ts `COMPARED_HEADERS`: what each request carried that both sides must agree on.
pub const COMPARED: [&str; 8] = [
    "accept",
    "authorization",
    "content-type",
    "cookie",
    "origin",
    "referer",
    "x-requested-by",
    "x-xsrf-token",
];

/// A private home for one case, removed on drop. Its path is canonical; its clock file sits
/// beside it, so no listing of the home holds it.
pub struct Home(pub PathBuf);

impl Home {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "inna-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        let home = Self(fs::canonicalize(path).unwrap());
        home.set_clock(NOW);
        home
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// The clock file the binary reads at each use (INNA_TEST_NOW).
    pub fn clock(&self) -> PathBuf {
        let mut path = self.0.clone().into_os_string();
        path.push(".now");
        PathBuf::from(path)
    }

    pub fn set_clock(&self, ms: i64) {
        fs::write(self.clock(), ms.to_string()).unwrap();
    }

    /// parity.ts's `environment`: scratch XDG folders, the store test seam, the fake, the clock.
    pub fn environment(&self, origin: &str) -> Vec<(String, String)> {
        let home = self.0.display().to_string();
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.clone()),
            ("TMPDIR".into(), home.clone()),
            ("XDG_CONFIG_HOME".into(), format!("{home}/.config")),
            ("XDG_DATA_HOME".into(), format!("{home}/.local/share")),
            ("XDG_STATE_HOME".into(), format!("{home}/.local/state")),
            ("XDG_CACHE_HOME".into(), format!("{home}/.cache")),
            ("FAMILY_MCP_STORE_TEST_SEAM".into(), "1".into()),
            ("INNA_TEST_ORIGIN".into(), origin.into()),
            ("INNA_TEST_NOW".into(), self.clock().display().to_string()),
        ]
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

    /// parity.ts's `write`: a file with its folders (0700) and `mode`.
    pub fn write(&self, relative: &str, text: &str, mode: u32) {
        let path = self.path(relative);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    pub fn chmod(&self, relative: &str, mode: u32) {
        fs::set_permissions(self.path(relative), fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The store options the binary uses in this home: the default record and key file.
    pub fn store_options(&self) -> SecretRecordOptions {
        self.store_options_at(&self.0)
    }

    /// The store options with the XDG folders under `root` (`Home::store_env(root)`).
    pub fn store_options_at(&self, root: &Path) -> SecretRecordOptions {
        family_store::enable_test_seam();
        let environment = StoreEnvironment {
            os: std::env::consts::OS.to_owned(),
            home: Some(self.0.clone()),
            xdg_config_home: Some(root.join(".config").into_os_string()),
            xdg_data_home: Some(root.join(".local/share").into_os_string()),
            key_backend: None,
            test_seam: true,
        };
        SecretRecordOptions::new(
            default_secret_record_path_in(&environment, "inna-mcp").unwrap(),
            "inna-mcp",
            "default",
            "session",
            1,
            key_provider_in(&environment, "inna-mcp", "default").unwrap(),
            RECORD_MAX_BYTES,
        )
    }

    /// The freeze's `stored`: `{"store": "none"}`, `{"record": text}` (`null` once logged out), or
    /// `{"error": CODE}`, normalized as the freeze recorded it.
    pub fn stored(&self, names: &Names) -> Value {
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

    /// The decrypted record text as the binary left it (`null` once logged out), if any.
    pub fn record(&self) -> Option<String> {
        with_secret_store(&self.store_options(), |store| store.read()).unwrap()
    }

    /// The record of the store under `root`.
    pub fn record_at(&self, root: &Path) -> Option<String> {
        with_secret_store(&self.store_options_at(root), |store| store.read()).unwrap()
    }

    /// The environment that moves the binary's store under `root` (the drop-ins' `storeAt`).
    pub fn store_env(root: &Path) -> Vec<(String, String)> {
        vec![
            (
                "XDG_CONFIG_HOME".into(),
                root.join(".config").display().to_string(),
            ),
            (
                "XDG_DATA_HOME".into(),
                root.join(".local/share").display().to_string(),
            ),
        ]
    }

    /// integration.test.ts's `resetStored`: a marker of a new generation, a new key and no
    /// record, as a sign-in that replaced a lost key's store leaves them before its write.
    pub fn reset_store(&self) {
        with_secret_store(&self.store_options(), |store| {
            store.reset()?;
            store.create_key()
        })
        .unwrap();
    }

    /// integration.test.ts's `updateStored`: rewrites the decrypted record with `change`.
    pub fn update_record(&self, change: impl FnOnce(String) -> String) {
        let options = self.store_options();
        with_secret_store(&options, |store| {
            let text = store.read()?.expect("a stored record");
            store.write(&change(text))
        })
        .unwrap();
    }

    /// The store marker's generation, when there is a marker.
    pub fn generation(&self) -> Option<u64> {
        let marker = fs::read_to_string(self.path(".config/inna-mcp/session.enc.marker")).ok()?;
        let rest = &marker[marker.find("\"generation\":")? + "\"generation\":".len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        rest[..digits].parse().ok()
    }

    /// Every path under the home, relative, in no particular order.
    fn walk(&self) -> Vec<PathBuf> {
        fn walk(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                paths.push(path.strip_prefix(root).unwrap().to_path_buf());

                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    walk(root, &path, paths);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&self.0, &self.0, &mut paths);
        paths
    }

    /// The freeze's `files`: every path with its permission bits (a directory ends in `/`), in
    /// JavaScript's default sort order.
    pub fn files(&self, names: &Names) -> Value {
        let mut entries: Vec<String> = self
            .walk()
            .into_iter()
            .map(|relative| {
                let info = fs::symlink_metadata(self.0.join(&relative)).unwrap();
                let mode = format!("{:o}", info.permissions().mode() & 0o777);
                let path = names.fresh(&relative.display().to_string());
                match info.is_dir() {
                    true => format!("{path}/ {mode}"),
                    false => format!("{path} {mode}"),
                }
            })
            .collect();
        entries.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        json!(entries)
    }

    /// The freeze's `journals`: the text of each absence record (`<legacy>.absence.json`), by path.
    pub fn journals(&self, names: &Names) -> Value {
        let mut found = Map::new();

        for relative in self.walk() {
            let full = self.0.join(&relative);

            if full.is_file() && relative.display().to_string().ends_with(".absence.json") {
                found.insert(
                    names.fresh(&relative.display().to_string()),
                    json!(names.fresh(&fs::read_to_string(full).unwrap())),
                );
            }
        }
        Value::Object(found)
    }

    /// Replace everything under the home with `entries` (`{path, mode, hex?}`, a folder without
    /// `hex`): what the TypeScript side had written when the freeze reached a `swap` step.
    pub fn plant(&self, entries: &Value) {
        for entry in fs::read_dir(&self.0).unwrap() {
            let path = entry.unwrap().path();

            match fs::symlink_metadata(&path).unwrap().is_dir() {
                true => fs::remove_dir_all(&path).unwrap(),
                false => fs::remove_file(&path).unwrap(),
            }
        }
        let entries = entries.as_array().unwrap();
        let mode = |entry: &Value| entry["mode"].as_u64().unwrap() as u32;

        for entry in entries {
            let path = self.path(entry["path"].as_str().unwrap());

            match entry["hex"].as_str() {
                None => DirBuilder::new()
                    .mode(0o700)
                    .recursive(true)
                    .create(&path)
                    .unwrap(),
                Some(hex) => {
                    fs::write(&path, bytes(hex)).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode(entry))).unwrap();
                }
            }
        }
        // Folders last, deepest first, so a closed folder never stops a write inside it.
        for entry in entries.iter().rev().filter(|entry| entry["hex"].is_null()) {
            let path = self.path(entry["path"].as_str().unwrap());
            fs::set_permissions(&path, fs::Permissions::from_mode(mode(entry))).unwrap();
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // A test may leave a folder it closed; open it again so it can be removed.
        for relative in self.walk() {
            let path = self.0.join(relative);

            if path.is_dir() {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
            }
        }
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_file(self.clock());
    }
}

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect()
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The freeze's `Names`, per home: `<home>`; `<operation-N>` for the Nth operation ID a prepare
/// returned; `<wall-clock>` for an ISO time from 2020 up to 2039 (a cookie's creation or last
/// access); and, in a scenario that plants a 401, `synthetic-rotated-<rotation>` for each
/// rotated cookie value. Shared, so the scripted fake names requests as the replay names outputs.
#[derive(Clone)]
pub struct Names(Arc<Mutex<NamesState>>);

struct NamesState {
    home: String,
    operations: Vec<String>,
    rotation: bool,
}

impl Names {
    pub fn new(home: &Home, rotation: bool) -> Self {
        Self(Arc::new(Mutex::new(NamesState {
            home: home.0.display().to_string(),
            operations: Vec::new(),
            rotation,
        })))
    }

    /// Learns the operation ID of a prepare's answer.
    pub fn learn(&self, answer: &Value) {
        if let Some(id) = answer["result"]["structuredContent"]["operationId"].as_str() {
            let mut state = self.0.lock().unwrap();

            if !state.operations.iter().any(|known| known == id) {
                state.operations.push(id.to_owned());
            }
        }
    }

    /// The operations known so far, replaced by a planted home's.
    pub fn set_operations(&self, operations: Vec<String>) {
        self.0.lock().unwrap().operations = operations;
    }

    /// `<operation>` in a call's arguments: the last prepared operation, as in parity.ts.
    pub fn placed(&self, arguments: &Value) -> Value {
        let state = self.0.lock().unwrap();
        let last = state
            .operations
            .last()
            .cloned()
            .unwrap_or_else(|| "<operation>".into());
        serde_json::from_str(&arguments.to_string().replace("<operation>", &last)).unwrap()
    }

    pub fn fresh(&self, text: &str) -> String {
        let state = self.0.lock().unwrap();
        let mut text = text.replace(&state.home, "<home>");

        for (index, id) in state.operations.iter().enumerate() {
            text = text.replace(id, &format!("<operation-{}>", index + 1));
        }
        let text = wall_clock(&text);

        match state.rotation {
            true => rotations(&text),
            false => text,
        }
    }
}

/// `synthetic-rotated-<digits>` as `synthetic-rotated-<rotation>`.
fn rotations(text: &str) -> String {
    let marker = "synthetic-rotated-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        out.push_str(&rest[..at + marker.len()]);

        match digits {
            0 => {}
            _ => out.push_str("<rotation>"),
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` from 2020 up to 2039 as `<wall-clock>`: a cookie jar's creation and
/// last access times are the wall clock's, not the scenario clock's (2040).
fn wall_clock(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let shape = b"dddd-dd-ddTdd:dd:dd.dddZ";

    while !rest.is_empty() {
        if rest.len() >= 24
            && rest.is_char_boundary(24)
            && rest.as_bytes()[..24]
                .iter()
                .zip(shape)
                .all(|(&byte, &want)| match want {
                    b'd' => byte.is_ascii_digit(),
                    _ => byte == want,
                })
            && ("2020".."2039").contains(&&rest[..4])
        {
            out.push_str("<wall-clock>");
            rest = &rest[24..];
            continue;
        }
        let next = rest.chars().next().unwrap();
        out.push(next);
        rest = &rest[next.len_utf8()..];
    }
    out
}

/// Fails if `text` holds anything fake-inna.ts marks as never returned, or a cookie value.
pub fn no_leak(text: &str, at: &str) {
    for leak in LEAKS {
        assert!(!text.contains(leak), "{at}: an output holds {leak}: {text}");
    }
}

/// One request as the fake saw it: the Inna URL the binary asked for (`https://<host><path>`) and
/// the compared headers (`null` when absent).
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Map<String, Value>,
    pub body: String,
}

impl Request {
    pub fn json(&self) -> Value {
        json!({"method": self.method, "url": self.url, "headers": self.headers, "body": self.body})
    }

    /// `METHOD /path` without the host or the query.
    pub fn line(&self) -> String {
        let path = self.url.trim_start_matches("https://");
        let path = &path[path.find('/').unwrap_or(path.len())..];
        format!("{} {}", self.method, path.split('?').next().unwrap())
    }
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
            headers: vec![("content-type".into(), "application/json".into())],
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

    /// No answer: the connection closes once the request was read, as a failed fetch.
    /// The head of an answer whose body never comes; the request counts as abandoned once the
    /// client closes the connection.
    pub fn stalled(status: u16, content_type: &str) -> Self {
        Self {
            status,
            headers: vec![
                ("content-type".into(), content_type.into()),
                (STALL.into(), String::new()),
            ],
            body: Vec::new(),
        }
    }

    pub fn dropped() -> Self {
        Self::empty(0)
    }

    /// A frozen `{status, headers, body}` response; a header may hold several values.
    pub fn frozen(response: &Value) -> Self {
        let mut headers = Vec::new();

        for (name, value) in response["headers"].as_object().unwrap() {
            match value {
                Value::Array(values) => {
                    for value in values {
                        headers.push((name.clone(), value.as_str().unwrap().to_owned()));
                    }
                }
                value => headers.push((name.clone(), value.as_str().unwrap().to_owned())),
            }
        }
        Self {
            status: response["status"].as_u64().unwrap() as u16,
            headers,
            body: response["body"].as_str().unwrap().as_bytes().to_vec(),
        }
    }
}

type Handler = Box<dyn Fn(&Request) -> Reply + Send + Sync>;

/// A loopback upstream for every host the binary reaches through its test origin
/// (`<origin>/<host><path>`). Every request is recorded in arrival order and answered by the
/// handler. Each answer closes its connection.
pub struct Upstream {
    pub origin: String,
    seen: Arc<Mutex<Vec<Request>>>,
    abandoned: Arc<(Mutex<Vec<String>>, Condvar)>,
}

impl Upstream {
    pub fn new(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let abandoned = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
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

    /// `METHOD /path` of each request whose client closed its connection before the answer.
    pub fn abandoned(&self) -> Vec<String> {
        self.abandoned.0.lock().unwrap().clone()
    }

    /// Waits, at most `patience`, until `count` requests were abandoned; whether they were.
    pub fn until_abandoned(&self, count: usize, patience: Duration) -> bool {
        let (lock, changed) = &*self.abandoned;
        let (_gone, timeout) = changed
            .wait_timeout_while(lock.lock().unwrap(), patience, |gone| gone.len() < count)
            .unwrap();
        !timeout.timed_out()
    }

    /// Every request so far, in arrival order.
    pub fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// `METHOD /path` per request, without the host or the query.
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
    abandoned: &(Mutex<Vec<String>>, Condvar),
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
    let target = start.next().unwrap_or_default().to_owned();
    let host = target
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");

    // A loopback port scanner's `GET /` names no host.
    if !host.contains('.') {
        let _ = reader
            .get_mut()
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let mut found: Vec<(String, String)> = Vec::new();
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
        found.push((name, value));
    }
    // As Node joins a repeated header: `; ` for cookies, `, ` otherwise.
    let headers = COMPARED
        .iter()
        .map(|name| {
            let values: Vec<&str> = found
                .iter()
                .filter(|(found, _)| found == name)
                .map(|(_, value)| value.as_str())
                .collect();
            let joined = match values.is_empty() {
                true => Value::Null,
                false if *name == "cookie" => json!(values.join("; ")),
                false => json!(values.join(", ")),
            };
            ((*name).to_owned(), joined)
        })
        .collect();
    let mut body = vec![0; length];

    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request = Request {
        method,
        url: format!("https://{}", &target[1..]),
        headers,
        body: String::from_utf8(body).unwrap(),
    };
    seen.lock().unwrap().push(request.clone());
    let reply = handler(&request);
    // A client that gave up while the answer was prepared has closed its side already.
    let stream = reader.get_mut();
    stream.set_nonblocking(true).unwrap();

    if matches!(stream.read(&mut [0u8; 1]), Ok(0)) {
        abandoned.0.lock().unwrap().push(request.line());
        abandoned.1.notify_all();
    }
    stream.set_nonblocking(false).unwrap();

    if reply.status == 0 {
        let _ = reader.get_mut().shutdown(std::net::Shutdown::Both);
        return;
    }
    let mut out = format!("HTTP/1.1 {} Status\r\nConnection: close\r\n", reply.status);
    let stalled = reply.headers.iter().any(|(name, _)| name == STALL);

    for (name, value) in reply.headers.iter().filter(|(name, _)| name != STALL) {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    // A stalled answer: its head, then a body that never comes, until the client gives it up.
    if stalled {
        out.push_str("Content-Length: 1048576\r\n\r\n");
        let stream = reader.get_mut();
        let _ = stream
            .write_all(out.as_bytes())
            .and_then(|()| stream.flush());
        let _ = std::io::copy(reader.get_mut(), &mut std::io::sink());
        abandoned.0.lock().unwrap().push(request.line());
        abandoned.1.notify_all();
        return;
    }
    if !matches!(reply.status, 204 | 304) {
        out.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    out.push_str("\r\n");
    let stream = reader.get_mut();
    let _ = stream
        .write_all(out.as_bytes())
        .and_then(|()| stream.write_all(&reply.body));
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Read to the end, so closing never resets a connection whose answer is still in flight.
    let _ = std::io::copy(reader.get_mut(), &mut std::io::sink());
}

/// One expected exchange: the request, its reply, and what parity.ts let the binary add after it.
struct Expected {
    request: Value,
    reply: Reply,
    /// The recovery the binary owes after this request (a GET answered 401 only).
    recovery: Option<Recovery>,
}

/// After a GET answered 401: the request it answered, and the renewal and retry still allowed.
/// The binary's recovery from a recorded 401 on a GET, which the freeze answered from a copy of the
/// fake (tooling/ts-freeze/inna-parity.ts): a renewal, the 401 request's own compared headers and
/// an empty body sent to [`RENEWAL`], then, after a 200, the original request again. Each is
/// compared whole, in that order, like a recorded request.
struct Recovery {
    renewal: Option<(Value, Reply)>,
    retry: Option<(Value, Reply)>,
}

#[derive(Default)]
struct ScriptState {
    queue: VecDeque<Expected>,
    recovery: Option<Recovery>,
    failures: Vec<String>,
    served: usize,
    hops: usize,
    /// The recovery hops the recorded exchanges given to `expect` require.
    required: usize,
}

/// A held reply: the fake has received the request to `path` and waits for `release`.
#[derive(Default)]
struct Gate {
    path: Option<String>,
    reached: bool,
    released: bool,
}

/// Expected exchanges, served in order: each request, named as the replay names outputs, must
/// equal the next one, or the fake records the difference, answers 599 and the test fails. The
/// one allowance is parity.ts's: right after a GET answered 401, the binary may renew
/// (`GET RENEWAL`) and, when that renewed, retry the same URL once; those are answered as a copy of
/// the TypeScript fake's state answered them during the freeze, and counted as hops.
#[derive(Clone)]
pub struct Script {
    state: Arc<Mutex<ScriptState>>,
    gate: Arc<(Mutex<Gate>, Condvar)>,
    names: Names,
}

impl Script {
    pub fn new(names: Names) -> Self {
        Self {
            state: Arc::default(),
            gate: Arc::default(),
            names,
        }
    }

    /// A fake that serves this script.
    pub fn upstream(&self) -> Upstream {
        let script = self.clone();
        Upstream::new(move |request| script.answer(request))
    }

    /// What the fake answers `request`; the upstream's handler, public for the fake's own check.
    pub fn answer(&self, request: &Request) -> Reply {
        let seen: Value =
            serde_json::from_str(&self.names.fresh(&request.json().to_string())).unwrap();
        let reply = {
            let mut state = self.state.lock().unwrap();
            self.decide(&mut state, &seen)
        };
        let (lock, changed) = &*self.gate;
        let mut gate = lock.lock().unwrap();
        let path = request.line();

        if gate.path.as_deref() == path.split_once(' ').map(|(_, path)| path) {
            gate.reached = true;
            changed.notify_all();

            while !gate.released {
                gate = changed.wait(gate).unwrap();
            }
        }
        reply
    }

    fn decide(&self, state: &mut ScriptState, seen: &Value) -> Reply {
        if let Some(recovery) = state.recovery.as_mut() {
            let (hop, retry) = match recovery.renewal.take() {
                Some(renewal) => (renewal, false),
                None => (recovery.retry.take().unwrap(), true),
            };
            let (expected, reply) = hop;

            if &expected != seen {
                state.recovery = None;
                state.failures.push(format!(
                    "expected the recovery's {} {expected}\n     got {seen}",
                    if retry { "retry" } else { "renewal" }
                ));
                return Reply::empty(599);
            }
            // A renewal other than 200 ends the recovery: no retry follows.
            if retry || reply.status != 200 || recovery.retry.is_none() {
                state.recovery = None;
            }
            state.hops += 1;
            return reply;
        }

        match state.queue.pop_front() {
            Some(expected) if &expected.request == seen => {
                state.served += 1;
                state.recovery = expected.recovery;
                expected.reply
            }
            Some(expected) => {
                state
                    .failures
                    .push(format!("expected {}\n     got {seen}", expected.request));
                Reply::empty(599)
            }
            None => {
                state.failures.push(format!("unexpected request {seen}"));
                Reply::empty(599)
            }
        }
    }

    /// Serve `exchanges` (frozen `{request, response, recovery?}` values) next.
    pub fn expect(&self, exchanges: &Value) {
        let mut state = self.state.lock().unwrap();

        for exchange in exchanges.as_array().unwrap() {
            let request = &exchange["request"];
            let recovery = exchange.get("recovery").map(|recovery| {
                let mut renewal = request.clone();
                renewal["method"] = json!("GET");
                renewal["url"] = json!(RENEWAL);
                renewal["body"] = json!("");
                let retry = recovery
                    .get("retry")
                    .map(|retry| (request.clone(), Reply::frozen(retry)));
                state.required += 1 + usize::from(retry.is_some());
                Recovery {
                    renewal: Some((renewal, Reply::frozen(&recovery["renewal"]))),
                    retry,
                }
            });
            state.queue.push_back(Expected {
                request: exchange["request"].clone(),
                reply: Reply::frozen(&exchange["response"]),
                recovery,
            });
        }
    }

    /// Hold the reply to the next request for `path` (no host, no query) until `release`.
    pub fn hold(&self, path: &str) {
        let (lock, _) = &*self.gate;
        *lock.lock().unwrap() = Gate {
            path: Some(path.to_owned()),
            ..Gate::default()
        };
    }

    /// Wait until the held request has reached the fake.
    pub fn reached(&self, at: &str) {
        let (lock, changed) = &*self.gate;
        let gate = lock.lock().unwrap();
        let (gate, timeout) = changed
            .wait_timeout_while(gate, PATIENCE, |gate| !gate.reached)
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "{at}: the held request never came: {:?}",
            gate.path
        );
    }

    pub fn release(&self) {
        let (lock, changed) = &*self.gate;
        let mut gate = lock.lock().unwrap();
        gate.released = true;
        gate.path = None;
        changed.notify_all();
    }

    /// Fails unless every expected exchange was served and nothing else was asked.
    pub fn finish(&self, context: &str) {
        let mut state = self.state.lock().unwrap();
        let failures = std::mem::take(&mut state.failures);
        let left = state.queue.len();
        state.queue.clear();
        let pending = state.recovery.take().is_some();
        assert!(failures.is_empty(), "{context}:\n{}", failures.join("\n"));
        assert!(!pending, "{context}: a recovery hop never came");
        assert_eq!(left, 0, "{context}: {left} expected requests never came");
    }

    /// Recorded exchanges served, and recovery hops the binary added.
    pub fn served(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.served, state.hops)
    }

    /// The recovery hops the recorded exchanges required.
    pub fn required_hops(&self) -> usize {
        self.state.lock().unwrap().required
    }
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

/// `inna-mcp serve` over raw newline-delimited JSON-RPC.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    /// Set by `deafen`: the reader closes stdout after the next line.
    deaf: Arc<AtomicBool>,
    stderr: Arc<Stream>,
    next: u64,
}

/// What a child wrote on stderr so far, and whether it has ended, with a wait on both.
#[derive(Default)]
struct Stream {
    state: Mutex<(Vec<u8>, bool)>,
    changed: Condvar,
}

impl Stream {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.state.lock().unwrap().0).into_owned()
    }

    fn until(&self, patience: Duration, mut ready: impl FnMut(&str, bool) -> bool) -> bool {
        let state = self.state.lock().unwrap();
        let (_state, timeout) = self
            .changed
            .wait_timeout_while(state, patience, |(bytes, ended)| {
                !ready(&String::from_utf8_lossy(bytes), *ended)
            })
            .unwrap();
        !timeout.timed_out()
    }
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

    /// A tool result's structured content, or its error text.
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
        "clientInfo": {"name": "inna-parity", "version": "1.0.0"}
    })
}

/// How a process ended, as Bun's `exited` and `signalCode` report it.
pub fn ended(status: ExitStatus) -> Value {
    match (status.code(), status.signal()) {
        (Some(code), _) => json!({"exited": code, "signal": null}),
        (None, Some(signal)) => json!({"exited": 128 + signal, "signal": signal_name(signal)}),
        (None, None) => unreachable!(),
    }
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => "other",
    }
}

impl Mcp {
    /// Start `serve` with `args` and initialize it, returning the initialize answer.
    pub fn start(
        home: &Home,
        origin: &str,
        extra: &[(String, String)],
        args: &[&str],
    ) -> (Self, Answer) {
        let mut mcp = Self::spawn(home, origin, extra, args);
        mcp.next = 0;
        let initialized = mcp.request("initialize", initialize_params());
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        (mcp, initialized)
    }

    /// Start `serve` with `args`, uninitialized; its requests are numbered from 1.
    pub fn spawn(home: &Home, origin: &str, extra: &[(String, String)], args: &[&str]) -> Self {
        let mut all = vec!["serve"];
        all.extend_from_slice(args);
        let mut child = home
            .command(origin, extra, &all)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = channel();
        let deaf = Arc::new(AtomicBool::new(false));
        let deafened = deaf.clone();

        // The reader is dropped, closing stdout's read side, before `send` is: a disconnected
        // `lines` means stdout is closed.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };

                if send.send(line).is_err() || deafened.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
        let stderr = Arc::new(Stream::default());
        let mut pipe = child.stderr.take().unwrap();
        let sink = stderr.clone();

        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];

            while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                sink.state
                    .lock()
                    .unwrap()
                    .0
                    .extend_from_slice(&buffer[..read]);
                sink.changed.notify_all();
            }
            sink.state.lock().unwrap().1 = true;
            sink.changed.notify_all();
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
            deaf,
            stderr,
            next: 1,
        }
    }

    /// Write one message without waiting for an answer.
    pub fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send a request without waiting for its answer; its id.
    pub fn begin(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// The next message that carries an id, with the notifications before it.
    pub fn answer(&mut self, context: &str) -> Answer {
        let mut notifications = Vec::new();

        loop {
            let line = self
                .lines
                .recv_timeout(PATIENCE)
                .unwrap_or_else(|error| panic!("no answer to {context}: {error}"));

            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(message["jsonrpc"], "2.0", "{line}");

            match message.get("id") {
                None => notifications.push(line),
                Some(_) => {
                    return Answer {
                        notifications,
                        line,
                    };
                }
            }
        }
    }

    /// Send one request and wait for its answer, which must carry its id.
    pub fn request(&mut self, method: &str, params: Value) -> Answer {
        let id = self.begin(method, params);
        let answer = self.answer(method);
        assert_eq!(answer.value()["id"], json!(id), "{}", answer.line);
        answer
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Answer {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// Close standard input without waiting.
    /// The host closes its read side of stdout after one more answer, as a broken pipe: the
    /// server's next write fails. Standard input stays open.
    pub fn deafen(&mut self) {
        self.deaf.store(true, Ordering::SeqCst);
        let answer = self.request("ping", json!({})).value();
        assert!(answer["result"].is_object(), "{answer}");
        assert!(
            matches!(
                self.lines.recv_timeout(PATIENCE),
                Err(RecvTimeoutError::Disconnected)
            ),
            "stdout was not closed"
        );
    }

    /// Write `bytes` to standard input on a thread of its own, so a server that stops reading
    /// cannot block the test; joining it gives standard input back, still open.
    pub fn write_input(&mut self, bytes: Vec<u8>) -> std::thread::JoinHandle<ChildStdin> {
        let mut stdin = self.stdin.take().unwrap();

        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes).and_then(|()| stdin.flush());
            stdin
        })
    }

    pub fn end_input(&mut self) {
        drop(self.stdin.take());
    }

    /// Send `signal` (`INT`, `TERM`, `USR1`, ...) to the server.
    pub fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -{signal}");
    }

    /// Wait for the exit (standard input closed first): how it ended and what went to stderr.
    pub fn finish(mut self) -> (ExitStatus, String) {
        drop(self.stdin.take());
        let status =
            wait(&mut self.child, PATIENCE).expect("serve did not exit after its input closed");
        // All of stderr: the reader may still be draining the pipe.
        self.stderr.until(PATIENCE, |_, ended| ended);
        (status, self.stderr.text())
    }

    /// The exit within `patience`, if it came.
    pub fn exit_within(&mut self, patience: Duration) -> Option<ExitStatus> {
        wait(&mut self.child, patience)
    }

    /// Wait for the exit with standard input still open (a signal's exit, not end of input's).
    pub fn exit(&mut self) -> ExitStatus {
        wait(&mut self.child, PATIENCE).expect("serve did not exit with its input open")
    }

    /// Close standard input and wait for the exit: the exit code and what went to stderr.
    pub fn close(self) -> (Option<i32>, String) {
        let (status, stderr) = self.finish();
        (status.code(), stderr)
    }

    /// Every further line the server wrote, once its output has closed.
    pub fn rest(&self) -> Vec<String> {
        self.lines.try_iter().collect()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// What the server wrote on stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr.text()
    }

    /// Waits, at most `patience`, until stderr holds `text` at least `times` times; whether it did.
    /// Waits for `times` whole lines holding `text`. Unbuffered stderr may write a line in
    /// pieces, so a line counts once its newline is there.
    pub fn until_stderr(&self, text: &str, times: usize, patience: Duration) -> bool {
        self.stderr.until(patience, |written, _| {
            whole_lines(written)
                .filter(|line| line.contains(text))
                .count()
                >= times
        })
    }

    /// The keep-alive statuses the server reported (INNA_TEST_KEEP_ALIVE), in order, from whole
    /// lines only.
    pub fn statuses(&self) -> Vec<String> {
        whole_lines(&self.stderr())
            .filter_map(|line| line.strip_prefix("keep-alive: "))
            .map(str::to_owned)
            .collect()
    }
}

/// Whether process `pid` waits for the lock on `locked`: its `<file>.lock-tmp.<pid>-<id>`
/// temporary (family-store/src/lock.rs) sits beside the file.
pub fn waits_for_lock(locked: &Path, pid: u32) -> bool {
    let prefix = format!(
        "{}.lock-tmp.{pid}-",
        locked.file_name().unwrap().to_string_lossy()
    );
    fs::read_dir(locked.parent().unwrap())
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or(false)
}

/// The lines of `text` its newline has ended, without the newline.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn wait(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
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

/// A frozen value with its packed runs expanded: the freeze wrote each run of 1024 or more of one
/// character inside a string as U+0001, the character, `*`, the count, U+0001.
pub fn unpack(value: Value) -> Value {
    match value {
        Value::String(text) if text.contains('\u{1}') => {
            let mut out = String::new();
            let mut parts = text.split('\u{1}');
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
