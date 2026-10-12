//! The money cases of tests/ts/money.test.ts, intent.test.ts and interop.test.ts, executed against
//! this binary with the drop-in's synthetic provider (support/provider.rs) on a loopback fake. Each
//! tool call runs in its own `serve`, as the drop-in client made it, so every call is also a
//! restart. Each case checks what the cases checked (states, counters, messages) and the exact
//! `METHOD /path` list the fake saw, the journal record's state and the journal files left. Needs
//! no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use serde_json::{Value, json};
use support::provider::cart;
use support::{Fixture, Home, Mcp, PATIENCE, Reply, payment};

const QUOTE: [&str; 4] = [
    "GET /website/panta/pizzur",
    "GET /api/user/newuser",
    "GET /api/store",
    "POST /api/orders",
];
const SETUP: &str = "POST /adyen/checkoutshopper/v1/sessions/synthetic-session/setup";
const PAY: &str = "POST /adyen/checkoutshopper/v1/sessions/synthetic-session/payments";

fn quote_then(rest: &[&str]) -> Vec<String> {
    QUOTE
        .iter()
        .chain(rest)
        .map(|line| line.to_string())
        .collect()
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|line| line.to_string()).collect()
}

/// A gate the fake holds a request at until `release`; `arrived` sees each held request.
#[derive(Clone)]
struct Gate {
    open: Arc<(Mutex<bool>, Condvar)>,
}

impl Gate {
    fn new() -> Self {
        Self {
            open: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn hold(&self) {
        let (open, signal) = &*self.open;
        let mut open = open.lock().unwrap();

        while !*open {
            open = signal.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.0.lock().unwrap() = true;
        self.open.1.notify_all();
    }
}

/// Another process waits for a lock under `home`: its lock temporary is published beside the
/// journal record or the session store.
fn waiter(home: &std::path::Path) -> bool {
    ["session.json.checkouts", "config/dominos-mcp"]
        .iter()
        .any(|folder| {
            fs::read_dir(home.join(folder)).is_ok_and(|mut entries| {
                entries.any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains(".lock-tmp.")
                })
            })
        })
}

/// Waits (bounded) until `check` holds.
fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;

    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// money.test.ts:21
#[test]
fn a_401_during_checkout_or_payment_is_never_retried() {
    let reject = Arc::new(Mutex::new(String::new()));
    let once = reject.clone();
    let fixture = Fixture::new(move |_, request| {
        let mut reject = once.lock().unwrap();
        let path = request.path.split('?').next().unwrap();

        (!reject.is_empty() && path.ends_with(reject.as_str())).then(|| {
            reject.clear();
            Reply::text(401, "")
        })
    });
    let mut since = 0;
    let quote = fixture.quote();
    assert_eq!(fixture.sent(&mut since), quote_then(&[]));

    *reject.lock().unwrap() = "/api/user/newuser".into();
    let refused = fixture.create(&quote).unwrap_err();
    assert!(refused.contains("No automatic retry was made"), "{refused}");
    assert_eq!(
        fixture.sent(&mut since),
        lines(&["GET /website/panta/pizzur", "GET /api/user/newuser"])
    );
    assert_eq!(fixture.provider.state().refreshes, 0);
    assert_eq!(fixture.provider.state().orders, 0);

    let checkout = fixture.create(&quote).unwrap();
    assert_eq!(checkout["state"], "ready");
    assert_eq!(fixture.sent(&mut since), quote_then(&[SETUP]));

    *reject.lock().unwrap() = "/payments".into();
    let paid = fixture.pay(&checkout).unwrap();
    // The payment's 401 never reaches a retry: the attempt is recorded as unknown.
    assert_eq!(paid["state"], "unknown");
    assert_eq!(fixture.sent(&mut since), lines(&[PAY]));
    assert_eq!(fixture.journal(&checkout["checkoutId"])["state"], "unknown");
    let state = fixture.provider.state();
    assert_eq!((state.payments, state.refreshes, state.orders), (0, 0, 1));
    drop(state);
    assert_eq!(
        fixture.left(),
        fixture.records(&[&quote["quoteId"], &checkout["checkoutId"]])
    );
    fixture.provider.check();
}

// money.test.ts:112
#[test]
fn checkout_is_bound_to_the_quote_and_permits_one_confirmed_payment_attempt() {
    // The first final order and the first payment are held until the other process waits for a
    // lock, so the two calls overlap instead of merely racing.
    let home_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let held = Arc::new(Mutex::new(Vec::<String>::new()));
    let (home_of, failures) = (home_slot.clone(), held.clone());
    let fixture = Arc::new(Fixture::new(move |_, request| {
        let path = request.path.split('?').next().unwrap();
        let send = match path {
            "/api/orders" => {
                serde_json::from_str::<Value>(&request.body).unwrap()["IsFinal"] == true
            }
            _ => path.ends_with("/payments"),
        };
        let mut seen = failures.lock().unwrap();

        if send && !seen.contains(&path.to_owned()) {
            seen.push(path.to_owned());
            drop(seen);
            let home = home_of.lock().unwrap().clone().unwrap();
            until("the other process waits for a lock", || waiter(&home));
        }
        None
    }));
    *home_slot.lock().unwrap() = Some(fixture.home.0.clone());
    let quote = fixture.quote();
    assert_eq!(fixture.provider.state().orders, 0);

    let both = |name: &'static str, arguments: Value| {
        let calls: Vec<_> = (0..2)
            .map(|_| {
                let (fixture, arguments) = (fixture.clone(), arguments.clone());
                std::thread::spawn(move || fixture.call(name, arguments))
            })
            .collect();
        let mut results = calls.into_iter().map(|call| call.join().unwrap());
        (results.next().unwrap(), results.next().unwrap())
    };
    let input = json!({"quoteId": quote["quoteId"], "expectedTotal": 2500});
    let (checkout, repeated) = both("create_checkout", input);
    let (checkout, repeated) = (checkout.unwrap(), repeated.unwrap());
    assert_eq!(checkout, repeated);
    assert_eq!(fixture.provider.state().orders, 1);
    assert_eq!(checkout["state"], "ready");
    assert_eq!(checkout["cart"], quote["cart"]);
    assert_eq!(checkout["cards"][0]["cardId"], "card_1");

