//! The example server (examples/stdio.rs) over pipes, for the stdio lifetime tests. Each test
//! binary uses part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// How long a test waits for a line it expects before it fails.
const PATIENCE: Duration = Duration::from_secs(15);

/// `cargo test` builds the examples next to the test binaries' `deps` directory; a run limited
/// to `--test signals` does not rebuild it.
pub fn example() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    let path = deps
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/stdio");
    assert!(
        path.exists(),
        "build the example first: cargo build -p mcp-runtime --example stdio"
    );
    path
}

pub fn send(stdin: &mut ChildStdin, message: &str) {
    writeln!(stdin, "{message}").unwrap();
    stdin.flush().unwrap();
}

pub fn exits_within(child: &mut Child, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;

    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Whether the child is still running once `limit` has passed, waiting for it all the while.
pub fn alive_after(child: &mut Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;

    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// The example's stderr lines, read on a thread so that a line that never comes fails the test
/// instead of hanging it.
pub struct Stderr(Receiver<String>);

impl Stderr {
    /// The next line with its newline, or an empty string once stderr has ended or `PATIENCE`
    /// has passed.
    pub fn next(&self) -> String {
        self.0.recv_timeout(PATIENCE).unwrap_or_default()
    }

    /// Everything stderr writes until it ends.
    pub fn rest(&self) -> String {
        let deadline = Instant::now() + PATIENCE;
        let mut rest = String::new();

        loop {
            match self
                .0
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => rest.push_str(&line),
                Err(RecvTimeoutError::Disconnected) => return rest,
                Err(RecvTimeoutError::Timeout) => panic!("stderr did not end: {rest:?}"),
            }
        }
    }
}

pub struct Example {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub stderr: Stderr,
}

impl Example {
    /// The next message on stdout.
    pub fn reply(&mut self) -> String {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        line
    }
}

/// Read stdout to its end on a thread, so the server never blocks on a full pipe.
pub fn drain(mut stdout: BufReader<ChildStdout>) {
    std::thread::spawn(move || {
        let _ = stdout.read_to_end(&mut Vec::new());
    });
}

/// The example, initialized.
pub fn initialized() -> Example {
    let mut child = Command::new(example())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut lines = BufReader::new(child.stderr.take().unwrap());
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();

        while lines.read_line(&mut line).unwrap_or(0) > 0 {
            if sender.send(std::mem::take(&mut line)).is_err() {
                return;
            }
        }
    });
    let mut example = Example {
        child,
        stdin,
        stdout,
        stderr: Stderr(receiver),
    };
    send(
        &mut example.stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    let reply = example.reply();
    assert!(reply.contains(r#""serverInfo""#), "{reply}");
    send(
        &mut example.stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    example
}

/// The example, initialized, with one `hang` call in flight (id 2).
pub fn hanging() -> Example {
    let mut example = initialized();
    send(
        &mut example.stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hang","arguments":{}}}"#,
    );
    assert_eq!(example.stderr.next(), "HANG\n");
    example
}
