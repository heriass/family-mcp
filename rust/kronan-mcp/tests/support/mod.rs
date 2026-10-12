//! What the frozen-fixture replay and the executing tests share: a scratch home laid out as
//! tests/ts/parity.ts laid out each scenario's, a loopback Krónan that records every request, the
//! binary's CLI and a raw JSON-RPC client for `serve`, and the normalizations the TypeScript freeze
//! applied (tooling/ts-freeze/kronan-parity.ts). No test here needs Bun.
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

pub const BINARY: &str = env!("CARGO_BIN_EXE_kronan-mcp");

/// How long one answer, one exit or one request may take before a test fails instead of hanging.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// A private home for one scenario, removed on drop.
pub struct Home(pub PathBuf);

impl Home {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kronan-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// parity.ts `environment`: the store test seam, scratch XDG directories, and the fake.
    pub fn environment(&self, origin: &str) -> Vec<(String, String)> {
        let home = self.0.display().to_string();
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.clone()),
            ("XDG_CONFIG_HOME".into(), format!("{home}/.config")),
            ("XDG_DATA_HOME".into(), format!("{home}/.local/share")),
            ("XDG_STATE_HOME".into(), format!("{home}/.local/state")),
            ("XDG_CACHE_HOME".into(), format!("{home}/.cache")),
            ("FAMILY_MCP_STORE_TEST_SEAM".into(), "1".into()),
            ("KRONAN_TEST_ORIGIN".into(), origin.into()),
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

    /// parity.ts `write`: a file with its folders (0700) and the mode given (default 0600).
    pub fn write(&self, relative: &str, text: &str, mode: Option<u32>) {
        let path = self.path(relative);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode.unwrap_or(0o600))).unwrap();
    }

    /// The store options the binary uses in this home: the default record and key file.
    pub fn store_options(&self) -> SecretRecordOptions {
        family_store::enable_test_seam();
        let environment = StoreEnvironment {
            os: std::env::consts::OS.to_owned(),
            home: Some(self.0.clone()),
            xdg_config_home: Some(self.path(".config").into_os_string()),
            xdg_data_home: Some(self.path(".local/share").into_os_string()),
            key_backend: None,
            test_seam: true,
        };
        SecretRecordOptions::new(
            default_secret_record_path_in(&environment, "kronan-mcp").unwrap(),
            "kronan-mcp",
            "default",
            "token",
            1,
            key_provider_in(&environment, "kronan-mcp", "default").unwrap(),
            16_384,
        )
    }

    /// parity.ts `save`: the record `{version: 1, token}` written through the store, with the key
    /// the store creates.
    pub fn save(&self, token: Option<&str>) {
        with_secret_store(&self.store_options(), |store| {
            if !store.exists()? {
                store.create_key()?;
            }
            store.write(&json!({"version": 1, "token": token}).to_string())
        })
        .unwrap();
    }

    /// What the store holds: `{"store": "none"}`, `{"record": text}` (`null` once logged out), or
    /// `{"error": CODE}`, as the freeze recorded it.
    pub fn stored(&self) -> Value {
        let options = self.store_options();
        let mut marker = options.path.clone().into_os_string();
        marker.push(".marker");

        if !options.path.exists() && !Path::new(&marker).exists() {
            return json!({"store": "none"});
        }
        match with_secret_store(&options, |store| store.read()) {
            Ok(record) => json!({ "record": record }),
            Err(error) => json!({ "error": error.code.as_str() }),
        }
    }

    /// parity.ts `files`: `.config/kronan-mcp` sorted, `.json` files with their normalized text.
    pub fn files(&self) -> Value {
        let directory = self.path(".config/kronan-mcp");

        let Ok(entries) = fs::read_dir(&directory) else {
            return json!([]);
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        names
            .into_iter()
            .map(|name| match name.ends_with(".json") {
                true => {
                    let text = fs::read_to_string(directory.join(&name)).unwrap();
                    json!({"name": name, "text": fresh(&text)})
                }
                false => json!(name),
            })
            .collect()
    }

    /// The freeze's `tree`: every path under the home, sorted, with its permission bits; a
    /// directory ends in `/`, a symbolic link names its target instead.
    pub fn tree(&self) -> Value {
        fn walk(root: &Path, directory: &Path, paths: &mut Vec<String>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                let info = fs::symlink_metadata(&path).unwrap();
                let mode = format!("{:o}", info.permissions().mode() & 0o777);

                if info.file_type().is_symlink() {
                    let target = fs::read_link(&path).unwrap();
                    paths.push(format!("{relative} -> {}", target.display()));
                } else if info.is_dir() {
                    paths.push(format!("{relative}/ {mode}"));
                    walk(root, &path, paths);
                } else {
                    paths.push(format!("{relative} {mode}"));
                }
            }
        }
        let mut paths = Vec::new();
        walk(&self.0, &self.0, &mut paths);
        paths.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        json!(paths)
    }

    /// parity.ts `local`: printed paths name `<home>`.
    pub fn local(&self, text: &str) -> String {
        let real = fs::canonicalize(&self.0).unwrap().display().to_string();
        text.replace(&real, "<home>")
            .replace(&self.0.display().to_string(), "<home>")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One request as the fake saw it: the four headers parity.ts kept, by lowercase name.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Map<String, Value>,
    pub body: String,
}