    let mut unconfirmed = payment(&checkout);
    unconfirmed["confirm"] = json!(false);
    assert!(fixture.call("pay_saved_card", unconfirmed).is_err());
    let mut changed = payment(&checkout);
    changed["expectedTotal"] = json!(2501);
    let refused = fixture.call("pay_saved_card", changed).unwrap_err();
    assert!(refused.contains("amount differs"), "{refused}");
    assert_eq!(fixture.provider.state().payments, 0);

    let mut since = fixture.upstream.count();
    let (paid, retried) = both("pay_saved_card", payment(&checkout));
    assert_eq!(paid.unwrap()["state"], "authorised");
    assert_eq!(retried.unwrap()["state"], "authorised");
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(fixture.sent(&mut since), lines(&[PAY]));
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    assert_eq!(
        fixture.upstream.sent(),
        quote_then(&[
            "GET /website/panta/pizzur",
            "GET /api/user/newuser",
            "GET /api/store",
            "POST /api/orders",
            SETUP,
            PAY
        ])
        .into_iter()
        .collect::<Vec<_>>()
    );
    assert_eq!(
        fixture.left(),
        fixture.records(&[&quote["quoteId"], &checkout["checkoutId"]])
    );
    fixture.provider.check();
}

// money.test.ts:172
#[test]
fn lost_order_creation_and_bank_verification_never_allow_another_attempt() {
    let fixture = Fixture::plain();
    let mut since = 0;
    let quote = fixture.quote();
    fixture.provider.state().lose_order_response = true;
    let checkout = fixture.create(&quote).unwrap();
    assert_eq!(checkout["state"], "unknown");
    assert_eq!(
        fixture.create(&quote).unwrap()["checkoutId"],
        checkout["checkoutId"]
    );
    assert_eq!(fixture.provider.state().orders, 1);
    assert_eq!(fixture.sent(&mut since), quote_then(&QUOTE));
    assert_eq!(fixture.journal(&checkout["checkoutId"])["state"], "unknown");

    fixture.provider.state().lose_order_response = false;
    fixture.provider.state().accept_checkout = false;
    let rejected_quote = fixture.quote();
    let rejected = fixture.create(&rejected_quote).unwrap();
    assert_eq!(rejected["state"], "unknown");
    fixture.pay(&rejected).unwrap();
    assert_eq!(fixture.provider.state().payments, 0);
    assert_eq!(fixture.sent(&mut since), quote_then(&QUOTE));

    fixture.provider.state().accept_checkout = true;
    let next = fixture.quote();
    let ready = fixture.create(&next).unwrap();
    fixture.provider.state().require_verification = true;
    let challenged = fixture.pay(&ready).unwrap();
    assert_eq!(challenged["state"], "requires_action");
    assert_eq!(fixture.pay(&ready).unwrap()["state"], "requires_action");
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(
        fixture.sent(&mut since),
        quote_then(&[QUOTE[0], QUOTE[1], QUOTE[2], QUOTE[3], SETUP, PAY])
    );
    assert_eq!(
        fixture.journal(&ready["checkoutId"])["state"],
        "requires_action"
    );
    assert_eq!(
        fixture.left(),
        fixture.records(&[
            &quote["quoteId"],
            &checkout["checkoutId"],
            &rejected_quote["quoteId"],
            &rejected["checkoutId"],
            &next["quoteId"],
            &ready["checkoutId"],
        ])
    );
    fixture.provider.check();
}

