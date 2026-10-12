//! The money cases of tests/ts/integration.test.ts and interop.test.ts, executed against this
//! binary without Bun: each drop-in client call is a tool call to a fresh `serve` (or one long
//! `serve` where the case uses one), against a loopback Krónan whose answers default to the frozen
//! TypeScript fixture answers. Each case checks the requests sent (method and path, in order), the
//! order-attempt record and the files left, and the messages the TypeScript case matched. Needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{Home, Mcp, Reply, Request, Upstream, fixture, fixture_json};

const TOKEN: &str = "synthetic-token-0123456789";

const ORDER_TOKEN: &str = "123e4567-e89b-12d3-a456-426614174001";

const LIST_TOKEN: &str = "123e4567-e89b-12d3-a456-426614174002";

const CHECKOUT_TOKEN: &str = "123e4567-e89b-12d3-a456-426614174006";

const CHECKOUT_TOTAL: i64 = 1489;

/// The legacy token file the binary reads by default, and the order-attempt record beside it.
const LEGACY: &str = ".config/kronan-mcp/session.json";

const JOURNAL: &str = ".config/kronan-mcp/session.json.order-attempts.json";

const NOTHING_SENT: &str = "Nothing was sent to Krónan";

fn approval() -> Value {
    json!({"confirm": true, "expectedTotal": CHECKOUT_TOTAL, "expectedCheckoutToken": CHECKOUT_TOKEN})
}

/// `base` with `extra`'s keys added or replaced.
fn with(base: &Value, extra: Value) -> Value {
    let mut merged = base.as_object().unwrap().clone();
    merged.extend(extra.as_object().unwrap().clone());
    Value::Object(merged)
}

/// integration.test.ts `placements`: each money tool, its path, and its call for an approval.
fn placements() -> [(&'static str, &'static str, Value); 4] {
    [
        (
            "reserve_delivery_slot",
            "/slots/delivery/reserve/",
            json!({"slotId": 501, "addressId": 11, "returnBags": false}),
        ),
        (
            "reserve_pickup_slot",
            "/slots/pickup/reserve/",
            json!({"slotId": 601, "returnBags": false}),
        ),
        (
            "complete_checkout",
            "/checkout/complete/",
            json!({"slotId": 501, "addressId": 11, "returnBags": false}),
        ),
        (
            "add_checkout_to_order",
            "/checkout/add-to-order/",
            json!({"expectedOrderToken": ORDER_TOKEN}),
        ),
    ]
}

/// A home with the legacy token file.
fn home(name: &str) -> Home {
    let home = Home::new(name);
    home.write(
        LEGACY,
        &format!("{{\"version\":1,\"token\":\"{TOKEN}\"}}\n"),
        None,
    );
    home
}

/// integration.test.ts `scriptedClient`: `script` answers chosen requests, the fixture the rest.
fn scripted(script: impl Fn(&Request) -> Option<Reply> + Send + Sync + 'static) -> Upstream {
    Upstream::new(move |request| script(request).unwrap_or_else(|| fixture(&request.path)))
}

/// A tool answer as the drop-in client sees it: structured content, or the error text.
fn outcome(answer: &Value) -> Result<Value, String> {
    assert!(answer.get("error").is_none(), "{answer}");
    let result = &answer["result"];

    if result["isError"] == true {
        Err(result["content"][0]["text"].as_str().unwrap().to_owned())
    } else {
        Ok(result["structuredContent"].clone())
    }
}

/// One tool call to a fresh `serve`, as the drop-in client's methods make it. Its answer never
/// discloses the token.
fn call(home: &Home, upstream: &Upstream, tool: &str, arguments: Value) -> Result<Value, String> {
    let (mut mcp, _) = Mcp::start(home, &upstream.origin, &[]);
    let answer = mcp.call(tool, arguments).answer;
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!answer.to_string().contains(TOKEN), "{answer}");
    outcome(&answer)
}

/// The order-attempt record's attempts, or None without a record.
fn journal(home: &Home) -> Option<Vec<Value>> {
    let text = fs::read_to_string(home.path(JOURNAL)).ok()?;
    let record: Value = serde_json::from_str(&text).unwrap();
    Some(record["attempts"].as_array().unwrap().clone())
}

