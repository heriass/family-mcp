//! `read_capped` on real loopback responses, as packages/mcp-runtime/test/index.test.ts checked
//! `readBody`: a cap counted in bytes, not characters, and a stalled body whose read is abandoned
//! (the servers abandon it with a timeout or a cancel) closes the connection instead of hanging on
//! to it.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use mcp_runtime::{BodyError, read_capped};

/// Serve one response whose head and first bytes are `head`, then report when the client closes
/// the connection. Returns the URL and the report.
fn serve(head: &'static str) -> (String, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (closed, report) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut rest = [0; 64];

        // Zero bytes is the client's close; a timeout means it kept the connection.
        if matches!(stream.read(&mut rest), Ok(0)) {
            closed.send(()).unwrap();
        }
    });
    (url, report)
}

#[tokio::test]
async fn the_cap_counts_bytes() {
    let client = reqwest::Client::new();

    for (cap, expected) in [
        (2, Ok("í".as_bytes().to_vec())),
        (1, Err(BodyError::TooLarge)),
    ] {
        let (url, _) = serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\ní");
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(read_capped(response, cap).await, expected, "cap {cap}");
    }
}

#[tokio::test]
async fn an_abandoned_stalled_read_closes_the_connection() {
    let client = reqwest::Client::new();
    let (url, closed) = serve("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial");
    let response = client.get(&url).send().await.unwrap();

    let read = tokio::time::timeout(Duration::from_millis(20), read_capped(response, 1024)).await;
    assert!(read.is_err(), "the stalled body never completes");
    // The client stays alive: only the abandoned read let go of the connection.
    assert_eq!(
        tokio::task::spawn_blocking(move || closed.recv_timeout(Duration::from_secs(5)))
            .await
            .unwrap(),
        Ok(())
    );
    drop(client);
}
