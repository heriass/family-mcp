//! `auth login` on a terminal: the hidden prompt of this binary and of the TypeScript CLI, typed
//! the same keystrokes on a pseudo-terminal, read the same token (sent to a local fake `/me/`),
//! print the same text, exit the same way and leave the terminal mode as they found it, also when
//! SIGTERM or SIGINT ends them in the middle of the prompt. A terminal that hangs up has no mode
//! left on Linux, and there each side may meet the hang-up as a failed read or as the input's
//! end, so that one case checks each side against both outcomes instead (see `typed` and the case
//! loop).
//! `FAMILY_MCP_BUN` must name a Bun 1.4.2 executable: this test fails without it and is never
//! skipped. It needs the `test-origin` feature, without which the binary would talk to Domino’s.
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

/// A fake Domino’s answering every request with an account, recording each Authorization header.
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
            let path = text
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .nth(1)
                .unwrap_or_default();
            let size = text
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|s| s.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut content = vec![0; size];
            let _ = stream.read_exact(&mut content);
            record
                .lock()
                .unwrap()
                .push(format!("{path} {}", String::from_utf8_lossy(&content)));
            let body = if path == "/api/token" {
                r#"{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","token_type":"bearer","username":"3545550123","expires_in":3600}"#
            } else {
                r#"{"id":1}"#
            };
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

#[derive(Debug, PartialEq, Clone)]
struct Outcome {
    code: Option<i32>,
    signal: Option<i32>,
    stdout: String,
    stderr: String,
    sent: Vec<String>,
    /// Whether the terminal ended in the mode it started in; `None` when it hung up on Linux.
    restored: Option<bool>,
}