/// The names under `.config/kronan-mcp`.
fn left(home: &Home) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(home.path(".config/kronan-mcp"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn only_the_token(home: &Home) {
    assert_eq!(left(home), ["session.json"]);
}

/// `message` holds each part, in order (a TypeScript `a.*b` pattern).
fn matches(message: &str, parts: &[&str]) -> bool {
    let mut rest = message;

    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

fn posts(sent: &[String]) -> Vec<String> {
    sent.iter()
        .filter(|entry| entry.starts_with("POST"))
        .cloned()
        .collect()
}

/// The fixture checkout, which the gate reads.
fn checkout() -> Value {
    fixture_json("/api/v1/checkout/")
}

#[test]
fn the_money_gate_refuses_without_sending_a_charge_bearing_request() {
    let home = home("gate");
    let current = Arc::new(Mutex::new(checkout()));
    let state = current.clone();
    let upstream = scripted(move |request| {
        (request.path == "/api/v1/checkout/")
            .then(|| Reply::json(200, &state.lock().unwrap().clone()))
    });
    let empty = with(&checkout(), json!({"lines": [], "total": 0}));
    let refusals = [
        (
            "total mismatch",
            checkout(),
            with(&approval(), json!({"expectedTotal": CHECKOUT_TOTAL - 1})),
            "total differs",
        ),
        (
            "token mismatch",
            checkout(),
            with(&approval(), json!({"expectedCheckoutToken": LIST_TOKEN})),
            "checkout token differs",
        ),
        (
            "empty checkout",
            empty,
            with(&approval(), json!({"expectedTotal": 0})),
            "checkout is empty",
        ),
    ];

    for (tool, _, arguments) in placements() {
        for (label, state, gate, pattern) in &refusals {
            *current.lock().unwrap() = state.clone();
            let before = upstream.count();
            let message = call(&home, &upstream, tool, with(gate, arguments.clone())).unwrap_err();
            assert!(message.contains(pattern), "{tool}: {label}: {message}");
            assert!(message.contains(NOTHING_SENT), "{tool}: {label}: {message}");
            assert!(message.contains("no order was placed"), "{message}");
            assert_eq!(
                upstream.sent()[before..],
                ["GET /checkout/"],
                "{tool}: {label}"
            );
        }
    }
    assert_eq!(posts(&upstream.sent()), Vec::<String>::new());
    // A refused gate records nothing, so it never blocks a later, corrected approval.
    assert_eq!(journal(&home), None);
    only_the_token(&home);
}

#[test]
fn money_and_destructive_tools_without_confirm_send_nothing() {
    let home = home("confirm");
    let upstream = scripted(|_| None);
    let mut unconfirmed = approval();
    unconfirmed.as_object_mut().unwrap().remove("confirm");
    let calls = [
        (
            "reserve_delivery_slot",
            with(
                &unconfirmed,
                json!({"slotId": 501, "addressId": 11, "returnBags": false}),
            ),
        ),
        (
            "reserve_pickup_slot",
            with(&unconfirmed, json!({"slotId": 601, "returnBags": false})),
        ),
        (
            "complete_checkout",
            with(&unconfirmed, json!({"slotId": 501, "returnBags": false})),
        ),
        (
            "add_checkout_to_order",
            with(&unconfirmed, json!({"expectedOrderToken": ORDER_TOKEN})),
        ),
        ("clear_shopping_note", json!({})),
        (
            "delete_order_lines",
            json!({"orderToken": ORDER_TOKEN, "lineIds": [1]}),
        ),
        (
            "lower_order_line_quantities",
            json!({"orderToken": ORDER_TOKEN, "lineIds": [1], "quantity": 0}),
        ),
    ];
    // One server, as the case connects one client.
    let (mut mcp, _) = Mcp::start(&home, &upstream.origin, &[]);

    for (tool, arguments) in &calls {
        for confirm in [None, Some(json!(false)), Some(json!("true"))] {
            let arguments = match &confirm {
                None => arguments.clone(),
                Some(confirm) => with(arguments, json!({"confirm": confirm})),
            };
            let answer = mcp.call(tool, arguments).answer;
            assert!(outcome(&answer).is_err(), "{tool} {confirm:?}: {answer}");
        }
    }
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(upstream.sent(), Vec::<String>::new());
    assert_eq!(journal(&home), None);
    only_the_token(&home);
}

#[test]
fn add_checkout_to_order_refuses_an_absent_or_different_active_order() {
    let home = home("active");
    let active = Arc::new(Mutex::new(Reply::empty(404)));
    let state = active.clone();
    let upstream = scripted(move |request| {
        (request.path == "/api/v1/orders/currently-active/").then(|| state.lock().unwrap().clone())
    });
    let differs = with(
        &fixture_json("/api/v1/orders/currently-active/"),
        json!({"orderToken": LIST_TOKEN}),
    );

    for (reply, pattern) in [
        (Reply::empty(404), "no active order"),
        (Reply::json(200, &differs), "active order differs"),
    ] {
        *active.lock().unwrap() = reply;
        let before = upstream.count();
        let arguments = with(&approval(), json!({"expectedOrderToken": ORDER_TOKEN}));
        let message = call(&home, &upstream, "add_checkout_to_order", arguments).unwrap_err();
        assert!(message.contains(pattern), "{message}");
        assert!(message.contains(NOTHING_SENT), "{message}");
        assert_eq!(
            upstream.sent()[before..],
            ["GET /checkout/", "GET /orders/currently-active/"]
        );
    }
    assert_eq!(journal(&home), None);
    only_the_token(&home);
}

#[test]
fn ambiguous_money_outcomes_are_unknown_after_exactly_one_request() {
    let home = home("ambiguous");
    let failure = Arc::new(Mutex::new(Reply::dropped()));
    let state = failure.clone();
    let upstream =
        scripted(move |request| (request.method == "POST").then(|| state.lock().unwrap().clone()));
    // The upstream may have accepted the request before any of these; none proves a refusal. A
    // failed fetch (a network error or a timeout) reaches the binary as a dropped connection.
    let ambiguous = [
        ("network error", Reply::dropped()),
        ("timeout", Reply::dropped()),
        ("server error", Reply::text(502, TOKEN)),
        ("unparsable body", Reply::text(201, "{not json")),
        (
            "undocumented body",
            Reply::json(201, &json!({"orderToken": ORDER_TOKEN})),
        ),
        ("accepted then 400", Reply::text(400, TOKEN)),
        ("accepted then 408", Reply::text(408, TOKEN)),
        ("accepted then 409", Reply::text(409, TOKEN)),
        ("accepted then 422", Reply::text(422, TOKEN)),
        ("accepted then 429", Reply::text(429, TOKEN)),
        ("accepted then 401", Reply::text(401, TOKEN)),
    ];

    for (tool, path, arguments) in placements() {
        for (label, reply) in &ambiguous {
            // Each case is a fresh approval; the record rules are tested separately.
            let _ = fs::remove_file(home.path(JOURNAL));
            *failure.lock().unwrap() = reply.clone();
            let before = upstream.count();
            let placed = call(&home, &upstream, tool, with(&approval(), arguments.clone()))
                .unwrap_or_else(|message| panic!("{tool}: {label}: {message}"));
            assert_eq!(placed["outcome"], "unknown", "{tool}: {label}");
            let message = placed["message"].as_str().unwrap();
            assert!(
                matches(
                    message,
                    &["Outcome unknown", "do not retry or place another order"]
                ),
                "{message}"
            );
            assert!(
                !message.contains("Nothing was sent") && !message.contains("no order was placed")
            );
            assert_eq!(
                posts(&upstream.sent()[before..]),
                [format!("POST {path}")],
                "{tool}: {label}"
            );
            // Exactly one attempt, recorded as unknown for this tool and checkout.
            let attempts = journal(&home).unwrap();
            assert_eq!(attempts.len(), 1, "{tool}: {label}");
            assert_eq!(attempts[0]["tool"], tool);
            assert_eq!(attempts[0]["state"], "unknown", "{tool}: {label}");
            assert_eq!(attempts[0]["checkoutToken"], CHECKOUT_TOKEN);
            assert_eq!(
                left(&home),
                ["session.json", "session.json.order-attempts.json"]
            );
        }
    }
}

#[test]
fn an_unknown_money_outcome_is_a_validated_result_not_an_error() {
    let home = home("validated");
    let upstream = scripted(|request| (request.method == "POST").then(|| Reply::text(502, TOKEN)));
    let calls = [
        (
            "reserve_delivery_slot",
            json!({"slotId": 501, "addressId": 11, "returnBags": false}),
        ),
        (
            "reserve_pickup_slot",
            json!({"slotId": 601, "returnBags": false}),
        ),
        (
            "complete_checkout",
            json!({"slotId": 501, "returnBags": false}),
        ),
        (
            "add_checkout_to_order",
            json!({"expectedOrderToken": ORDER_TOKEN}),
        ),
    ];
    // One server for every call: the record removed between them is read again each time.
    let (mut mcp, _) = Mcp::start(&home, &upstream.origin, &[]);

    for (tool, arguments) in calls {
        let _ = fs::remove_file(home.path(JOURNAL));
        let before = upstream.count();
        let answer = mcp.call(tool, with(&approval(), arguments)).answer;
        assert!(!answer.to_string().contains(TOKEN), "{answer}");
        assert_ne!(answer["result"]["isError"], true, "{tool}: {answer}");
        let structured = &answer["result"]["structuredContent"];
        assert_eq!(structured["outcome"], "unknown", "{tool}");
        assert!(matches(
            &structured.to_string(),
            &["Outcome unknown", "do not retry or place another order"]
        ));
        assert_eq!(posts(&upstream.sent()[before..]).len(), 1, "{tool}");
        assert_eq!(journal(&home).unwrap()[0]["state"], "unknown", "{tool}");
    }
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
}

#[test]
fn a_failed_gate_read_refuses_the_money_call_without_sending() {
    let home = home("gate-read");
    let upstream =
        scripted(|request| (request.path == "/api/v1/checkout/").then(|| Reply::text(503, TOKEN)));

    for (tool, _, arguments) in placements() {
        let before = upstream.count();
        let message = call(&home, &upstream, tool, with(&approval(), arguments)).unwrap_err();
        assert!(message.contains(NOTHING_SENT), "{tool}: {message}");
        assert!(message.contains("Could not read the checkout"), "{message}");
        assert_eq!(upstream.sent()[before..], ["GET /checkout/"], "{tool}");
    }
    assert_eq!(journal(&home), None);
    only_the_token(&home);
}

#[test]
fn a_record_written_by_another_holder_during_the_gate_stops_the_real_client_before_its_post() {
    let home = home("gate-write");
    let other = json!({
        "id": "other-holder",
        "tool": "reserve_pickup_slot",
        "checkoutToken": LIST_TOKEN,
        "fingerprint": "other-lines",
        "total": 1,
        "state": "unknown",
        "orderToken": null,
        "createdAt": "2026-09-25T00:00:00.000Z",
        "updatedAt": "2026-09-25T00:00:00.000Z",
    });
    let text = json!({"version": 1, "attempts": [other]}).to_string();
    let journal_path = home.path(JOURNAL);
    let written = text.clone();
    // Another holder records an unresolved attempt for a different checkout while this gate reads.
    let upstream = scripted(move |request| {
        if request.path == "/api/v1/checkout/" {
            fs::write(&journal_path, &written).unwrap();
            fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        None
    });
    let arguments = with(&approval(), json!({"slotId": 501, "returnBags": false}));
    let message = call(&home, &upstream, "complete_checkout", arguments).unwrap_err();

    assert_eq!(
        message,
        "Another Krónan order call ran at the same time and changed the local order-attempt record. Nothing was sent to Krónan. Read get_active_order before anything else."
    );
    assert_eq!(upstream.sent(), ["GET /checkout/"]);
    assert_eq!(journal(&home), Some(vec![other]));
    assert_eq!(fs::read_to_string(home.path(JOURNAL)).unwrap(), text);
    assert_eq!(
        left(&home),
        ["session.json", "session.json.order-attempts.json"]
    );
}

// The binary reads the saved token once, in the gate, and sends the order call with it (api.rs
// `place`), an intended difference from api.ts that the drop-in case pins.
#[test]
fn an_unreadable_token_refuses_the_money_call_before_any_record_or_send() {
    let unreadable = "Cannot read the Krónan token file. Use a regular file that you own with owner-only permissions (chmod 600 on Unix), not a symlink or hard link.";

    for (tool, path, arguments) in placements() {
        let home = home("unreadable");
        let token: PathBuf = home.path(LEGACY);
        let mode = |mode| fs::set_permissions(&token, fs::Permissions::from_mode(mode)).unwrap();

        // A token file a group can read: refused before the gate.
        mode(0o640);
        let upstream = scripted(|_| None);
        let message = call(&home, &upstream, tool, with(&approval(), arguments.clone()));
        assert_eq!(message, Err(unreadable.to_owned()), "{tool}");
        assert_eq!(upstream.sent(), Vec::<String>::new(), "{tool}");
        assert_eq!(journal(&home), None, "{tool}");

        // The same failure once the gate has read the live checkout: the order call is still sent
        // once, with the token the gate read, and recorded as accepted.
        let later_token = token.clone();
        let later = scripted(move |request| {
            if request.method == "GET" && request.path == "/api/v1/checkout/" {
                fs::set_permissions(&later_token, fs::Permissions::from_mode(0o640)).unwrap();
            }
            None
        });
        mode(0o600);
        let placed = call(&home, &later, tool, with(&approval(), arguments)).unwrap();
        assert_eq!(placed["outcome"], "accepted", "{tool}");
        assert_eq!(posts(&later.sent()), [format!("POST {path}")], "{tool}");
        let attempts = journal(&home).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["state"], "accepted", "{tool}");
        assert_eq!(
            left(&home),
            ["session.json", "session.json.order-attempts.json"]
        );
    }
}

/// Tool calls with their arguments.
type Writes<'a> = &'a [(&'a str, Value)];

#[test]
fn other_writes_are_sent_once_and_report_an_unconfirmed_change() {
    let home = home("writes");
    // 0 stands for a connection failure after the request was sent.
    let status = Arc::new(Mutex::new(0u16));
    let state = status.clone();
    let upstream = scripted(move |request| {
        if request.method == "GET" {
            return None;
        }
        Some(match *state.lock().unwrap() {
            0 => Reply::dropped(),
            404 => Reply::empty(404),
            status => Reply::text(status, TOKEN),
        })
    });
    let note_and_basket = [
        (
            "add_shopping_note_lines",
            json!({"lines": [{"text": "Eggs"}]}),
        ),
        ("clear_shopping_note", json!({"confirm": true})),
        (
            "set_checkout_lines",
            json!({"lines": [{"sku": "SKU-1"}], "replace": false}),
        ),
    ];
    // Placed-order changes move money; after sending, no status proves they were not applied.
    let order_changes = [
        (
            "delete_order_lines",
            json!({"orderToken": ORDER_TOKEN, "lineIds": [1], "confirm": true}),
        ),
        (
            "lower_order_line_quantities",
            json!({"orderToken": ORDER_TOKEN, "lineIds": [1], "quantity": 0, "confirm": true}),
        ),
        (
            "toggle_order_line_substitution",
            json!({"orderToken": ORDER_TOKEN, "lineIds": [1]}),
        ),
    ];
    let order_pattern: &[&str] =
        &["did not confirm this order change; it may have been applied. Read get_order"];
    let mut expectations: Vec<(u16, &[&str], Writes)> = vec![
        (
            0,
            &["did not confirm this change", "may have been applied"],
            &note_and_basket,
        ),
        (500, &["did not confirm this change"], &note_and_basket),
        (
            400,
            &["refused the request; nothing was changed"],
            &note_and_basket,
        ),
    ];

    for code in [0, 400, 404, 409, 422, 429, 500] {
        expectations.push((code, order_pattern, &order_changes));
    }

    for (code, pattern, writes) in expectations {
        *status.lock().unwrap() = code;

        for (tool, arguments) in writes {
            let before = upstream.count();
            let message = call(&home, &upstream, tool, arguments.clone()).unwrap_err();
            assert!(matches(&message, pattern), "{tool} {code}: {message}");
            assert!(!message.contains("Check the connection and try again"));
            let sent = upstream.sent()[before..].to_vec();
            assert_eq!(sent.len(), 1, "{tool} {code}: {sent:?}");
            assert!(!sent[0].starts_with("GET"), "{tool} {code}: {sent:?}");
        }
    }
    assert_eq!(journal(&home), None);
    only_the_token(&home);
}

/// A POST the fake holds until released, reporting when it arrived.
struct Held {
    released: Arc<(Mutex<bool>, std::sync::Condvar)>,
    arrived: std::sync::mpsc::Receiver<()>,
}

impl Held {
    /// A fake that holds each POST until `release`, then answers with `then`, and the fixture
    /// otherwise.
    fn upstream(then: Reply) -> (Upstream, Self) {
        let released = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (arrive, arrived) = std::sync::mpsc::channel();
        let (gate, arrive) = (released.clone(), Mutex::new(arrive));
        let upstream = scripted(move |request| {
            if request.method != "POST" {
                return None;
            }
            let _ = arrive.lock().unwrap().send(());
            let (open, signal) = &*gate;
            let mut open = open.lock().unwrap();

            while !*open {
                open = signal.wait(open).unwrap();
            }
            Some(then.clone())
        });
        (upstream, Self { released, arrived })
    }

    fn release(&self) {
        *self.released.0.lock().unwrap() = true;
        self.released.1.notify_all();
    }
}

/// Waits (bounded) until another process waits for the order-attempt lock: its lock temporary is
/// published beside the record.
fn lock_waiter(home: &Home) {
    let deadline = std::time::Instant::now() + support::PATIENCE;

    while !left(home)
        .iter()
        .any(|name| name.starts_with("session.json.order-attempts.json.lock-tmp."))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no call waited for the lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// integration.test.ts `describeSettled`: an outcome, or the first clause of a refusal.
fn settled(result: &Result<Value, String>) -> String {
    match result {
        Ok(placed) => placed["outcome"].as_str().unwrap().to_owned(),
        Err(message) => format!(
            "rejected: {}",
            message.split(['.', '(']).next().unwrap().trim()
        ),
    }
}

#[test]
fn concurrent_calls_for_one_approval_send_exactly_one_charge_bearing_request() {
    let call_in =
        |home: &Arc<Home>, upstream: &Arc<Upstream>, tool: &'static str, arguments: Value| {
            let (home, upstream) = (home.clone(), upstream.clone());
            std::thread::spawn(move || call(&home, &upstream, tool, with(&approval(), arguments)))
        };
    let complete = json!({"slotId": 501, "addressId": 11, "returnBags": false});

    // Same tool in two processes: the second waits for the lock, then finds the accepted attempt.
    let home = Arc::new(home("concurrent"));
    let (upstream, held) = Held::upstream(fixture("/api/v1/checkout/complete/"));
    let upstream = Arc::new(upstream);
    let first = call_in(&home, &upstream, "complete_checkout", complete.clone());
    let second = call_in(&home, &upstream, "complete_checkout", complete.clone());
    held.arrived.recv_timeout(support::PATIENCE).unwrap();
    lock_waiter(&home);
    held.release();
    let mut outcomes = [first.join().unwrap(), second.join().unwrap()]
        .iter()
        .map(settled)
        .collect::<Vec<_>>();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            "accepted",
            "rejected: Krónan already accepted this order call for this exact checkout"
        ]
    );
    assert_eq!(posts(&upstream.sent()), ["POST /checkout/complete/"]);
    let attempts = journal(&home).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["state"], "accepted");
    assert_eq!(
        left(&home),
        ["session.json", "session.json.order-attempts.json"]
    );

    // Across tools, with the first outcome unknown: the waiting call is blocked, not sent.
    let home = Arc::new(self::home("concurrent-cross"));
    let (upstream, held) = Held::upstream(Reply::dropped());
    let upstream = Arc::new(upstream);
    let first = call_in(&home, &upstream, "complete_checkout", complete);
    held.arrived.recv_timeout(support::PATIENCE).unwrap();
    let second = call_in(
        &home,
        &upstream,
        "reserve_pickup_slot",
        json!({"slotId": 601, "returnBags": false}),
    );
    lock_waiter(&home);
    held.release();
    let mut outcomes = [first.join().unwrap(), second.join().unwrap()]
        .iter()
        .map(settled)
        .collect::<Vec<_>>();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            "rejected: An earlier order call for this checkout is still unresolved",
            "unknown"
        ]
    );
    assert_eq!(posts(&upstream.sent()), ["POST /checkout/complete/"]);
    let attempts = journal(&home).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["tool"], "complete_checkout");
    assert_eq!(attempts[0]["state"], "unknown");
}

