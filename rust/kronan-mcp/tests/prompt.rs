//! `auth set` on a terminal: the hidden prompt of this binary and of the TypeScript CLI, typed
//! the same keystrokes on a pseudo-terminal, read the same token (sent to a local fake `/me/`),
//! print the same text, exit the same way and leave the terminal mode as they found it, also when
//! SIGTERM or SIGINT ends them in the middle of the prompt. A terminal that hangs up has no mode
//! left on Linux, so there that one case checks the hang-up instead (see `typed`).
//! `FAMILY_MCP_BUN` must name a Bun 1.4.2 executable: this test fails without it and is never
//! skipped. It needs the `test-origin` feature, without which the binary would talk to Krónan.
#![cfg(feature = "test-origin")]

mod scratch;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

/// A fake Krónan answering every request with an account, recording each Authorization header.
fn upstream() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut byte = [0u8; 1];

            while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                request.push(byte[0]);
            }
            let text = String::from_utf8_lossy(&request).into_owned();

            if let Some(line) = text
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            {
                record.lock().unwrap().push(line[14..].trim().to_owned());
            }
            let body = r#"{"type":"user","name":"Terminal"}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, seen)
}

/// A pseudo-terminal: the controlling side and the terminal the child reads.
fn terminal() -> (OwnedFd, OwnedFd) {
    let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    // A child that inherited the controlling side would keep the terminal from hanging up.
    fcntl_setfd(&controller, FdFlags::CLOEXEC).unwrap();
    grantpt(&controller).unwrap();
    unlockpt(&controller).unwrap();
    let name = ptsname(&controller, Vec::new()).unwrap();
    let path = Path::new(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let terminal = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    (controller, terminal.into())
}

/// How a case ends after its keys are typed.
#[derive(Debug, Clone, Copy)]
enum End {
    /// The keys finish the prompt.
    Typed,
    /// The controlling side closes: the terminal hangs up.
    HangUp,
    /// Another process sends this signal.
    Signal(&'static str),
}

#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    signal: Option<i32>,
    stdout: String,
    stderr: String,
    sent: Vec<String>,
    /// Whether the terminal ended in the mode it started in; `None` when it hung up on Linux.
    restored: Option<bool>,
}

/// Run `auth set` on a fresh terminal, type `keys` once it is in raw mode, then end it as `end`
/// says.
fn typed(command: &mut Command, keys: &[u8], end: End) -> Outcome {
    let (origin, seen) = upstream();
    let (controller, terminal) = terminal();
    // Start from a mode no fresh terminal has, so a mode the kernel reset to its defaults never
    // passes for one the prompt restored. Neither prompt changes ECHOCTL.
    let mut marked = tcgetattr(&terminal).unwrap();
    marked.local_modes.toggle(LocalModes::ECHOCTL);
    tcsetattr(&terminal, OptionalActions::Now, &marked).unwrap();
    let before = tcgetattr(&terminal).unwrap();
    let child = command
        .env("KRONAN_TEST_ORIGIN", &origin)
        .stdin(Stdio::from(terminal.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Typed keys reach the prompt only once it switched the terminal to raw mode.
    let deadline = Instant::now() + Duration::from_secs(20);

    while tcgetattr(&terminal)
        .unwrap()
        .local_modes
        .contains(LocalModes::ICANON)
    {
        assert!(Instant::now() < deadline, "the prompt never read raw input");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut writer = std::fs::File::from(controller.try_clone().unwrap());
    writer.write_all(keys).unwrap();

    match end {
        End::Typed => {}
        End::HangUp => {
            drop(writer);
            drop(controller);
        }
        End::Signal(name) => {
            // The typed bytes are read before the signal, so the prompt is mid-line.
            std::thread::sleep(Duration::from_millis(200));
            let sent = Command::new("kill")
                .arg(format!("-{name}"))
                .arg(child.id().to_string())
                .status()
                .unwrap();
            assert!(sent.success());
        }
    }
    let output = child.wait_with_output().unwrap();
    // The kernel may set PENDIN when a terminal returns to canonical mode (macOS always does,
    // until the next read); it is not part of the mode the prompt saved.
    let restored = match tcgetattr(&terminal) {
        Ok(after) => Some(
            after.local_modes - LocalModes::PENDIN == before.local_modes
                && after.input_modes == before.input_modes,
        ),
        // When the controlling side's last descriptor closes, Linux hangs the terminal up: it
        // resets the mode to the pty driver's defaults, and every terminal call on any of its
        // descriptors, the child's included, fails with EIO. No descriptor is left that could show
        // a restored mode, so on Linux this case checks the hang-up itself. macOS keeps the mode
        // the prompt left, and there it is checked.
        Err(Errno::IO) if cfg!(target_os = "linux") && matches!(end, End::HangUp) => None,
        Err(error) => panic!("{end:?}: cannot read the terminal mode: {error}"),
    };

    Outcome {
        code: output.status.code(),
        signal: output.status.signal(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        sent: seen.lock().unwrap().clone(),
        restored,
    }
}

/// The keystrokes typed and how each case ends.
fn cases() -> [(&'static [u8], End); 9] {
    [
        (b"  synthetic-token-0123456789x\x7f\r", End::Typed),
        (b"synthetic-token-0123456789\x08\x086789\n", End::Typed),
        // Backspace removes one UTF-16 unit: one leaves half of the emoji, two remove it.
        ("synthetic-token-0123456789😀\x7f\r".as_bytes(), End::Typed),
        (
            "synthetic-token-0123456789😀\x7f\x7f\r".as_bytes(),
            End::Typed,
        ),
        (b"synthetic-tok\x03en-0123456789\r", End::Typed),
        (b"\x04", End::Typed),
        (b"synthetic-token-0123456789", End::HangUp),
        (b"synthetic-tok", End::Signal("TERM")),
        (b"synthetic-tok", End::Signal("INT")),
    ]
}

/// One case on a fresh terminal: the TypeScript CLI under `bun`, or this binary without it.
fn run(bun: Option<&std::ffi::OsStr>, keys: &[u8], end: End) -> Outcome {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = scratch::Scratch::new(match bun {
        Some(_) => "prompt-ts",
        None => "prompt-rust",
    });
    let mut command = match bun {
        Some(bun) => {
            let mut command = Command::new(bun);
            command
                .arg("--preload")
                .arg(manifest.join("tests/ts/rewrite.ts"))
                .arg(manifest.join("../../packages/kronan-mcp/src/cli.ts"))
                .env("BUN_RUNTIME_TRANSPILER_CACHE_PATH", "0");
            command
        }
        None => Command::new(env!("CARGO_BIN_EXE_kronan-mcp")),
    };
    command.args(["auth", "set"]);
    scratch.isolate(&mut command);
    typed(&mut command, keys, end)
}

/// The TypeScript outcome checks every TypeScript run gets; the frozen outcomes passed them.
fn check_typescript(ts: &Outcome, keys: &[u8], end: End) {
    let hung_up = cfg!(target_os = "linux") && matches!(end, End::HangUp);
    assert_eq!(ts.restored, (!hung_up).then_some(true), "{keys:?}: {ts:?}");
}

/// Bun dies of the signal; the binary keeps its handler installed, so it exits with the status a
/// shell reports for that death (128 + the signal number).
fn as_typescript_ends(mut rust: Outcome, ts: &Outcome, end: End) -> Outcome {
    if let (End::Signal(_), Some(signal)) = (end, ts.signal) {
        assert_eq!(rust.code, Some(128 + signal), "{end:?}: {rust:?}");
        (rust.code, rust.signal) = (None, Some(signal));
    }
    rust
}

#[test]
fn the_hidden_prompt_reads_keystrokes_like_the_typescript_cli() {
    let bun = std::env::var_os("FAMILY_MCP_BUN")
        .expect("FAMILY_MCP_BUN must name a Bun 1.4.2 executable; parity tests never skip");

    for (keys, end) in cases() {
        let ts = run(Some(&bun), keys, end);
        check_typescript(&ts, keys, end);
        let rust = as_typescript_ends(run(None, keys, end), &ts, end);
        assert_eq!(rust, ts, "{keys:?} {end:?}");
    }
}

impl End {
    fn name(self) -> String {
        match self {
            End::Typed => "typed".into(),
            End::HangUp => "hang-up".into(),
            End::Signal(name) => format!("signal:{name}"),
        }
    }
}

impl Outcome {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "signal": self.signal,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "sent": self.sent,
            "restored": self.restored,
        })
    }

    fn from_json(value: &serde_json::Value) -> Self {
        let text = |key: &str| value[key].as_str().unwrap().to_owned();
        let number = |key: &str| {
            value[key]
                .as_i64()
                .map(|number| i32::try_from(number).unwrap())
        };
        Self {
            code: number("code"),
            signal: number("signal"),
            stdout: text("stdout"),
            stderr: text("stderr"),
            sent: value["sent"]
                .as_array()
                .unwrap()
                .iter()
                .map(|token| token.as_str().unwrap().to_owned())
                .collect(),
            restored: value["restored"].as_bool(),
        }
    }
}

/// The frozen TypeScript outcomes of this platform (fixtures/pty, PROVENANCE.md there).
fn frozen_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pty")
        .join(format!("{}.json", std::env::consts::OS))
}

const FROZEN_ID: &str = "kronan/pty/the-hidden-prompt-reads-keystrokes-like-the-typescript-cli";

/// Writes the TypeScript CLI's outcome of every case on this platform to `KRONAN_PTY_FREEZE`. Run
/// once with Bun 1.4.2 at the commit PROVENANCE.md names; PR 3 deletes it with the TypeScript CLI.
#[test]
#[ignore = "freezes the TypeScript outcomes; needs FAMILY_MCP_BUN and KRONAN_PTY_FREEZE"]
fn freeze_the_typescript_prompt_outcomes() {
    let bun = std::env::var_os("FAMILY_MCP_BUN").expect("FAMILY_MCP_BUN must name Bun 1.4.2");
    let output = std::env::var_os("KRONAN_PTY_FREEZE").expect("KRONAN_PTY_FREEZE names the file");
    let cases: Vec<serde_json::Value> = cases()
        .into_iter()
        .map(|(keys, end)| {
            let ts = run(Some(&bun), keys, end);
            check_typescript(&ts, keys, end);
            serde_json::json!({
                "keys": String::from_utf8_lossy(keys),
                "end": end.name(),
                "outcome": ts.json(),
            })
        })
        .collect();
    let frozen = serde_json::json!({
        "id": FROZEN_ID,
        "platform": std::env::consts::OS,
        "cases": cases,
    });
    std::fs::write(
        output,
        format!("{}\n", serde_json::to_string_pretty(&frozen).unwrap()),
    )
    .unwrap();
}

#[test]
fn the_hidden_prompt_reads_keystrokes_like_the_frozen_typescript_cli() {
    let path = frozen_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no frozen TypeScript outcomes at {}: {error}",
            path.display()
        )
    });
    let frozen: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(frozen["id"], FROZEN_ID);
    assert_eq!(frozen["platform"], std::env::consts::OS);
    let recorded = frozen["cases"].as_array().unwrap();
    assert_eq!(recorded.len(), cases().len());

    for ((keys, end), case) in cases().into_iter().zip(recorded) {
        assert_eq!(case["keys"], *String::from_utf8_lossy(keys), "{keys:?}");
        assert_eq!(case["end"], end.name(), "{keys:?}");
        let ts = Outcome::from_json(&case["outcome"]);
        check_typescript(&ts, keys, end);
        let rust = as_typescript_ends(run(None, keys, end), &ts, end);
        assert_eq!(rust, ts, "{keys:?} {end:?}");
    }
}
