//! `auth login` on a terminal: the hidden phone prompt of this binary and of the TypeScript CLI,
//! typed the same keystrokes on a pseudo-terminal, read the same number (the sign-in's first
//! request goes to a local fake that refuses it), print the same text, exit the same way and leave
//! the terminal mode as they found it, also when signals arrive in the middle of the prompt: the
//! first SIGINT cancels the sign-in once the line ends, a second one ends the process, and
//! SIGTERM ends it at once; only the binary also restores the terminal after a second SIGINT
//! (`raw_left`). A terminal that hangs up has no mode left on Linux, and there each side may
//! meet the hang-up as a failed read or as the input's end, so that one case checks each side
//! against both outcomes instead (see `typed` and the case loop). Adapted from
//! rust/kronan-mcp/tests/prompt.rs.
//! The live cases need `FAMILY_MCP_BUN` to name a Bun 1.4.2 executable: they fail without it and
//! are never skipped. Their `_frozen_` twins need no Bun: they compare the binary with the
//! TypeScript outcomes frozen per platform in tests/fixtures/pty (PROVENANCE.md there). Every
//! case needs the `test-origin` feature, without which the binary would talk to Inna.
#![cfg(feature = "test-origin")]

mod scratch;

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rustix::io::{Errno, FdFlags, fcntl_setfd, ioctl_fionbio};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

/// A fake upstream refusing every request with 404, recording each request line.
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
            record
                .lock()
                .unwrap()
                .push(text.lines().next().unwrap_or_default().to_owned());
            let _ = write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
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

/// One case: keys typed once the prompt reads raw input, then signals from another process, then
/// more keys, and the terminal hung up at the end or not.
#[derive(Debug, Clone, Copy)]
struct Case {
    keys: &'static [u8],
    signals: &'static [&'static str],
    after: &'static [u8],
    hang_up: bool,
}