impl Request {
    pub fn json(&self) -> Value {
        json!({"method": self.method, "path": self.path, "headers": self.headers, "body": self.body})
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

    /// A text body with no content type, as `new Response(text, { status })`.
    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// No answer: the connection closes once the request was read, as a drop-in's failed fetch
    /// (a network error or a timeout) destroys its socket.
    pub fn dropped() -> Self {
        Self::empty(0)
    }
}

/// tests/ts/fixtures.ts `fixtureResponse(path)`: what the TypeScript freeze served for `path`
/// (query ignored), from the frozen published-paths contract case.
pub fn fixture(path: &str) -> Reply {
    static ANSWERS: std::sync::OnceLock<Vec<(String, Reply)>> = std::sync::OnceLock::new();
    let answers = ANSWERS.get_or_init(|| {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contract/published-paths-and-bodies.json");
        let frozen: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        let mut answers = Vec::new();

        for step in frozen["steps"].as_array().unwrap() {
            for result in step["results"].as_array().into_iter().flatten() {
                for exchange in result["exchanges"].as_array().unwrap() {
                    let path = exchange["request"]["path"].as_str().unwrap();
                    let path = path.split('?').next().unwrap().to_owned();
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
                    answers.push((path, reply));
                }
            }
        }
        answers
    });
    let path = path.split('?').next().unwrap();
    answers
        .iter()
        .find(|(frozen, _)| frozen == path)
        .map(|(_, reply)| reply.clone())
        .unwrap_or_else(|| panic!("no frozen answer for {path}"))
}

/// A frozen TypeScript store state (fixtures/store, PROVENANCE.md there).
pub fn store_state(name: &str) -> Value {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/store")
        .join(format!("{name}.json"));
    let state: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(state["id"], format!("kronan/store/{name}"));
    state
}

/// Writes every file of the frozen state `name` into `home`, with its mode.
pub fn plant(home: &Home, name: &str) {
    for file in store_state(name)["files"].as_array().unwrap() {
        let path = home.path(file["path"].as_str().unwrap());
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        let bytes = match file["text"].as_str() {
            Some(text) => text.as_bytes().to_vec(),
            None => {
                let hex = file["hex"].as_str().unwrap();
                (0..hex.len())
                    .step_by(2)
                    .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                    .collect()
            }
        };
        fs::write(&path, bytes).unwrap();
        let mode = u32::from_str_radix(file["mode"].as_str().unwrap(), 8).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// The JSON body of `fixture(path)`.
pub fn fixture_json(path: &str) -> Value {
    serde_json::from_slice(&fixture(path).body).unwrap()
}

type Handler = Box<dyn Fn(&Request) -> Reply + Send + Sync>;

/// A loopback Krónan. Every request is recorded in arrival order and answered by the handler.
/// Each answer closes its connection, so the binary never reuses one the fake already closed.
pub struct Upstream {
    pub origin: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl Upstream {
    pub fn new(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(Box::new(handler));
        let record = seen.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (record, handler) = (record.clone(), handler.clone());
                std::thread::spawn(move || serve(stream, &record, &handler));
            }
        });
        Self { origin, seen }
    }

    /// Every request so far, in arrival order.
    pub fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// The integration cases' `sent`: `METHOD /path` per request, without `/api/v1` or the query.
    pub fn sent(&self) -> Vec<String> {
        self.seen()
            .iter()
            .map(|request| {
                let path = request.path.split('?').next().unwrap();
                format!("{} {}", request.method, path.trim_start_matches("/api/v1"))
            })
            .collect()
    }

    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn serve(stream: TcpStream, seen: &Mutex<Vec<Request>>, handler: &Handler) {
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

    // A loopback port scanner's `GET /` is never Krónan's.
    if !path.starts_with("/api/v1/") {
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

/// A finished CLI run, printed paths already shown as `<home>`.
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
        // A large input is written beside the reads, so a child that stops reading never blocks it.
        std::thread::spawn(move || {
            let _ = input.write_all(text.as_bytes());
        });
    }
    let output = child.wait_with_output().unwrap();
    Run {
        code: output.status.code(),
        stdout: home.local(&String::from_utf8(output.stdout).unwrap()),
        stderr: home.local(&String::from_utf8(output.stderr).unwrap()),
    }
}

/// `kronan-mcp serve` over raw newline-delimited JSON-RPC, one request at a time.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<Vec<u8>>>,
    next: u64,
}

/// The answer to one request and the notifications that came before it.
#[derive(Debug, Clone)]
pub struct Answer {
    pub notifications: Vec<Value>,
    /// `{"result": ...}` or `{"error": ...}`.
    pub answer: Value,
}

/// The protocol version and client the freeze sent.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "kronan-parity", "version": "1.0.0"}
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
            let mut message: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            let object = message.as_object_mut().unwrap();

