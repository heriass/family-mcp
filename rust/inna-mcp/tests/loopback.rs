//! Response bodies the binary must not read, executed: tests/ts/integration.test.ts:934 against
//! `serve`. Inna answers the message list with a head whose body never comes; the binary refuses
//! each failed or unexpected answer without reading it (its connection is given up, as the
//! TypeScript case's body stream was cancelled) and never returns an empty feed. In the drop-in the
//! stalled read ended at the case's own abort, which the binary was never told of; here the
//! host's cancel ends it. Needs no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::time::{Duration, Instant};

use serde_json::json;
use support::case::{Case, Gate, refused};
use support::{NOW, Reply, Request};

/// Well inside the request's own 30 s deadline.
const PROMPT: Duration = Duration::from_secs(10);
const MESSAGES: &str = "GET /api/Messages/GetReceivedMessages";

fn messages() -> serde_json::Value {
    json!({"rowFrom": 1, "rowTo": 21})
}

#[test]
fn failed_and_stalled_bodies_are_cancelled_without_a_false_empty_feed() {
    let case = Case::new("bodies", false);
    let mut now = NOW;

    for status in [302, 401, 403, 429, 500, 200] {
        let provider = case.provider.clone();
        let mut client = case.client_with(move |request: &Request| {
            let reply = provider.answer(request);

            match request.line() == MESSAGES {
                true => Reply::stalled(status, "text/html"),
                false => reply,
            }
        });
        let started = Instant::now();
        let text = refused(client.call("inna_get_messages", messages()), "");
        // Refused at its head, well inside the request's 30 s deadline: the body was never read.
        assert!(started.elapsed() < PROMPT, "{status}");
        assert!(
            ["sign-in", "denied", "rate limited", "unexpected response"]
                .iter()
                .any(|part| text.contains(part)),
            "{status}: {text}"
        );
        // A 401 is refreshed once and retried once, so both bodies are given up.
        let reads = if status == 401 { 2 } else { 1 };
        assert!(client.upstream().until_abandoned(reads, PROMPT), "{status}");
        assert_eq!(
            client.upstream().abandoned(),
            vec![MESSAGES; reads],
            "{status}"
        );

        if status == 429 {
            // The same private session after its verified pause, its stored state unrewritten.
            now += 60_000;
            case.home.set_clock(now);
            case.client().call("inna_get_messages", messages()).unwrap();
        }
    }

    // A body that stalls is given up when the host cancels the call.
    let gate = Gate::default();
    let (provider, held) = (case.provider.clone(), gate.clone());
    let mut stalled = case.client_with(move |request: &Request| {
        let reply = provider.answer(request);

        match request.line() == MESSAGES {
            true => {
                held.hold();
                Reply::stalled(200, "application/json")
            }
            false => reply,
        }
    });
    let id = stalled.mcp.begin(
        "tools/call",
        json!({"name": "inna_get_messages", "arguments": messages()}),
    );
    gate.reached();
    gate.release();
    stalled.mcp.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": id, "reason": "timed out"},
    }));
    assert!(stalled.upstream().until_abandoned(1, PROMPT));
    // A cancelled call gets no answer; the next request's is the next one.
    let ping = stalled.mcp.request("ping", json!({})).value();
    assert_eq!(ping["id"], id + 1, "{ping}");
    assert_eq!(case.provider.state().posts, 0);
}