#[test]
fn an_unknown_or_submitting_attempt_blocks_every_money_tool_for_that_checkout() {
    let home = home("blocks");
    let upstream = scripted(|request| (request.method == "POST").then(Reply::dropped));
    let first = call(
        &home,
        &upstream,
        "complete_checkout",
        with(&approval(), json!({"slotId": 501, "returnBags": false})),
    )
    .unwrap();
    assert_eq!(first["outcome"], "unknown");
    assert_eq!(posts(&upstream.sent()).len(), 1);
    let recorded = journal(&home).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["tool"], "complete_checkout");
    assert_eq!(recorded[0]["state"], "unknown");
    assert_eq!(recorded[0]["checkoutToken"], CHECKOUT_TOKEN);
    let mode = fs::metadata(home.path(JOURNAL))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    for (tool, _, arguments) in placements() {
        let before = upstream.count();
        let message = call(&home, &upstream, tool, with(&approval(), arguments)).unwrap_err();
        assert!(
            matches(&message, &["still unresolved", "not permission to retry"]),
            "{tool}: {message}"
        );
        assert!(message.contains(NOTHING_SENT), "{message}");
        // Blocked before the gate read: nothing at all reaches Krónan.
        assert_eq!(upstream.sent()[before..], [] as [String; 0], "{tool}");
    }

    // A crash between saving intent and the response leaves `submitting`; it blocks the same way.
    let mut submitting = recorded[0].clone();
    submitting["tool"] = json!("reserve_pickup_slot");
    submitting["state"] = json!("submitting");
    home.write(
        JOURNAL,
        &json!({"version": 1, "attempts": [submitting]}).to_string(),
        None,
    );
    let before = upstream.count();
    let message = call(
        &home,
        &upstream,
        "reserve_delivery_slot",
        with(
            &approval(),
            json!({"slotId": 501, "addressId": 11, "returnBags": false}),
        ),
    )
    .unwrap_err();
    assert!(message.contains("still unresolved"), "{message}");
    assert_eq!(upstream.count(), before);

    // An unreadable record fails closed, and is left for `orders clear-attempts`.
    let unreadable = r#"{"version":1,"attempts":[{"state":"accepted"}]}"#;
    home.write(JOURNAL, unreadable, None);
    let message = call(
        &home,
        &upstream,
        "complete_checkout",
        with(&approval(), json!({"slotId": 501, "returnBags": false})),
    )
    .unwrap_err();
    assert!(
        message.contains("record is unreadable or unsafe. Nothing was sent"),
        "{message}"
    );
    assert_eq!(upstream.count(), before);
    assert_eq!(fs::read_to_string(home.path(JOURNAL)).unwrap(), unreadable);
}