// money.test.ts:228
#[test]
fn an_amount_mismatch_prevents_charging_and_uncertain_payments_survive_restarts() {
    let fixture = Fixture::plain();
    let mut since = 0;
    fixture.provider.state().minor_amount = 2500;
    let quote = fixture.quote();
    let changed = fixture.create(&quote).unwrap();
    assert_eq!(changed["state"], "amount_changed");
    fixture.pay(&changed).unwrap();
    assert_eq!(fixture.provider.state().payments, 0);
    assert_eq!(
        fixture.sent(&mut since),
        quote_then(&QUOTE)
            .into_iter()
            .chain([SETUP.to_owned()])
            .collect::<Vec<_>>()
    );

    fixture.provider.state().minor_amount = 250_000;
    let next = fixture.quote();
    let checkout = fixture.create(&next).unwrap();
    fixture.provider.state().lose_payment_response = true;
    // Each call is its own process: every one after the lost response is a restart.
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "unknown");
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "unknown");
    let get = json!({"checkoutId": checkout["checkoutId"]});
    assert_eq!(
        fixture.call("get_checkout", get.clone()).unwrap()["state"],
        "unknown"
    );
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(fixture.journal(&checkout["checkoutId"])["state"], "unknown");
    let mut empty = cart();
    for key in ["pizzas", "sides", "beverages", "packages"] {
        empty[key] = json!([]);
    }
    assert!(fixture.call("quote_order", empty).is_err());

    fixture.provider.state().latest_paid = true;
    assert_eq!(
        fixture.call("get_checkout", get).unwrap()["state"],
        "authorised"
    );
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "authorised");
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    assert_eq!(
        fixture.sent(&mut since),
        quote_then(&[
            QUOTE[0],
            QUOTE[1],
            QUOTE[2],
            QUOTE[3],
            SETUP,
            PAY,
            "GET /api/orders/cart/latest",
            "GET /api/orders/cart/latest"
        ])
    );
    fixture.provider.check();
}

/// The intent case fails the write after the send with a read-only (0500) journal folder. That
/// stops every user but root, so the case cannot run as root: say so instead of failing later.
fn read_only_folders_stop_this_user(probe: &std::path::Path) {
    fs::create_dir(probe).unwrap();
    fs::set_permissions(probe, fs::Permissions::from_mode(0o500)).unwrap();
    let refused = fs::write(probe.join("written"), "").is_err();
    fs::set_permissions(probe, fs::Permissions::from_mode(0o700)).unwrap();
    let _ = fs::remove_file(probe.join("written"));
    fs::remove_dir(probe).unwrap();
    assert!(
        refused,
        "a 0500 folder must refuse this user's writes; root is not refused, so run as another user"
    );
}

