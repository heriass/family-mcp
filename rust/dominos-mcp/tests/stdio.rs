//! A broken stdout ends `serve`, as mcp-runtime's `serve_stdio` ends it
//! (rust/mcp-runtime/tests/connection.rs): once the host closes its read side, the next answer
//! fails to write, and the process exits 0 with nothing on stderr while stdin stays open. This
//! guards that `serve` keeps going through `serve_stdio`. Needs the `test-origin` feature, without
//! which the binary would talk to Domino's.
#![cfg(feature = "test-origin")]

use std::fs::DirBuilder;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;

const PING: &str = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;

#[test]
fn a_broken_stdout_ends_serve_while_stdin_stays_open() {
    let home = std::env::temp_dir().join(format!("dominos-stdio-{}", std::process::id()));
    DirBuilder::new().mode(0o700).create(&home).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dominos-mcp"))
        .arg("serve")
        .env("FAMILY_MCP_STORE_TEST_SEAM", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("TMPDIR", &home)
        // Nothing listens there; serve makes no request before a tool call.
        .env("DOMINOS_TEST_ORIGIN", "http://127.0.0.1:9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "{INITIALIZE}").unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.contains(r#""serverInfo""#), "{line}");

    drop(stdout);
    writeln!(stdin, "{PING}").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    // Held open until the process has exited.
    drop(stdin);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stderr, "");
}