#[test]
fn an_accepted_attempt_blocks_a_repeat_but_allows_complete_after_reserve() {
    let home = home("accepted");
    let upstream = scripted(|_| None);
    let complete = with(
        &approval(),
        json!({"slotId": 501, "addressId": 11, "returnBags": false}),
    );
    let reserved = call(&home, &upstream, "reserve_delivery_slot", complete.clone()).unwrap();
    assert_eq!(reserved["outcome"], "accepted");
    // The live reserve/complete sequence is unverified, so an accepted reserve must not block it.
    let completed = call(&home, &upstream, "complete_checkout", complete.clone()).unwrap();
    assert_eq!(completed["outcome"], "accepted");
    assert_eq!(
        posts(&upstream.sent()),
        ["POST /slots/delivery/reserve/", "POST /checkout/complete/"]
    );

    for (tool, arguments) in [
        ("complete_checkout", complete.clone()),
        ("reserve_delivery_slot", complete.clone()),
        (
            "reserve_pickup_slot",
            with(&approval(), json!({"slotId": 601, "returnBags": false})),
        ),
        (
            "add_checkout_to_order",
            with(&approval(), json!({"expectedOrderToken": ORDER_TOKEN})),
        ),
    ] {
        let before = upstream.count();
        let message = call(&home, &upstream, tool, arguments).unwrap_err();
        assert!(
            message.contains("already accepted this order call"),
            "{tool}: {message}"
        );
        assert!(message.contains(NOTHING_SENT), "{message}");
        assert_eq!(
            posts(&upstream.sent()[before..]),
            Vec::<String>::new(),
            "{tool}"
        );
    }
    let states: Vec<_> = journal(&home)
        .unwrap()
        .iter()
        .map(|attempt| (attempt["tool"].clone(), attempt["state"].clone()))
        .collect();
    assert_eq!(
        states,
        [
            (json!("reserve_delivery_slot"), json!("accepted")),
            (json!("complete_checkout"), json!("accepted"))
        ]
    );

    // A changed checkout (different lines and total) is a new approval and may be sent.
    let home = self::home("changed");
    let current = Arc::new(Mutex::new(checkout()));
    let state = current.clone();
    let changed = scripted(move |request| {
        (request.path == "/api/v1/checkout/")
            .then(|| Reply::json(200, &state.lock().unwrap().clone()))
    });
    let placed = call(&home, &changed, "complete_checkout", complete.clone()).unwrap();
    assert_eq!(placed["outcome"], "accepted");
    let mut more = checkout();
    more["lines"][0]["quantity"] = json!(2);
    more["total"] = json!(1988);
    *current.lock().unwrap() = more;
    let arguments = with(&complete, json!({"expectedTotal": 1988}));
    let placed = call(&home, &changed, "complete_checkout", arguments).unwrap();
    assert_eq!(placed["outcome"], "accepted");
    assert_eq!(posts(&changed.sent()).len(), 2);
    assert_eq!(journal(&home).unwrap().len(), 2);
}