/// intent.test.ts:11: once the intent is committed and the send reached the provider, the journal
/// folder turns read-only, so the outcome cannot be written.
fn retains_intent(phase: &'static str) {
    let inject = Arc::new(Mutex::new(false));
    let intent = Arc::new(Mutex::new(None::<PathBuf>));
    let (armed, found) = (inject.clone(), intent.clone());
    let pending = if phase == "pay" {
        "submitting"
    } else {
        "creating"
    };
    let home_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let directory_of = home_slot.clone();
    let fixture = Fixture::new(move |provider, request| {
        let path = request.path.split('?').next().unwrap();
        let trigger = match phase {
            "pay" => path.ends_with("/payments"),
            _ => {
                path == "/api/orders"
                    && serde_json::from_str::<Value>(&request.body).unwrap()["IsFinal"] == true
            }
        };
        let reply = provider.answer(request);

        if *armed.lock().unwrap() && trigger {
            let directory = directory_of.lock().unwrap().clone().unwrap();

            for entry in fs::read_dir(&directory).unwrap() {
                let file = entry.unwrap().path();

                if file.extension().is_some_and(|ext| ext == "json")
                    && serde_json::from_str::<Value>(&fs::read_to_string(&file).unwrap()).unwrap()["state"]
                        == pending
                {
                    *found.lock().unwrap() = Some(file);
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
                }
            }
            assert!(
                found.lock().unwrap().is_some(),
                "the intent must be committed before the send reaches the fake"
            );
        }
        Some(reply)
    });
    *home_slot.lock().unwrap() = Some(fixture.checkouts());
    read_only_folders_stop_this_user(&fixture.home.path("probe"));
    let quote = fixture.quote();
    let mut since = fixture.upstream.count();
    let directory = fixture.checkouts();

    let retry = || -> Value {
        let file = intent.lock().unwrap().clone().expect("no intent file");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();

            if path.extension().is_some_and(|ext| ext == "lock") {
                fs::remove_dir(path).unwrap();
            }
        }
        let record: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(record["state"], pending);
        record
    };

    if phase == "create" {
        *inject.lock().unwrap() = true;
        let refused = fixture.create(&quote).unwrap_err();
        assert!(
            refused.contains("Cannot safely access the local Domino’s session"),
            "{refused}"
        );
        retry();
        assert_eq!(fixture.create(&quote).unwrap()["state"], "creating");
        assert_eq!(fixture.sent(&mut since), quote_then(&[]));
        assert_eq!(fixture.provider.state().orders, 1);
        assert_eq!(fixture.provider.state().payments, 0);
    } else {
        let checkout = fixture.create(&quote).unwrap();
        since = fixture.upstream.count();
        *inject.lock().unwrap() = true;
        let refused = fixture.pay(&checkout).unwrap_err();
        assert!(
            refused.contains("Cannot safely access the local Domino’s session"),
            "{refused}"
        );
        let record = retry();
        assert_eq!(record["id"], checkout["checkoutId"]);
        assert_eq!(fixture.pay(&checkout).unwrap()["state"], "submitting");
        assert_eq!(fixture.sent(&mut since), lines(&[PAY]));
        assert_eq!(fixture.provider.state().payments, 1);
    }
    fixture.provider.check();
}

// intent.test.ts:11 (the Rust half; no TypeScript writer remains)
#[test]
fn checkout_and_payment_retain_intent_when_post_send_persistence_fails() {
    retains_intent("create");
    retains_intent("pay");
}

/// A fixture whose provider holds the payment at `gate`, after `arrived` reports it.
fn held_payment(answer_first: bool) -> (Fixture, Gate, std::sync::mpsc::Receiver<()>) {
    let gate = Gate::new();
    let (arrive, arrived) = channel();
    let (held, arrive) = (gate.clone(), Mutex::new(arrive));
    let fixture = Fixture::new(move |provider, request| {
        if !request
            .path
            .split('?')
            .next()
            .unwrap()
            .ends_with("/payments")
        {
            return None;
        }
        let reply = answer_first.then(|| provider.answer(request));

        if !answer_first {
            provider.state().payments += 1;
        }
        arrive.lock().unwrap().send(()).unwrap();
        held.hold();
        Some(reply.unwrap_or_else(|| Reply::empty(200)))
    });
    (fixture, gate, arrived)
}

// intent.test.ts:92
#[test]
fn a_killed_payment_process_leaves_submitting_intent_and_a_restart_sends_nothing() {
    let (fixture, gate, arrived) = held_payment(false);
    let quote = fixture.quote();
    let checkout = fixture.create(&quote).unwrap();
    let mut since = fixture.upstream.count();
    let (mut mcp, _) = Mcp::start(
        &fixture.home,
        &fixture.upstream.origin,
        &fixture.home.with_session(),
    );
    mcp.begin("pay_saved_card", payment(&checkout));
    arrived.recv_timeout(PATIENCE).unwrap();
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "submitting"
    );
    mcp.kill();
    assert_eq!(mcp.next_line(), None, "a killed server answers nothing");
    // The killed holder's lock folder names a pid that no longer runs, so the restart removes it,
    // takes the lock, and sends nothing.
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "submitting");
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(fixture.sent(&mut since), lines(&[PAY]));
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "submitting"
    );
    gate.release();
    fixture.provider.check();
}

