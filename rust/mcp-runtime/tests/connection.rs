//! A broken connection while the host still holds stdin open, as the TypeScript SDK's stdio
//! transport closes it: a failed write to stdout (its `error` event) or an input line over its
//! 10 MiB read buffer. Either stops serving at once, which cancels the calls in flight, then runs
//! the server's `close`, and the process exits 0 without a word on stderr, as the TypeScript
//! server does.

mod support;

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::{exits_within, send};

/// The SDK's `STDIO_DEFAULT_MAX_BUFFER_SIZE`.
const LIMIT: usize = 10 * 1024 * 1024;

const PING: &str = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;

/// At once: rmcp's drain at stdin's end waits up to 5 s.
const AT_ONCE: Duration = Duration::from_secs(2);

#[test]
fn a_failed_stdout_write_cancels_the_call_in_flight_and_ends_the_process() {
    let mut example = support::hanging();
    // The host closes its read side; the ping's answer is the next write.
    drop(example.stdout);
    let broken = Instant::now();
    send(&mut example.stdin, PING);
    assert_eq!(example.stderr.next(), "CANCELLED\n");
    assert!(broken.elapsed() < AT_ONCE, "{:?}", broken.elapsed());
    assert_eq!(example.stderr.next(), "CLOSE\n");
    assert_eq!(
        exits_within(&mut example.child, Duration::from_secs(5)),
        Some(0)
    );
    assert_eq!(example.stderr.rest(), "");
    // Held open until the process has exited.
    drop(example.stdin);
}

#[test]
fn an_input_line_over_10_mib_cancels_the_call_in_flight_and_ends_the_process() {
    let mut example = support::hanging();
    support::drain(example.stdout);
    example.stdin.write_all(&vec![b' '; LIMIT + 1]).unwrap();
    example.stdin.flush().unwrap();
    let sent = Instant::now();
    assert_eq!(example.stderr.next(), "CANCELLED\n");
    assert!(sent.elapsed() < AT_ONCE, "{:?}", sent.elapsed());
    assert_eq!(example.stderr.next(), "CLOSE\n");
    assert_eq!(
        exits_within(&mut example.child, Duration::from_secs(5)),
        Some(0)
    );
    assert_eq!(example.stderr.rest(), "");
    drop(example.stdin);
}

/// The SDK counts every buffered byte, the newline too: a line of exactly 10 MiB with its newline
/// is read, and one byte more always closes, however the input is split into reads.
#[test]
fn a_line_of_10_mib_with_its_newline_is_read_and_one_byte_more_ends_the_process() {
    let mut example = support::initialized();
    let mut line = vec![b' '; LIMIT - 1];
    line.push(b'\n');
    example.stdin.write_all(&line).unwrap();
    send(&mut example.stdin, PING);
    let reply = example.reply();
    assert!(reply.contains(r#""id":3"#), "{reply}");

    line.insert(0, b' ');
    example.stdin.write_all(&line).unwrap();
    example.stdin.flush().unwrap();
    assert_eq!(example.stderr.next(), "CLOSE\n");
    assert_eq!(
        exits_within(&mut example.child, Duration::from_secs(5)),
        Some(0)
    );
    assert_eq!(example.stderr.rest(), "");
    assert_eq!(example.reply(), "");
    drop(example.stdin);
}

/// A stdout the process cannot write, such as a read-only file: every write fails with EBADF,
/// which `std::io::stdout` would report as success, so the answer to initialize breaks the
/// connection, as the TypeScript server exits 0 there.
#[test]
fn a_stdout_that_cannot_be_written_ends_the_process() {
    let path = std::env::temp_dir().join(format!("mcp-runtime-read-only-{}", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    let mut child = Command::new(support::example())
        .stdin(Stdio::piped())
        .stdout(std::fs::File::open(&path).unwrap())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    let code = exits_within(&mut child, Duration::from_secs(5));
    let mut written = String::new();
    stderr.read_to_string(&mut written).unwrap();
    // Held open until the process has exited.
    drop(stdin);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code, Some(0), "{written}");
    assert_eq!(written, "CLOSE\n");
}

/// The example with its `hang` call in flight and a stdout the host no longer reads: a ping whose
/// 3 MiB id comes back in its answer has started writing (4096 bytes read), and the rest waits on
/// the full pipe, which stays open.
fn stuck() -> support::Example {
    let mut example = support::hanging();
    let id = "x".repeat(3 * 1024 * 1024);
    send(
        &mut example.stdin,
        &format!(r#"{{"jsonrpc":"2.0","id":"{id}","method":"ping"}}"#),
    );
    let mut started = [0; 4096];
    example.stdout.read_exact(&mut started).unwrap();
    example
}

/// rmcp drains for up to 5 s at stdin's end, then closes its transport, which waits without
/// bound for the stuck write. Serving stops anyway once `ENDED_SERVING` (6 s) has passed.
const ENDED: Duration = Duration::from_secs(9);

#[test]
fn the_end_of_stdin_ends_the_process_while_a_write_waits_on_an_unread_stdout() {
    let mut example = stuck();
    let ended = Instant::now();
    drop(example.stdin);
    assert_eq!(example.stderr.next(), "CANCELLED\n");
    assert_eq!(example.stderr.next(), "CLOSE\n");
    assert_eq!(exits_within(&mut example.child, ENDED), Some(0));
    assert!(ended.elapsed() < ENDED, "{:?}", ended.elapsed());
    assert_eq!(example.stderr.rest(), "");
    // Open and unread until the process has exited.
    drop(example.stdout);
}

#[test]
fn a_call_cancelled_before_stdins_end_still_ends_the_process_while_a_write_waits() {
    let mut example = stuck();
    send(
        &mut example.stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#,
    );
    assert_eq!(example.stderr.next(), "CANCELLED\n");
    let ended = Instant::now();
    drop(example.stdin);
    assert_eq!(example.stderr.next(), "CLOSE\n");
    assert_eq!(exits_within(&mut example.child, ENDED), Some(0));
    assert!(ended.elapsed() < ENDED, "{:?}", ended.elapsed());
    assert_eq!(example.stderr.rest(), "");
    drop(example.stdout);
}