#[test]
fn clear_attempts_lists_the_record_and_clears_it_only_after_an_explicit_yes() {
    let home = home("clear");
    let upstream = scripted(|request| (request.method == "POST").then(Reply::dropped));
    let clear = |stdin: Option<&str>| {
        support::cli(
            &home,
            &upstream.origin,
            &[],
            &["orders", "clear-attempts"],
            stdin,
        )
    };

    let empty = clear(None);
    assert_eq!(empty.code, Some(0));
    assert!(
        empty.stdout.contains("No recorded order attempts"),
        "{}",
        empty.stdout
    );

    let placed = call(
        &home,
        &upstream,
        "complete_checkout",
        with(&approval(), json!({"slotId": 501, "returnBags": false})),
    )
    .unwrap();
    assert_eq!(placed["outcome"], "unknown");
    let recorded = fs::read_to_string(home.path(JOURNAL)).unwrap();

    for input in ["n\n", "\n", "", "yes please\n"] {
        let kept = clear(Some(input));
        assert_eq!(kept.code, Some(0), "{input:?}");
        assert!(
            kept.stdout.contains("complete_checkout  unknown"),
            "{}",
            kept.stdout
        );
        assert!(kept.stdout.contains("Kept the recorded order attempts."));
        assert!(kept.stderr.contains("[y/N]"), "{}", kept.stderr);
        assert_eq!(fs::read_to_string(home.path(JOURNAL)).unwrap(), recorded);
    }

    let cleared = clear(Some("y\n"));
    assert_eq!(cleared.code, Some(0));
    assert!(
        cleared
            .stdout
            .contains("Cleared the recorded order attempts.")
    );
    assert!(!format!("{}{}", cleared.stdout, cleared.stderr).contains(TOKEN));
    assert_eq!(journal(&home), None);

    // An unreadable record can be inspected and cleared the same way.
    home.write(JOURNAL, "not json", None);
    let invalid = clear(Some("Y\n"));
    assert!(
        invalid.stdout.contains("unreadable or unsafe"),
        "{}",
        invalid.stdout
    );
    assert_eq!(journal(&home), None);
    only_the_token(&home);
    // Only the one order call reached Krónan.
    assert_eq!(posts(&upstream.sent()), ["POST /checkout/complete/"]);
}