// intent.test.ts:147
#[test]
fn host_cancellation_does_not_change_the_payment_outcome_or_abort_the_provider() {
    let (fixture, gate, arrived) = held_payment(true);
    let quote = fixture.quote();
    let checkout = fixture.create(&quote).unwrap();
    let mut since = fixture.upstream.count();
    let (mut mcp, _) = Mcp::start(
        &fixture.home,
        &fixture.upstream.origin,
        &fixture.home.with_session(),
    );
    let id = mcp.begin("pay_saved_card", payment(&checkout));
    arrived.recv_timeout(PATIENCE).unwrap();
    mcp.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": id, "reason": "The host cancelled."}
    }));
    // The ping's answer comes after the server read the cancellation.
    mcp.request("ping", json!({}));
    gate.release();
    until("the payment outcome is recorded", || {
        fixture.journal(&checkout["checkoutId"])["state"] != "submitting"
    });
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    let (code, _) = mcp.close();
    assert_eq!(code, Some(0));
    assert_eq!(fixture.upstream.abandoned(), Vec::<String>::new());
    assert_eq!(
        fixture
            .call(
                "get_checkout",
                json!({"checkoutId": checkout["checkoutId"]})
            )
            .unwrap()["state"],
        "authorised"
    );
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(
        fixture.sent(&mut since),
        lines(&[PAY, "GET /api/orders/cart/latest"])
    );
    fixture.provider.check();
}

/// The public checkout must never carry the journal's private Adyen values.
fn public(value: &Value) -> &Value {
    assert!(!value.to_string().contains("private-"), "{value}");
    value
}

// interop.test.ts:45 (the TypeScript-writer half; the Rust-writer half is every case above, whose
// records the TypeScript client no longer reads)
#[test]
fn frozen_typescript_quote_and_checkout_records_continue_without_a_second_send() {
    // A TypeScript quote: the binary creates its checkout, once, and pays it, once.
    let (home, frozen) = support::plant("quoted");
    let fixture = Fixture::in_home(home, |_, _| None);
    let quote = &frozen["result"]["quote"];
    let checkout = fixture.create(quote).unwrap();
    assert_eq!(public(&checkout)["state"], "ready");
    let get = json!({"checkoutId": checkout["checkoutId"]});
    assert_eq!(fixture.call("get_checkout", get.clone()).unwrap(), checkout);
    assert_eq!(fixture.create(quote).unwrap(), checkout);
    assert_eq!(fixture.provider.state().orders, 1);
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "authorised");
    assert_eq!(fixture.pay(&checkout).unwrap()["state"], "authorised");
    assert_eq!(fixture.provider.state().payments, 1);
    assert_eq!(
        public(&fixture.call("get_checkout", get).unwrap())["state"],
        "authorised"
    );
    // A read of a non-ready checkout asks Domino’s for its latest state, as the package did.
    assert_eq!(
        fixture.upstream.sent(),
        quote_then(&[SETUP, PAY, "GET /api/orders/cart/latest"])
    );
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    assert_eq!(
        fixture.left(),
        fixture.records(&[&quote["quoteId"], &checkout["checkoutId"]])
    );
    fixture.provider.check();

    // A TypeScript checkout: the binary reads it as the TypeScript client returned it, creates
    // nothing, and pays it once.
    let (home, frozen) = support::plant("checkout-ready");
    let fixture = Fixture::in_home(home, |_, _| None);
    let (quote, checkout) = (&frozen["result"]["quote"], &frozen["result"]["checkout"]);
    assert_eq!(checkout["state"], "ready");
    let get = json!({"checkoutId": checkout["checkoutId"]});
    assert_eq!(
        &fixture.call("get_checkout", get.clone()).unwrap(),
        checkout
    );
    assert_eq!(&fixture.create(quote).unwrap(), checkout);
    assert_eq!(fixture.upstream.sent(), Vec::<String>::new());
    assert_eq!(fixture.pay(checkout).unwrap()["state"], "authorised");
    assert_eq!(fixture.pay(checkout).unwrap()["state"], "authorised");
    assert_eq!(
        public(&fixture.call("get_checkout", get).unwrap())["state"],
        "authorised"
    );
    assert_eq!(
        fixture.upstream.sent(),
        lines(&[PAY, "GET /api/orders/cart/latest"])
    );
    let state = fixture.provider.state();
    assert_eq!((state.orders, state.payments), (0, 1));
    drop(state);
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    assert_eq!(
        fixture.left(),
        fixture.records(&[&quote["quoteId"], &checkout["checkoutId"]])
    );
    fixture.provider.check();

    // A checkout the TypeScript client paid: the binary sends no order or payment.
    let (home, frozen) = support::plant("checkout-authorised");
    let fixture = Fixture::in_home(home, |_, _| None);
    let (quote, checkout) = (&frozen["result"]["quote"], &frozen["result"]["checkout"]);
    assert_eq!(frozen["result"]["paid"]["state"], "authorised");
    let before = fs::read(
        fixture
            .checkouts()
            .join(format!("{}.json", checkout["checkoutId"].as_str().unwrap())),
    )
    .unwrap();
    assert_eq!(fixture.pay(checkout).unwrap(), frozen["result"]["paid"]);
    assert_eq!(fixture.create(quote).unwrap()["state"], "authorised");
    let get = json!({"checkoutId": checkout["checkoutId"]});
    assert_eq!(
        public(&fixture.call("get_checkout", get).unwrap())["state"],
        "authorised"
    );
    assert_eq!(
        fixture.upstream.sent(),
        lines(&["GET /api/orders/cart/latest"])
    );
    let after = fs::read(
        fixture
            .checkouts()
            .join(format!("{}.json", checkout["checkoutId"].as_str().unwrap())),
    )
    .unwrap();
    assert_eq!(after, before, "the paid record is left byte for byte");
    fixture.provider.check();
}