const fn keys(keys: &'static [u8]) -> Case {
    Case {
        keys,
        signals: &[],
        after: b"",
        hang_up: false,
    }
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

/// Kills a child still running at its deadline, so a case that hangs fails in time instead of
/// blocking the test on a read or a wait. A dropped watchdog (the test panicked) kills at once.
struct Watchdog {
    stop: mpsc::Sender<()>,
    fired: std::thread::JoinHandle<bool>,
}

impl Watchdog {
    const BOUND: Duration = Duration::from_secs(60);

    fn start(child: &std::process::Child) -> Self {
        let (stop, stopped) = mpsc::channel();
        let pid = child.id().to_string();
        let fired = std::thread::spawn(move || {
            let fired = stopped.recv_timeout(Self::BOUND);

            if fired.is_err() {
                let _ = Command::new("kill").args(["-KILL", &pid]).status();
            }
            fired == Err(mpsc::RecvTimeoutError::Timeout)
        });
        Self { stop, fired }
    }

    /// Ends the watch once the child has exited; panics if the deadline killed it.
    fn stop(self, side: &str) {
        let _ = self.stop.send(());
        assert!(
            !self.fired.join().unwrap(),
            "{side}: still running after {:?}, killed",
            Self::BOUND
        );
    }
}

/// Run `auth login` on a fresh terminal and play `case` on it.
fn typed(command: &mut Command, case: Case) -> Outcome {
    let (origin, seen) = upstream();
    let (controller, terminal) = terminal();
    // Start from a mode no fresh terminal has, so a mode the kernel reset to its defaults never
    // passes for one the prompt restored. Neither prompt changes ECHOCTL.
    let mut marked = tcgetattr(&terminal).unwrap();
    marked.local_modes.toggle(LocalModes::ECHOCTL);
    tcsetattr(&terminal, OptionalActions::Now, &marked).unwrap();
    let before = tcgetattr(&terminal).unwrap();
    let child = command
        .env("INNA_TEST_ORIGIN", &origin)
        .stdin(Stdio::from(terminal.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let watchdog = Watchdog::start(&child);
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
    writer.write_all(case.keys).unwrap();

    for name in case.signals {
        // The typed bytes are read before the signal, so the prompt is mid-line.
        std::thread::sleep(Duration::from_millis(200));
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
    }

    if !case.after.is_empty() {
        std::thread::sleep(Duration::from_millis(200));
        writer.write_all(case.after).unwrap();
    }

    if case.hang_up {
        drop(writer);
        drop(controller);
    }
    let output = child.wait_with_output().unwrap();
    watchdog.stop(&format!("{case:?}"));
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
        Err(Errno::IO) if cfg!(target_os = "linux") && case.hang_up => None,
        Err(error) => panic!("{case:?}: cannot read the terminal mode: {error}"),
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

fn cases() -> [Case; 10] {
    [
        keys(b" 5550000 \r"),
        keys(b"55x\x7f50000\n"),
        // Ctrl-D does nothing on a line with text, and closes the input on an empty one.
        keys(b"5\x04550000\r"),
        keys(b"5\x7f\x04"),
        // Ctrl-C cancels.
        keys(b"555\x03"),
        keys(b"123\r5550000\r"),
        Case {
            hang_up: true,
            ..keys(b"5550000")
        },
        Case {
            signals: &["TERM"],
            ..keys(b"555")
        },
        // The first SIGINT cancels the sign-in once the line ends; the second ends the process.
        Case {
            signals: &["INT"],
            after: b"0000\r",
            ..keys(b"555")
        },
        Case {
            signals: &["INT", "INT"],
            ..keys(b"555")
        },
    ]
}

/// `auth login` of the TypeScript CLI under `bun`, or of this binary without it, in a scratch home.
fn command(bun: Option<&OsStr>, name: &str) -> (scratch::Scratch, Command) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let side = match bun {
        Some(_) => "ts",
        None => "rust",
    };
    let scratch = scratch::Scratch::new(&format!("{name}-{side}"));
    let mut command = match bun {
        Some(bun) => {
            let mut command = Command::new(bun);
            command
                .arg("--preload")
                .arg(manifest.join("tests/ts/rewrite.ts"))
                .arg(manifest.join("../../packages/inna-mcp/src/cli.ts"))
                .env("BUN_RUNTIME_TRANSPILER_CACHE_PATH", "0");
            command
        }
        None => Command::new(env!("CARGO_BIN_EXE_inna-mcp")),
    };
    command.args(["auth", "login"]);
    scratch.isolate(&mut command);
    (scratch, command)
}

fn run(bun: Option<&OsStr>, case: Case) -> Outcome {
    let (_scratch, mut command) = command(bun, "prompt");
    typed(&mut command, case)
}

/// When the controlling side closes, Linux hangs the terminal up. A read already waiting in the
/// terminal driver fails (EIO), which readline reports as an error, the generic failure; a read
/// that starts after the hang-up ends the input, which cancels. Which one a prompt meets depends on
/// whether it is back in its read when the terminal hangs up, which the test cannot see for Bun, so
/// on Linux each side is checked against both outcomes and not against the other. The binary maps
/// each read result as readline does (terminal::tests). macOS ends the input either way, and there
/// both sides cancel.
fn linux_hang_up_outcomes() -> [Outcome; 2] {
    let after_prompt = |text: &str| Outcome {
        code: Some(1),
        signal: None,
        stdout: String::new(),
        stderr: format!("Icelandic phone number (input hidden): {text}"),
        sent: Vec::new(),
        restored: None,
    };
    [
        after_prompt(
            "Inna MCP failed. Check input format, file permissions, and local configuration.\n",
        ),
        after_prompt("\nInna login cancelled.\n"),
    ]
}

fn linux_hang_up(case: Case) -> bool {
    cfg!(target_os = "linux") && case.hang_up
}

/// Rust: a deliberate difference. Bun dies of the second SIGINT with the terminal still in raw
/// mode; the binary gives the terminal its mode back first.
fn raw_left(case: Case) -> bool {
    case.signals == ["INT", "INT"]
}

/// The checks every TypeScript run gets; the frozen outcomes passed them when they were frozen.
fn check_typescript(ts: &Outcome, case: Case) {
    if linux_hang_up(case) {
        assert!(linux_hang_up_outcomes().contains(ts), "{case:?}: {ts:?}");
        return;
    }

    if case.hang_up {
        assert_eq!(
            (ts.code, ts.stderr.as_str()),
            (
                Some(1),
                "Icelandic phone number (input hidden): \nInna login cancelled.\n"
            ),
            "{case:?}"
        );
    }
    assert_eq!(ts.restored, Some(!raw_left(case)), "{case:?}: {ts:?}");
}

/// Compares this binary's outcome of a case with the TypeScript one, live or frozen.
fn compare(mut rust: Outcome, ts: &Outcome, case: Case) {
    if linux_hang_up(case) {
        assert!(
            linux_hang_up_outcomes().contains(&rust),
            "{case:?}: {rust:?}"
        );
        return;
    }

    if (case.keys == b"55x\x7f50000\n" || case.keys == b"5\x04550000\r")
        && case.signals.is_empty()
        && case.after.is_empty()
        && !case.hang_up
    {
        check_keystrokes(&rust, ts, case.keys);
        return;
    }

    if raw_left(case) {
        assert_eq!(rust.restored, Some(true), "{case:?}: {rust:?}");
        rust.restored = ts.restored;
    }

    // Bun dies of the signal; the binary keeps its handler installed, so it exits with the status
    // a shell reports for that death (128 + the signal number).
    if let (true, Some(signal)) = (!case.signals.is_empty(), ts.signal) {
        assert_eq!(rust.code, Some(128 + signal), "{case:?}: {rust:?}");
        (rust.code, rust.signal) = (None, Some(signal));
    }
    assert_eq!(rust, *ts, "{case:?}");
}

fn bun() -> std::ffi::OsString {
    std::env::var_os("FAMILY_MCP_BUN")
        .expect("FAMILY_MCP_BUN must name a Bun 1.4.2 executable; parity tests never skip")
}

#[test]
fn the_hidden_phone_prompt_reads_keystrokes_like_the_typescript_cli() {
    let bun = bun();

    for case in cases() {
        let ts = run(Some(&bun), case);
        check_typescript(&ts, case);
        compare(run(None, case), &ts, case);
    }
}

// taskr 76066/76588/76640/76644: Bun may ignore DEL or cancel on nonempty-line Ctrl-D on the pty.
// Rust stays strict; input-close, signal, hang-up and non-editing cases keep exact parity.
fn check_keystrokes(rust: &Outcome, ts: &Outcome, keys: &[u8]) {
    let documented = Outcome {
        code: Some(1),
        signal: None,
        stdout: String::new(),
        stderr: "Icelandic phone number (input hidden): \nInna electronic-ID login failed or expired. Check your phone and start a fresh explicit login.\n".into(),
        sent: vec!["GET /r.inna.is/auth/island HTTP/1.1".into()],
        restored: Some(true),
    };
    let plain_input = Outcome {
        code: Some(1),
        signal: None,
        stdout: String::new(),
        stderr: if keys == b"5\x04550000\r" {
            "Icelandic phone number (input hidden): \nInna login cancelled.\n"
        } else {
            "Icelandic phone number (input hidden): \nEnter a seven-digit Icelandic phone number.\n"
        }
        .into(),
        sent: Vec::new(),
        restored: Some(true),
    };
    assert_eq!(rust, &documented);
    assert!(ts == &documented || ts == &plain_input, "{keys:?}: {ts:?}");
}

/// What a key typed as the prompt shows leads to: the exit code, standard output and error after
/// the filler, the requests, and whether the terminal mode was restored.
#[derive(Debug, PartialEq)]
struct Ordered {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    sent: Vec<String>,
    restored: bool,
}

/// The prompt shows only once the terminal reads raw keys, in both CLIs: cli.ts creates its
/// readline interface, which sets raw mode, before it writes the prompt. Standard error is a pipe
/// filled to capacity before the start, so the prompt's write waits until the test reads it, and
/// the terminal must already be raw while it waits. The number is typed the moment the prompt has
/// been read; it is read whole and never echoed. Those are checked here, on every run.
fn ordered(bun: Option<&OsStr>) -> Ordered {
    const PROMPT: &[u8] = b"Icelandic phone number (input hidden): ";
    let side = if bun.is_some() { "ts" } else { "rust" };
    let (_scratch, mut command) = command(bun, "prompt-order");
    let (origin, seen) = upstream();
    let (controller, terminal) = terminal();
    let mut marked = tcgetattr(&terminal).unwrap();
    marked.local_modes.toggle(LocalModes::ECHOCTL);
    tcsetattr(&terminal, OptionalActions::Now, &marked).unwrap();
    let before = tcgetattr(&terminal).unwrap();
    let (mut errors, full) = std::io::pipe().unwrap();
    // Fill the pipe, so the child's first write to standard error waits for the test.
    ioctl_fionbio(&full, true).unwrap();
    let mut filled = 0;

    for size in [4096, 1] {
        loop {
            match (&full).write(&vec![b'.'; size]) {
                Ok(written) => filled += written,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("cannot fill the pipe: {error}"),
            }
        }
    }
    ioctl_fionbio(&full, false).unwrap();
    let child = command
        .env("INNA_TEST_ORIGIN", &origin)
        .stdin(Stdio::from(terminal.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::from(OwnedFd::from(full)))
        .spawn()
        .unwrap();
    let watchdog = Watchdog::start(&child);
    // The child holds the pipe's only writing end now, so it ends when the child does.
    drop(command);
    let raw = || {
        !tcgetattr(&terminal)
            .unwrap()
            .local_modes
            .intersects(LocalModes::ICANON | LocalModes::ECHO)
    };
    let deadline = Instant::now() + Duration::from_secs(20);

    while !raw() {
        assert!(
            Instant::now() < deadline,
            "{side}: the prompt was written before the terminal read raw keys"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stderr = Vec::new();
    let mut chunk = [0u8; 4096];

    while stderr.len() < filled || !stderr[filled..].ends_with(PROMPT) {
        let read = errors.read(&mut chunk).unwrap();
        assert!(
            read > 0,
            "{side}: standard error ended before the prompt (killed after {:?} if it hung)",
            Watchdog::BOUND
        );
        stderr.extend_from_slice(&chunk[..read]);
    }
    assert!(raw(), "{side}");
    std::fs::File::from(controller.try_clone().unwrap())
        .write_all(b"5550000\r")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    watchdog.stop(side);
    errors.read_to_end(&mut stderr).unwrap();
    // Whatever the terminal echoed would wait on the controlling side.
    ioctl_fionbio(&controller, true).unwrap();
    // A clone: closing the controlling side would hang the terminal up before it is read.
    let echoed = std::fs::File::from(controller.try_clone().unwrap()).read(&mut chunk);
    assert!(
        matches!(&echoed, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "{side}: {echoed:?}"
    );
    let after = tcgetattr(&terminal).unwrap();
    Ordered {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr[filled..]).into_owned(),
        sent: seen.lock().unwrap().clone(),
        restored: after.local_modes - LocalModes::PENDIN == before.local_modes
            && after.input_modes == before.input_modes,
    }
}

fn expected_order() -> Ordered {
    Ordered {
        code: Some(1),
        stdout: String::new(),
        stderr: "Icelandic phone number (input hidden): \nInna electronic-ID login failed or expired. Check your phone and start a fresh explicit login.\n".to_owned(),
        sent: vec!["GET /r.inna.is/auth/island HTTP/1.1".to_owned()],
        restored: true,
    }
}

#[test]
fn a_key_typed_as_the_prompt_shows_is_read_raw_and_not_echoed() {
    let bun = bun();

    for side in [Some(bun.as_os_str()), None] {
        assert_eq!(ordered(side), expected_order(), "{side:?}");
    }
}

impl Case {
    fn json(self) -> serde_json::Value {
        serde_json::json!({
            "keys": String::from_utf8_lossy(self.keys),
            "signals": self.signals,
            "after": String::from_utf8_lossy(self.after),
            "hangUp": self.hang_up,
        })
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
            sent: strings(&value["sent"]),
            restored: value["restored"].as_bool(),
        }
    }
}

impl Ordered {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "sent": self.sent,
            "restored": self.restored,
        })
    }

    fn from_json(value: &serde_json::Value) -> Self {
        Self {
            code: value["code"]
                .as_i64()
                .map(|number| i32::try_from(number).unwrap()),
            stdout: value["stdout"].as_str().unwrap().to_owned(),
            stderr: value["stderr"].as_str().unwrap().to_owned(),
            sent: strings(&value["sent"]),
            restored: value["restored"].as_bool().unwrap(),
        }
    }
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// The frozen TypeScript outcomes of this platform (fixtures/pty, PROVENANCE.md there).
fn frozen() -> serde_json::Value {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pty")
        .join(format!("{}.json", std::env::consts::OS));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no frozen TypeScript outcomes at {}: {error}",
            path.display()
        )
    });
    let frozen: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(frozen["platform"], std::env::consts::OS);
    frozen
}