            let Some(answered) = object.remove("id") else {
                notifications.push(message);
                continue;
            };
            assert_eq!(answered, json!(id), "{line}");
            object.remove("jsonrpc");
            return Answer {
                notifications,
                answer: message,
            };
        }
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Answer {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
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

    /// Wait for the exit without closing standard input.
    pub fn wait(mut self, patience: Duration) -> Option<std::process::ExitStatus> {
        wait(&mut self.child, patience)
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

/// parity.ts `fresh`: v4 attempt ids are `<uuid>`, and ISO times within a minute of now `<now>`.
pub fn fresh(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("\"id\":\"")
            && after.len() > 36
            && uuid_v4(&after[..36])
            && after.as_bytes()[36] == b'"'
        {
            out.push_str("\"id\":\"<uuid>\"");
            rest = &after[37..];
            continue;
        }
        if rest.len() >= 24
            && rest.is_char_boundary(24)
            && let Some(time) = iso_millis(&rest[..24])
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            match (time - now).abs() < 60_000 {
                true => out.push_str("<now>"),
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

    /// Serve `exchanges` (frozen `{request, response}` values) next, with `<origin>` as `origin`.
    pub fn expect(&self, exchanges: &Value, origin: &str) {
        let mut state = self.0.lock().unwrap();

        for exchange in exchanges.as_array().unwrap() {
            let response = &exchange["response"];
            let local = |text: &str| text.replace("<origin>", origin);
            let reply = Reply {
                status: response["status"].as_u64().unwrap() as u16,
                headers: response["headers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, value)| (name.clone(), local(value.as_str().unwrap())))
                    .collect(),
                body: local(response["body"].as_str().unwrap()).into_bytes(),
            };
            state.queue.push_back((exchange["request"].clone(), reply));
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
