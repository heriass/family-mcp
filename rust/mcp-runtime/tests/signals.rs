//! A server on the documented entry point (examples/stdio.rs): when a tool call's `Cancelled`
//! completes, and that SIGINT and SIGTERM end the process while a call is in flight and the host
//! still holds stdin open. Without `shutdown_background`, dropping the runtime waits for Tokio's
//! blocking stdin read, which never returns. The example's call ends only once it is cancelled,
//! and its `close` waits for it, so a cancel that never arrives keeps the process alive.

mod support;

use std::process::{Child, ChildStdin};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use support::{Example, Stderr, alive_after, exits_within, send};

/// The example, initialized, with one `hang` call in flight (id 2), and stdout drained.
fn hanging() -> (Child, ChildStdin, Stderr) {
    let Example {
        child,
        stdin,
        stdout,
        stderr,
    } = support::hanging();
    support::drain(stdout);
    (child, stdin, stderr)
}

#[test]
fn a_signal_cancels_the_call_in_flight_and_ends_the_process_while_stdin_stays_open() {
    for signal in [Signal::INT, Signal::TERM] {
        let (mut child, stdin, stderr) = hanging();
        kill_process(Pid::from_child(&child), signal).unwrap();
        assert_eq!(stderr.next(), "CANCELLED\n", "{signal:?}");
        assert_eq!(stderr.next(), "CLOSE\n", "{signal:?}");
        assert_eq!(
            exits_within(&mut child, Duration::from_secs(5)),
            Some(0),
            "{signal:?}"
        );
        // Held open until the process has exited.
        drop(stdin);
    }
}

#[test]
fn the_host_cancelling_a_call_cancels_it_at_once() {
    let (mut child, mut stdin, stderr) = hanging();
    let sent = Instant::now();
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#,
    );
    assert_eq!(stderr.next(), "CANCELLED\n");
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "{:?}",
        sent.elapsed()
    );
    drop(stdin);
    assert_eq!(stderr.next(), "CLOSE\n");
    assert_eq!(exits_within(&mut child, Duration::from_secs(5)), Some(0));
}

/// rmcp first waits up to 5 s for calls in flight to answer, then stops serving, which cancels
/// them. A server that must stop its calls at once does so in `stdin_ended`.
#[test]
fn the_end_of_stdin_cancels_a_call_in_flight_only_after_rmcps_drain() {
    let (mut child, stdin, stderr) = hanging();
    let ended = Instant::now();
    drop(stdin);
    assert_eq!(stderr.next(), "CANCELLED\n");
    let waited = ended.elapsed();
    assert!(
        (Duration::from_secs(4)..Duration::from_secs(8)).contains(&waited),
        "{waited:?}"
    );
    assert_eq!(stderr.next(), "CLOSE\n");
    assert_eq!(exits_within(&mut child, Duration::from_secs(5)), Some(0));
}

/// The bound on serving after stdin's end (6 s, src/stdio.rs `ENDED_SERVING`) starts at that end:
/// a server whose stdin stays open longer keeps serving, and its drain still runs from the end.
#[test]
fn serving_outlasts_the_bound_while_stdin_stays_open_and_drains_from_its_end() {
    let (mut child, stdin, stderr) = hanging();
    assert!(alive_after(&mut child, Duration::from_secs(7)));
    let ended = Instant::now();
    drop(stdin);
    assert_eq!(stderr.next(), "CANCELLED\n");
    let waited = ended.elapsed();
    assert!(
        (Duration::from_secs(4)..Duration::from_secs(8)).contains(&waited),
        "{waited:?}"
    );
    assert_eq!(stderr.next(), "CLOSE\n");
    assert_eq!(exits_within(&mut child, Duration::from_secs(5)), Some(0));
    assert_eq!(stderr.rest(), "");
}