const FROZEN_ID: &str = "inna/pty/the-hidden-phone-prompt-reads-keystrokes-like-the-typescript-cli";
const ORDER_ID: &str = "inna/pty/a-key-typed-as-the-prompt-shows-is-read-raw-and-not-echoed";

/// Writes the TypeScript CLI's outcome of every case on this platform to `INNA_PTY_FREEZE`. Run
/// once with Bun 1.4.2 at the commit PROVENANCE.md names; PR 3 deletes it with the TypeScript CLI.
#[test]
#[ignore = "freezes the TypeScript outcomes; needs FAMILY_MCP_BUN and INNA_PTY_FREEZE"]
fn freeze_the_typescript_prompt_outcomes() {
    let bun = std::env::var_os("FAMILY_MCP_BUN").expect("FAMILY_MCP_BUN must name Bun 1.4.2");
    let output = std::env::var_os("INNA_PTY_FREEZE").expect("INNA_PTY_FREEZE names the file");
    let cases: Vec<serde_json::Value> = cases()
        .into_iter()
        .map(|case| {
            let ts = run(Some(&bun), case);
            check_typescript(&ts, case);
            let mut frozen = serde_json::json!({"case": case.json(), "outcome": ts.json()});

            // On Linux a hang-up has two outcomes; the pair is frozen with the one Bun met.
            if linux_hang_up(case) {
                frozen["allowed"] = linux_hang_up_outcomes().iter().map(Outcome::json).collect();
            }
            frozen
        })
        .collect();
    let order = ordered(Some(&bun));
    assert_eq!(order, expected_order());
    let frozen = serde_json::json!({
        "id": FROZEN_ID,
        "platform": std::env::consts::OS,
        "cases": cases,
        "order": {"id": ORDER_ID, "outcome": order.json()},
    });
    std::fs::write(
        output,
        format!("{}\n", serde_json::to_string_pretty(&frozen).unwrap()),
    )
    .unwrap();
}