/// Run `auth login` on a fresh terminal, type `keys` once it is in raw mode, then end it as `end`
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
    let mut child = command
        .env("DOMINOS_TEST_ORIGIN", &origin)
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
    assert!(
        !tcgetattr(&terminal)
            .unwrap()
            .local_modes
            .contains(LocalModes::ECHO),
        "The hidden prompt must not echo input."
    );
    // Raw mode can be entered before readline's async iterator is installed. Wait for the
    // visible prompt too, so keys are typed only once the user could type them.
    let mut prompt = vec![0u8; "Icelandic phone number (input hidden): ".len()];
    child
        .stderr
        .as_mut()
        .unwrap()
        .read_exact(&mut prompt)
        .unwrap();
    let mut writer = std::fs::File::from(controller.try_clone().unwrap());
    // Type each answer at its own prompt, as an interactive login does. Readline can process
    // editing keys for an already queued second line before its first async iterator consumer.
    let split = keys
        .iter()
        .position(|byte| matches!(byte, b'\r' | b'\n'))
        .map(|at| at + 1)
        .filter(|at| *at < keys.len());
    if let Some(split) = split {
        if keys.starts_with(&[0xef, 0xbb, 0xbf]) {
            writer.write_all(&keys[..1]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            writer.write_all(&keys[1..split]).unwrap();
        } else {
            for byte in &keys[..split] {
                writer.write_all(&[*byte]).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while seen.lock().unwrap().is_empty() {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{keys:?}: SMS request never reached the fake"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            for byte in &keys[split..] {
                writer.write_all(&[*byte]).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    } else {
        writer.write_all(keys).unwrap();
    }

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
        stderr: format!(
            "{}{}",
            String::from_utf8_lossy(&prompt),
            String::from_utf8_lossy(&output.stderr)
        ),
        sent: seen.lock().unwrap().clone(),
        restored,
    }
}

#[test]
fn the_hidden_prompt_reads_keystrokes_like_the_typescript_cli() {
    let bun = std::env::var_os("FAMILY_MCP_BUN")
        .expect("FAMILY_MCP_BUN must name a Bun 1.4.2 executable; parity tests never skip");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cases: [(&[u8], End); 15] = [
        (b"5550123\r123456\r", End::Typed),
        ("\u{feff}5550123\r123456\r".as_bytes(), End::Typed),
        (b"+354 555-0123x\x7f\r123456x\x7f\r", End::Typed),
        (b"bad\x155550123\r12x\x083456\r", End::Typed),
        (b"5550123\r\x1b[A123456\r", End::Typed),
        (b"bad\r", End::Typed),
        (b"555013\x1b[D2\r123456\r", End::Typed),
        (b"x5550123\x01\x1b[3~\x05\r123456\r", End::Typed),
        (b"5550123\r12345x\x1b[D\x04\x056\r", End::Typed),
        (b"555\x03", End::Typed),
        (b"\x04", End::Typed),
        (b"555", End::HangUp),
        (b"555", End::Signal("TERM")),
        (b"555", End::Signal("INT")),
        (b"5550123\r12", End::Signal("TERM")),
    ];

    for (keys, end) in cases {
        let run = |side: &str| {
            let scratch = scratch::Scratch::new(&format!("prompt-{side}"));
            let mut command = match side {
                "ts" => {
                    let mut command = Command::new(&bun);
                    command
                        .arg("--preload")
                        .arg(manifest.join("tests/ts/rewrite.ts"))
                        .arg(manifest.join("../../packages/dominos-mcp/src/cli.ts"))
                        .env("BUN_RUNTIME_TRANSPILER_CACHE_PATH", "0");
                    command
                }
                _ => Command::new(env!("CARGO_BIN_EXE_dominos-mcp")),
            };
            command.args(["auth", "login"]);
            scratch.isolate(&mut command);
            typed(&mut command, keys, end)
        };
        let ts = run("ts");

        // When the controlling side closes, Linux hangs the terminal up. A read already waiting in
        // the terminal driver fails (EIO), the generic failure; a read that starts after the
        // hang-up ends the input, which cancels. Which one a prompt meets depends on whether it is
        // back in its read when the terminal hangs up, which the test cannot see for Bun, so on
        // Linux each side is checked against both outcomes and not against the other. macOS ends
        // the input either way, and there the two sides must match.
        if cfg!(target_os = "linux") && matches!(end, End::HangUp) {
            let after_prompt = |text: &str| Outcome {
                code: Some(1),
                signal: None,
                stdout: String::new(),
                stderr: format!("Icelandic phone number (input hidden): {text}"),
                sent: Vec::new(),
                restored: None,
            };
            let outcomes = [
                after_prompt("Domino’s MCP failed. Check the local configuration.\n"),
                after_prompt("Sign-in cancelled.\n"),
            ];
            assert!(outcomes.contains(&ts), "{keys:?}: {ts:?}");
            let rust = run("rust");
            assert!(outcomes.contains(&rust), "{keys:?}: {rust:?}");
            continue;
        }
        assert_eq!(ts.restored, Some(true), "{keys:?}: {ts:?}");
        let mut rust = run("rust");
        let interpretation = if keys == b"5550123\r\x1b[A123456\r" {
            Some("history")
        } else if keys == b"555013\x1b[D2\r123456\r" {
            Some("left")
        } else if keys == b"5550123\r12345x\x1b[D\x04\x056\r" {
            Some("code-delete")
        } else if keys.iter().any(|byte| [0x7f, 0x08, 0x15].contains(byte))
            || keys == b"x5550123\x01\x1b[3~\x05\r123456\r"
        {
            Some("phone-edit")
        } else {
            None
        };
        if let Some(interpretation) = interpretation {
            check_keystrokes(&rust, &ts, interpretation);
            continue;
        }

        // Bun dies of the signal; the binary keeps its handler installed, so it exits with the
        // status a shell reports for that death (128 + the signal number).
        if let (End::Signal(_), Some(signal)) = (end, ts.signal) {
            assert_eq!(rust.code, Some(128 + signal), "{end:?}: {rust:?}");
            (rust.code, rust.signal) = (None, Some(signal));
        }
        assert_eq!(rust, ts, "{keys:?} {end:?}");
    }
}

// taskr decisions 76087/76089/76096/76141: the unchanged Bun pty reference can ignore editing
// (strict serial and TTY-warmup logs in the report). Rust stays strict; each listed TS case
// allows only its documented result or the exact plain-input result, requests and restored mode.
fn check_keystrokes(rust: &Outcome, ts: &Outcome, interpretation: &str) {
    let success = |phone: &str| Outcome {
        code: Some(0),
        signal: None,
        stdout: "Signed in. Session saved encrypted.\n".into(),
        stderr:
            "Icelandic phone number (input hidden): \nSMS sent. Six-digit code (input hidden): \n"
                .into(),
        sent: vec![
            format!("/api/login/sendPin?phoneNumber={phone} "),
            format!(
                "/api/token grant_type=password&username={phone}&password=123456&authentication_type=sms"
            ),
            "bearer synthetic-access".into(),
            "/api/user/newuser ".into(),
        ],
        restored: Some(true),
    };
    let code_refusal=Outcome {
        code:Some(1),signal:None,stdout:String::new(),
        stderr:"Icelandic phone number (input hidden): \nSMS sent. Six-digit code (input hidden): The SMS code must contain six digits.\n".into(),
        sent:vec!["/api/login/sendPin?phoneNumber=3545550123 ".into()],restored:Some(true),
    };
    let phone_refusal=Outcome {
        code:Some(1),signal:None,stdout:String::new(),
        stderr:"Icelandic phone number (input hidden): Use a seven-digit Icelandic phone number, optionally prefixed with +354.\n".into(),
        sent:Vec::new(),restored:Some(true),
    };
    let documented = if interpretation == "history" {
        code_refusal.clone()
    } else {
        success("3545550123")
    };
    let alternate = match interpretation {
        "history" => success("3545550123"),
        "left" => success("3545550132"),
        "code-delete" => Outcome {
            stderr: "Icelandic phone number (input hidden): \nSMS sent. Six-digit code (input hidden): Sign-in cancelled.\n".into(),
            ..code_refusal
        },
        _ => phone_refusal,
    };
    assert_eq!(rust, &documented);
    assert!(
        ts == &documented || ts == &alternate,
        "{interpretation}: {ts:?}"
    );
}