#[test]
fn a_frozen_typescript_order_attempt_blocks_and_lists_in_the_binary() {
    // What the TypeScript client's `claimAttempt` recorded for an accepted pickup reservation.
    let home = Home::new("frozen-attempt");
    support::plant(&home, "attempt-pickup-accepted");
    let recorded = fs::read_to_string(home.path(JOURNAL)).unwrap();
    let attempt = journal(&home).unwrap()[0].clone();
    let upstream = scripted(|_| None);
    let listed = support::cli(
        &home,
        &upstream.origin,
        &[],
        &["orders", "clear-attempts"],
        Some("n\n"),
    );
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    let line = format!(
        "  {}  reserve_pickup_slot  accepted  checkout checkout-token  total 990 ISK  order order-token\n",
        attempt["createdAt"].as_str().unwrap()
    );
    assert!(listed.stdout.contains(&line), "{}", listed.stdout);
    // Listed and kept byte for byte; nothing reached Krónan.
    assert_eq!(fs::read_to_string(home.path(JOURNAL)).unwrap(), recorded);
    assert_eq!(upstream.sent(), Vec::<String>::new());
}

#[test]
fn an_order_attempt_recorded_by_one_process_blocks_the_next_processs_order_calls() {
    let home = home("next-process");
    let upstream = scripted(|request| (request.method == "POST").then(Reply::dropped));

    // This binary's own money call, whose request may have reached Krónan: an unknown outcome,
    // recorded with the fingerprint the TypeScript client computes for the same checkout.
    let complete = with(&approval(), json!({"slotId": 501, "returnBags": false}));
    let placed = call(&home, &upstream, "complete_checkout", complete).unwrap();
    assert_eq!(placed["outcome"], "unknown");
    let print = support::store_state("attempt-complete-accepted")["print"].clone();
    let recorded = journal(&home).unwrap();
    assert_eq!(recorded.len(), 1);

    for (key, value) in [
        ("tool", json!("complete_checkout")),
        ("checkoutToken", json!(CHECKOUT_TOKEN)),
        ("fingerprint", print),
        ("total", json!(CHECKOUT_TOTAL)),
        ("state", json!("unknown")),
        ("orderToken", Value::Null),
    ] {
        assert_eq!(recorded[0][key], value, "{key}");
    }

    // The next process's order call for that checkout is blocked before anything is sent.
    let before = upstream.count();
    let message = call(
        &home,
        &upstream,
        "reserve_pickup_slot",
        with(&approval(), json!({"slotId": 601, "returnBags": false})),
    )
    .unwrap_err();
    assert!(message.contains("still unresolved"), "{message}");
    assert_eq!(upstream.count(), before);

    // An accepted completion the TypeScript client recorded for the same checkout stops the binary.
    fs::remove_file(home.path(JOURNAL)).unwrap();
    support::plant(&home, "attempt-complete-accepted");
    let frozen = fs::read_to_string(home.path(JOURNAL)).unwrap();
    let message = call(
        &home,
        &upstream,
        "add_checkout_to_order",
        with(&approval(), json!({"expectedOrderToken": ORDER_TOKEN})),
    )
    .unwrap_err();
    assert!(message.contains("already accepted"), "{message}");
    assert_eq!(posts(&upstream.sent()), ["POST /checkout/complete/"]);
    assert_eq!(fs::read_to_string(home.path(JOURNAL)).unwrap(), frozen);
}