#[test]
fn the_hidden_phone_prompt_reads_keystrokes_like_the_frozen_typescript_cli() {
    let frozen = frozen();
    assert_eq!(frozen["id"], FROZEN_ID);
    let recorded = frozen["cases"].as_array().unwrap();
    assert_eq!(recorded.len(), cases().len());

    for (case, recorded) in cases().into_iter().zip(recorded) {
        assert_eq!(recorded["case"], case.json(), "{case:?}");
        let ts = Outcome::from_json(&recorded["outcome"]);
        check_typescript(&ts, case);

        if let Some(allowed) = recorded.get("allowed") {
            let allowed: Vec<Outcome> = allowed
                .as_array()
                .unwrap()
                .iter()
                .map(Outcome::from_json)
                .collect();
            assert_eq!(allowed, linux_hang_up_outcomes(), "{case:?}");
        }
        compare(run(None, case), &ts, case);
    }
}

#[test]
fn a_key_typed_as_the_prompt_shows_is_read_raw_like_the_frozen_typescript_cli() {
    let frozen = frozen();
    assert_eq!(frozen["order"]["id"], ORDER_ID);
    let ts = Ordered::from_json(&frozen["order"]["outcome"]);
    assert_eq!(ts, expected_order());
    assert_eq!(ordered(None), ts);
}