/// Another process that shares this session file but not this home's store: its own
/// `XDG_CONFIG_HOME` and `XDG_DATA_HOME` (another MCP host's environment), so its session lock is
/// another store's lock.
fn call_elsewhere(
    fixture: &Fixture,
    home: &Home,
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let (mut mcp, _) = Mcp::start(home, &fixture.upstream.origin, &fixture.home.with_session());
    let answer = mcp.call(name, arguments);
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{name}: {stderr}");
    answer.outcome()
}

// Not a TypeScript case. Processes whose config homes differ hold different session locks but share
// the journal beside DOMINOS_SESSION_FILE; there only the checkout's own lock allows one payment.
#[test]
fn processes_with_different_stores_sharing_a_session_file_pay_a_checkout_once() {
    const PROCESSES: usize = 4;
    // Each process refreshes the expired plaintext session first. The token answers are held
    // until every process has asked or another one waits for the checkout's lock, so without
    // that lock all of them read the ready checkout at once.
    let armed = Arc::new(Mutex::new(false));
    let arrived = Arc::new(Mutex::new(0usize));
    let folder: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let (hold, count, journal) = (armed.clone(), arrived.clone(), folder.clone());
    let fixture = Arc::new(Fixture::setup(false, true, move |_, request| {
        if request.path == "/api/token" && *hold.lock().unwrap() {
            *count.lock().unwrap() += 1;
            let home = journal.lock().unwrap().clone().unwrap();
            until("every process asked or one waits for the checkout", || {
                *count.lock().unwrap() == PROCESSES || waiter(&home)
            });
        }
        None
    }));
    *folder.lock().unwrap() = Some(fixture.home.0.clone());
    let quote = fixture.quote();
    let checkout = fixture.create(&quote).unwrap();
    // The plaintext session expires again, so every process starts with a refresh.
    fixture.home.legacy(true, "synthetic");
    *armed.lock().unwrap() = true;
    let mut since = fixture.upstream.count();
    let calls: Vec<_> = (0..PROCESSES)
        .map(|_| {
            let (fixture, checkout) = (fixture.clone(), checkout.clone());
            std::thread::spawn(move || {
                let home = Home::new("elsewhere");
                call_elsewhere(&fixture, &home, "pay_saved_card", payment(&checkout))
            })
        })
        .collect();

    let states: Vec<Value> = calls
        .into_iter()
        .map(|call| call.join().unwrap().unwrap()["state"].clone())
        .collect();
    assert_eq!(fixture.provider.state().payments, 1, "{states:?}");
    assert_eq!(states, vec![json!("authorised"); PROCESSES]);
    // The first process to hold the checkout refreshed the shared file; the others read it fresh.
    assert_eq!(fixture.sent(&mut since), lines(&["POST /api/token", PAY]));
    assert_eq!(
        fixture.journal(&checkout["checkoutId"])["state"],
        "authorised"
    );
    assert_eq!(
        fixture.left(),
        fixture.records(&[&quote["quoteId"], &checkout["checkoutId"]])
    );
    fixture.provider.check();
}
