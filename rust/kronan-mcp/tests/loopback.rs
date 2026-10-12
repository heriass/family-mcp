//! tests/ts/loopback.test.ts, executed against this binary without Bun: a real loopback HTTP
//! server checks the token header, that redirects are never followed, that 429 is the rate-limit
//! message, and that an endless body is cancelled at the 4 MiB cap. Each `client.status()` is an
//! `auth_status` call to a fresh `serve`. Needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{Home, Mcp};

const TOKEN: &str = "synthetic-token-0123456789";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    Rate,
    Large,
    Redirect(u16),
}

#[derive(Default)]
struct Seen {
    authorization: Mutex<Option<String>>,
    redirect_target_hit: AtomicBool,
    chunks: AtomicUsize,
    cancelled: AtomicBool,
}

/// One answer per connection, chosen by `mode`, as the drop-in's `Bun.serve` answered.
fn upstream(mode: Arc<Mutex<Mode>>, seen: Arc<Seen>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream);
            let mut start = String::new();
            let mut authorization = None;

            loop {
                let mut line = String::new();

                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if start.is_empty() {
                    start = line;
                } else if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorization = Some(value.trim().to_owned());
                }
            }
            let path = start.split(' ').nth(1).unwrap_or_default().to_owned();
            let stream = reader.get_mut();
            let json = |body: &Value| {
                let body = body.to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };

            if path == "/api/v1/redirect-target" {
                seen.redirect_target_hit.store(true, Ordering::SeqCst);
                let _ = stream.write_all(
                    json(&json!({"type": "user", "name": "Unexpected redirect target"})).as_bytes(),
                );
                continue;
            }
            if path != "/api/v1/me/" {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nNot found",
                );
                continue;
            }
            *seen.authorization.lock().unwrap() = authorization;
            let mode = *mode.lock().unwrap();

            let _ = match mode {
                Mode::Redirect(status) => stream.write_all(
                    format!("HTTP/1.1 {status} Redirect\r\nLocation: /api/v1/redirect-target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                ),
                Mode::Rate => stream.write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                Mode::Normal => stream.write_all(
                    json(&json!({"type": "user", "name": "Synthetic account"})).as_bytes(),
                ),
                // An endless body of 1 MiB chunks, without a length, until the client stops it.
                Mode::Large => {
                    let chunk = vec![0u8; 1024 * 1024];
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");

                    // A chunk counts once it is started, as a stream pull does: a client that
                    // read past 4 MiB has seen bytes of the fifth.
                    loop {
                        seen.chunks.fetch_add(1, Ordering::SeqCst);

                        if stream.write_all(&chunk).is_err() {
                            break;
                        }
                    }
                    seen.cancelled.store(true, Ordering::SeqCst);
                    Ok(())
                }
            };
        }
    });
    origin
}

/// `client.status()`: the structured result, or the error text.
fn status(home: &Home, origin: &str) -> Result<Value, String> {
    let (mut mcp, _) = Mcp::start(home, origin, &[]);
    let answer = mcp.call("auth_status", json!({})).answer;
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
    let result = &answer["result"];

    match result["isError"] == true {
        true => Err(result["content"][0]["text"].as_str().unwrap().to_owned()),
        false => Ok(result["structuredContent"].clone()),
    }
}

#[test]
fn real_loopback_fetch_authenticates_refuses_redirects_maps_429_and_caps_streams() {
    let home = Home::new("loopback");
    home.write(
        ".config/kronan-mcp/session.json",
        &format!("{}\n", json!({"version": 1, "token": TOKEN})),
        None,
    );
    let mode = Arc::new(Mutex::new(Mode::Normal));
    let seen = Arc::new(Seen::default());
    let origin = upstream(mode.clone(), seen.clone());
    let set = |next| *mode.lock().unwrap() = next;

    assert_eq!(
        status(&home, &origin),
        Ok(
            json!({"authenticated": true, "account": {"type": "user", "name": "Synthetic account"}})
        )
    );
    assert_eq!(
        seen.authorization.lock().unwrap().as_deref(),
        Some(format!("AccessToken {TOKEN}").as_str())
    );

    for redirect in [302, 303, 307] {
        set(Mode::Redirect(redirect));
        let refused = status(&home, &origin).unwrap_err();
        assert!(
            refused.contains("Krónan request failed or timed out"),
            "{redirect}: {refused}"
        );
    }
    assert!(!seen.redirect_target_hit.load(Ordering::SeqCst));

    set(Mode::Rate);
    let limited = status(&home, &origin).unwrap_err();
    assert!(limited.contains("Krónan rate limit reached"), "{limited}");

    set(Mode::Large);
    let large = status(&home, &origin).unwrap_err();
    assert!(large.contains("exceeded the 4 MiB limit"), "{large}");
    let chunks = seen.chunks.load(Ordering::SeqCst);
    assert!((5..32).contains(&chunks), "{chunks} chunks");
    assert!(seen.cancelled.load(Ordering::SeqCst));
}
