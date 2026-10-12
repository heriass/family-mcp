//! A broken connection with a keep-alive tick in flight, executed: tests/ts/renewal.test.ts:364
//! without Bun. On its own in this test binary: a server spawned by a parallel test could inherit
//! the read side of this one's stdout while the pipe is made (macOS sets close-on-exec after
//! creating it) and keep the pipe from breaking. Needs no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::time::Duration;

use serde_json::json;
use support::case::{Case, Client, Gate};
use support::{PATIENCE, Request, ended};

const USER_PATH: &str = "/api/UserData/GetLoggedInUser";

/// A client whose upstream holds the keep-alive's user request until released.
fn holding(case: &Case, gate: &Gate) -> Client {
    let (provider, held) = (case.provider.clone(), gate.clone());
    case.client_with(move |request: &Request| {
        if request.line() == format!("GET {USER_PATH}") {
            held.hold();
        }
        provider.answer(request)
    })
}

/// tests/ts/renewal.test.ts:364, without Bun, and packages/inna-mcp/test/integration.test.ts:1961
/// (its `error` event), :2113 and :2132 (G1 and G2, fixed in b0c2234): with a tick in flight and
/// standard input still open, a broken stdout or one input line over 10 MiB ends serve with code 0
/// at once. Closing stops the keep-alive and aborts its tick, without which the exit would wait for
/// Inna's held request.
#[test]
fn a_broken_output_pipe_or_an_oversized_input_line_ends_serve_and_aborts_the_tick_in_flight() {
    for breaks in ["output", "input"] {
        let case = Case::new(&format!("broken-{breaks}"), false);
        let gate = Gate::default();
        let mut client = holding(&case, &gate);
        let from = case.provider.paths(0).len();
        client.mcp.signal("USR1");
        gate.reached();

        let writer = if breaks == "output" {
            client.mcp.deafen();
            client
                .mcp
                .write_input(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"ping\"}\n".to_vec())
        } else {
            client.mcp.write_input(vec![b' '; 10 * 1024 * 1024 + 1])
        };
        // Well inside the held request's 30 s deadline: the tick was aborted, not awaited.
        let status = client.mcp.exit_within(Duration::from_secs(5));
        gate.release();
        let status = status.unwrap_or_else(|| panic!("{breaks}: serve kept running"));
        assert_eq!(
            ended(status),
            json!({"exited": 0, "signal": null}),
            "{breaks}"
        );
        // Held open until the process has exited.
        drop(writer.join().unwrap());
        // The held request is answered, and its path recorded, once released; its client gave it up.
        assert!(client.upstream().until_abandoned(1, PATIENCE), "{breaks}");
        assert_eq!(
            client.upstream().abandoned(),
            [format!("GET {USER_PATH}")],
            "{breaks}"
        );
        assert_eq!(case.provider.paths(from), [USER_PATH], "{breaks}");
        assert!(
            client.mcp.statuses().is_empty(),
            "{breaks}: {}",
            client.mcp.stderr()
        );
    }
}
