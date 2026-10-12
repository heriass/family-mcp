//! integration.test.ts:2500, executed against this binary without Bun: SIGTERM while a Krónan
//! request is pending aborts that request and stops the stdio server. Needs the `test-origin`
//! feature.
#![cfg(feature = "test-origin")]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::channel;
use std::time::Duration;

use serde_json::json;
use support::{Home, Mcp};

const TOKEN: &str = "synthetic-token-0123456789";

/// The drop-in case's `Bun.sleep(5000)` bounds.
const BOUND: Duration = Duration::from_secs(5);

#[test]
fn sigterm_aborts_a_pending_request_and_closes_the_stdio_process() {
    let home = Home::new("shutdown");
    home.save(Some(TOKEN));

    // A Krónan that never answers: it reports when a request arrived and when its client closed
    // the connection, as the drop-in's never-resolving fetch reported FETCH_STARTED and
    // FETCH_ABORTED.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (started, fetch_started) = channel();
    let (aborted, fetch_aborted) = channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream);
            let mut start = String::new();
            let _ = reader.read_line(&mut start);

            // A loopback port scanner's `GET /` is never Krónan's.
            if !start.contains(" /api/v1/") {
                let _ = reader.get_mut().write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            let _ = started.send(start);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            let _ = aborted.send(());
            return;
        }
    });

    let (mut mcp, _) = Mcp::start(&home, &origin, &[]);
    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "auth_status", "arguments": {}},
    }));
    let request = fetch_started
        .recv_timeout(support::PATIENCE)
        .expect("auth_status never reached Krónan");
    assert!(request.starts_with("GET /api/v1/me/ "), "{request}");

    let killed = std::process::Command::new("kill")
        .args(["-TERM", &mcp.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    fetch_aborted
        .recv_timeout(BOUND)
        .expect("The in-flight request was not aborted.");
    assert!(
        mcp.wait(BOUND).is_some(),
        "The stdio process did not stop after SIGTERM."
    );
}
