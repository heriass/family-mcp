//! The keep-alive timer, executed: the drop-in keep-alive cases of tests/ts/integration.test.ts,
//! whose `keepAlive()` was one tick of the binary's own timer (SIGUSR1 under
//! INNA_TEST_KEEP_ALIVE) and the status it reported, and the observable outcome of the
//! TypeScript package's stdio-lifecycle cases. The drop-ins' `scheduled()` drove the TypeScript
//! scheduler, so its intervals and cancel count were never the binary's; the binary's durations
//! and its stop are its own unit tests (src/keep_alive.rs). Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::{Value, json};
use support::case::{Case, Client, Gate, refused};
use support::provider::{SIBLING, expect_clean, sibling};
use support::{NOW, PATIENCE, Request, ended};

const USER_PATH: &str = "/api/UserData/GetLoggedInUser";

fn import(case: &Case) {
    let source = case.home.path("cookies.json").display().to_string();
    let run = case.cli(&["auth", "import", &source]);
    assert_eq!(run.code, Some(0), "{run:?}");
}

/// The record without its cookie jar.
fn without_jar(saved: &Value) -> Value {
    let mut saved = saved.clone();
    saved["jar"] = json!("");
    saved
}

/// integration.test.ts:1778
#[test]
fn keep_alive_makes_one_user_request_persists_the_rotated_cookie_and_returns_a_status() {
    let case = Case::new("kept", false);
    let before = case.saved();
    let count = case.provider.state().calls.len();
    case.provider.state().rotation = "synthetic-kept".into();
    let mut client = case.client();
    let status = client.keep_alive();
    assert_eq!(status, "kept");
    expect_clean(&client.mcp.stderr(), "the keep-alive's report");
    assert_eq!(case.provider.paths(count), [USER_PATH]);
    let after = case.saved();
    assert!(after["jar"].as_str().unwrap().contains("synthetic-kept"));
    assert!(!before["jar"].as_str().unwrap().contains("synthetic-kept"));
    assert_eq!(without_jar(&after), without_jar(&before));
    expect_clean(&case.home.record().unwrap(), "the stored record");
    let path = case.home.store_options().path;
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// integration.test.ts:1795
#[test]
fn keep_alive_makes_no_request_without_a_session_or_during_a_pause() {
    let case = Case::new("no-session", false);
    assert_eq!(case.cli(&["auth", "logout"]).code, Some(0));
    let count = case.provider.state().calls.len();
    assert_eq!(case.client().keep_alive(), "skipped");
    assert_eq!(case.provider.state().calls.len(), count);
    assert!(!case.home.path("session.json").exists());
    assert_eq!(case.home.record().as_deref(), Some("null"));

    let limited = Case::new("paused", false);
    let mut client = limited.client();
    limited.provider.state().rate_limit = true;
    assert_eq!(client.keep_alive(), "failed");
    assert_eq!(limited.saved()["pauseUntil"], json!(NOW + 60_000));
    limited.provider.state().rate_limit = false;
    let paused = limited.provider.state().calls.len();
    let file = limited.home.record();
    // Rust: an unknown renewal age is overdue; the pause still sends no request.
    assert_eq!(client.keep_alive(), "failed");
    assert_eq!(limited.client().keep_alive(), "failed");
    assert_eq!(limited.provider.state().calls.len(), paused);
    assert_eq!(limited.home.record(), file);
}

/// integration.test.ts:1820. The server makes no request until its timer ticks; the stop is the
/// end of its input.
#[test]
fn an_expired_session_stops_the_keep_alive_until_a_new_import() {
    let case = Case::new("expired-session", false);
    let count = case.provider.state().calls.len();
    let mut client = case.client();
    assert!(client.mcp.statuses().is_empty());
    assert_eq!(case.provider.state().calls.len(), count);
    case.provider.state().unauthorized = true;
    assert_eq!(client.keep_alive(), "signInRequired");
    expect_clean(&client.mcp.stderr(), "the keep-alive's report");
    assert_eq!(case.provider.paths(count), [USER_PATH]);
    assert_eq!(client.keep_alive(), "skipped");
    assert_eq!(client.keep_alive(), "skipped");
    assert_eq!(case.provider.state().calls.len(), count + 1);
    // The same refusal reaches a tool call with the existing message.
    let text = refused(client.call("inna_get_overview", json!({})), "");
    assert!(text.starts_with("Inna sign-in is required."), "{text}");

    case.provider.state().unauthorized = false;
    case.provider.state().rotation = "synthetic-renewed".into();
    import(&case);
    let renewed = case.provider.state().calls.len();
    assert_eq!(client.keep_alive(), "kept");
    assert_eq!(case.provider.paths(renewed), [USER_PATH]);
    let (code, _) = client.mcp.close();
    assert_eq!(code, Some(0));
}

/// integration.test.ts:1847
#[test]
fn keep_alive_on_a_session_moved_to_the_sibling_neither_switches_nor_relearns() {
    let case = Case::new("moved", false);
    let before = case.saved();
    {
        let mut state = case.provider.state();
        state.selected = SIBLING.into();
        state.user = sibling();
    }
    let count = case.provider.state().calls.len();
    assert_eq!(case.client().keep_alive(), "kept");
    assert_eq!(case.provider.paths(count), [USER_PATH]);
    assert_eq!(case.provider.state().switches, 0);
    let after = case.saved();
    assert_eq!(after["account"], before["account"]);
    assert_eq!(after["students"], before["students"]);
    let keys: Vec<&String> = after["students"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["1"]);
}

/// integration.test.ts:1862
#[test]
fn two_servers_sharing_a_refused_session_stop_asking_and_replaced_credentials_resume() {
    let case = Case::new("shared-refusal", false);
    let mut clients = [case.client(), case.client()];
    case.provider.state().unauthorized = true;
    let start = case.provider.state().calls.len();
    let mut statuses = Vec::new();

    for _ in 0..4 {
        for client in &mut clients {
            // Cookie access times have millisecond resolution; each round must land on a new one.
            std::thread::sleep(Duration::from_millis(2));
            let before = case.home.record();
            statuses.push(client.keep_alive());

            // A refused request rewrites cookie access times; that alone must not look like a new
            // session.
            if statuses.last().unwrap() == "signInRequired" {
                assert_ne!(case.home.record(), before);
            }
        }
    }
    let mut expected = vec!["signInRequired"; 2];
    expected.extend(["skipped"; 6]);
    assert_eq!(statuses, expected);
    assert_eq!(case.provider.state().calls.len(), start + 2);
    // A tool call on the dead session rewrites the jar without changing the credentials.
    let text = refused(clients[0].call("inna_get_overview", json!({})), "");
    assert!(text.starts_with("Inna sign-in is required."), "{text}");
    let after_tool = case.provider.state().calls.len();

    for client in &mut clients {
        assert_eq!(client.keep_alive(), "skipped");
    }
    assert_eq!(case.provider.state().calls.len(), after_tool);

    case.provider.state().unauthorized = false;
    case.provider.state().rotation = "synthetic-replaced".into();
    import(&case);
    let replaced = case.provider.state().calls.len();

    for client in &mut clients {
        assert_eq!(client.keep_alive(), "kept");
    }
    assert_eq!(case.provider.paths(replaced), [USER_PATH, USER_PATH]);

    for client in clients {
        let (code, stderr) = client.mcp.close();
        assert_eq!(code, Some(0));
        expect_clean(&stderr, "a server's stderr");
    }
}

/// integration.test.ts:1903
#[test]
fn a_rotated_cookie_value_counts_as_new_credentials_for_a_refused_session() {
    let case = Case::new("rotated-value", false);
    let mut client = case.client();
    case.provider.state().unauthorized = true;
    assert_eq!(client.keep_alive(), "signInRequired");
    assert_eq!(client.keep_alive(), "skipped");
    assert!(case.home.record().unwrap().contains("synthetic-rotated"));
    case.home
        .update_record(|saved| saved.replace("synthetic-rotated", "synthetic-other"));
    let count = case.provider.state().calls.len();
    assert_eq!(client.keep_alive(), "signInRequired");
    assert_eq!(client.keep_alive(), "skipped");
    assert_eq!(case.provider.state().calls.len(), count + 1);
}

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

/// packages/inna-mcp/test/integration.test.ts:1961 (its `close` event) and :2154: with a tick in
/// flight, the end of the server's input stops the timer and aborts the tick. The process exits
/// while Inna still holds the tick's request, which its client has given up. The `error` event
/// (a broken stdout) is broken_connection.rs's.
#[test]
fn the_end_of_input_stops_the_timer_and_aborts_the_tick_in_flight() {
    let case = Case::new("close-in-flight", false);
    let gate = Gate::default();
    let mut client = holding(&case, &gate);
    client.mcp.signal("USR1");
    gate.reached();
    client.mcp.end_input();
    // Well before the request's own 30 s deadline: the tick was aborted, not awaited.
    let status = client.mcp.exit_within(Duration::from_secs(10));
    let status = status.expect("serve waited for the tick in flight");
    assert_eq!(ended(status), json!({"exited": 0, "signal": null}));
    gate.release();
    assert!(client.upstream().until_abandoned(1, PATIENCE));
    assert_eq!(client.upstream().abandoned(), [format!("GET {USER_PATH}")]);
    assert!(client.mcp.statuses().is_empty(), "{}", client.mcp.stderr());
    assert_eq!(
        case.provider
            .paths(0)
            .iter()
            .filter(|path| *path == USER_PATH)
            .count(),
        2
    );
}

/// packages/inna-mcp/test/integration.test.ts:2078: a `server/discover` negotiation, then a
/// legacy `initialize` on the same connection, and the keep-alive ticks after each exchange.
#[test]
fn discovery_then_a_legacy_initialize_keeps_the_keep_alive_running() {
    let case = Case::new("discovery", false);
    let mut mcp = support::Mcp::spawn(&case.home, &case.upstream.origin, &case.env(), &[]);
    let client = json!({"name": "offline", "version": "1"});
    let mut replies = Vec::new();
    let mut tick = |mcp: &mut support::Mcp, method: &str, params: Value| {
        let answer = mcp.request(method, params).value();
        replies.push(answer["id"].clone());
        assert!(answer["result"].is_object(), "{answer}");
        let before = mcp.statuses().len();
        mcp.signal("USR1");
        assert!(mcp.until_stderr("keep-alive: ", before + 1, PATIENCE));
    };
    tick(
        &mut mcp,
        "server/discover",
        json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": client,
        }}),
    );
    tick(
        &mut mcp,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": client}),
    );
    tick(&mut mcp, "ping", json!({}));
    assert_eq!(replies, [json!(1), json!(2), json!(3)]);
    assert_eq!(mcp.statuses(), ["kept", "kept", "kept"]);
    let (code, _) = mcp.close();
    assert_eq!(code, Some(0));
}
