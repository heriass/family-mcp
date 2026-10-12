//! The frozen TypeScript outcomes of every tests/ts/parity.ts case (with reads-parity.ts,
//! auth-parity.ts and every money-parity.ts scenario) and of the drop-in cases the ledger names
//! `dominos/contract/*`, replayed through this binary step by step (fixtures/parity and
//! fixtures/contract, PROVENANCE.md in each). Each case runs in one or more fresh homes. A fake
//! Domino’s serves the recorded responses in order and requires each request to equal the
//! recorded one (method, path and query, the four headers, body), so every step checks the full
//! request list, its count and its order, not only the output. Each CLI run compares its exit code,
//! stdout and stderr; each tool call, made in its own `serve` as the drop-in client made it, its
//! raw JSON-RPC answer; each home ends by comparing the stored session, every path left with its
//! mode, and the text of every quote and checkout journal file. No output may hold a secret the
//! parity scripts guarded. Timing and restart behaviour (a lost response, a kill mid-send,
//! concurrent refresh) is not replayed: money.rs and session.rs execute it. Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use serde_json::{Value, json};
use support::{Home, Mcp, Names, Script, cli, no_leak, unpack};

/// The parity cases, one file each: the argument list, the surface, the invalid inputs, the
/// reads, the logins, the unsafe folder, and the 54 money scenarios under their ledger names.
const CASES: [&str; 60] = [
    "cli-argument-list",
    "tool-surface-and-instructions",
    "invalid-tool-input-list",
    "reads-parity",
    "auth-parity",
    "unsafe-config-folder-refusal",
    "quote-pickup",
    "quote-delivery",
    "quote-menu-selection-refusal",
    "quote-zero",
    "quote-fraction",
    "quote-401",
    "store-disabled",
    "create-ready",
    "quote-account-changed",
    "quote-confirmed-total-changed",
    "quote-expired",
    "quote-already-used-despite-expiry",
    "quote-linked-checkout-missing",
    "order-lost",
    "order-rejected",
    "order-schema",
    "order-total",
    "setup-amount",
    "setup-currency",
    "setup-id",
    "setup-date",
    "setup-schema",
    "payment-confirmed-total-changed",
    "payment-account-changed",
    "payment-expired",
    "payment-card-missing",
    "payment-card-numeric-overflow",
    "payment-empty-session-refuses-without-send",
    "payment-already-creating",
    "payment-already-submitting",
    "payment-already-authorised",
    "payment-already-refused",
    "payment-already-pending",
    "payment-already-requires-action",
    "payment-already-unknown",
    "payment-already-amount-changed",
    "authorised",
    "refused",
    "cancelled",
    "error",
    "pending",
    "action",
    "payment-schema",
    "payment-401",
    "payment-lost",
    "get-different-account",
    "get-unknown-unpaid",
    "get-unknown-paid",
    "get-nonempty-string-invalid-latest",
    "get-empty-string-latest",
    "get-ready-never-reconciles",
    "local-record-invalid-schema",
    "local-record-invalid-json",
    "local-record-oversized",
];

/// The drop-in cases the ledger names `dominos/contract/*`, one file each.
const CONTRACTS: [&str; 4] = [
    "no-session-commands",
    "auth-errors-hide-credentials",
    "offer-slots",
    "public-menu-and-mcp-reads",
];

fn frozen(set: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(set)
        .join(format!("{name}.json"));
    unpack(serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
}

/// What the replay covered: homes, CLI runs, tool results, and those with structured content.
#[derive(Default, Debug)]
struct Coverage {
    homes: usize,
    cli: usize,
    tools: usize,
    structured: usize,
}

/// Replay one home of `name`, failing at the first difference.
fn replay_home(name: &str, number: usize, spec: &Value, coverage: &mut Coverage) {
    coverage.homes += 1;
    let home = Home::new(name);
    let mut names = Names::new(&home);
    let script = Script::default();
    let upstream = script.upstream();
    let origin = upstream.origin.clone();
    let extra: Vec<(String, String)> = spec["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().unwrap();
            (
                key.clone(),
                value.replace("<home>", &home.0.display().to_string()),
            )
        })
        .collect();
    let mut served = 0;

    for (index, step) in spec["steps"].as_array().unwrap().iter().enumerate() {
        let at = format!("{name} home {number} step {index}");

        if let Some(seed) = step.get("seed") {
            home.seed(seed["expired"] == true);
        } else if step.get("mode").is_some() && step.get("dir").is_none() {
            // The recorded exchanges already carry what the fake's mode answered.
        } else if let Some(directory) = step["dir"].as_str() {
            DirBuilder::new()
                .recursive(true)
                .mode(step["mode"].as_u64().unwrap() as u32)
                .create(home.path(directory))
                .unwrap();
        } else if let Some(file) = step["file"].as_str() {
            home.write(file, step["text"].as_str().unwrap());
        } else if let Some(args) = step["cli"].as_array() {
            coverage.cli += 1;
            let args: Vec<&str> = args.iter().map(|arg| arg.as_str().unwrap()).collect();
            // A contract step's own environment; `<home>` in a value is this home.
            let mut extra = extra.clone();

            if let Some(own) = step.get("env") {
                for (key, value) in own.as_object().unwrap() {
                    let value = value.as_str().unwrap();
                    let value = value.replace("<home>", &home.0.display().to_string());
                    extra.push((key.clone(), value));
                }
            }
            script.expect(&step["exchanges"]);
            let run = cli(&home, &origin, &extra, &args, step["stdin"].as_str());
            script.finish(&format!("{at} {args:?}"));
            served += step["exchanges"].as_array().unwrap().len();
            no_leak(&format!("{}{}", run.stdout, run.stderr), &at);
            assert_eq!(
                json!({"code": run.code, "stdout": names.fresh(&run.stdout), "stderr": names.fresh(&run.stderr)}),
                json!({"code": step["code"], "stdout": step["stdout"], "stderr": step["stderr"]}),
                "{at} {args:?}"
            );
        } else {
            assert_eq!(step["serve"], true, "{at}: unknown step {step}");
            let (mut mcp, initialized) = Mcp::start(&home, &origin, &extra);

            if let Some(expected) = step.get("initialize") {
                assert_eq!(
                    envelope(&initialized.value()),
                    envelope(expected),
                    "{at} initialize"
                );
                let listed = mcp.request("tools/list", json!({}));
                // Schema key order differs between rmcp and zod; parity.ts compared the listed
                // tools as values, and tests/surface.json pins the contract on its own.
                assert_eq!(
                    envelope(&listed.value()),
                    envelope(&step["list"]),
                    "{at} tools/list"
                );
                assert_eq!(
                    listed.value()["result"]["tools"].as_array().unwrap().len(),
                    13
                );
            }
            for (call_number, result) in step["results"].as_array().unwrap().iter().enumerate() {
                let call = &result["call"];
                let at = format!("{at} call {call_number} {}", call[0]);
                script.expect(&result["exchanges"]);
                let got = mcp.call(call[0].as_str().unwrap(), call[1].clone());
                script.finish(&at);
                served += result["exchanges"].as_array().unwrap().len();
                no_leak(&format!("{}{}", got.line, got.notifications.join("")), &at);
                let notifications: Vec<Value> = got
                    .notifications
                    .iter()
                    .map(|line| serde_json::from_str(&names.fresh(line)).unwrap())
                    .collect();
                let frozen_notifications = result
                    .get("notifications")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                assert_eq!(
                    json!(notifications),
                    frozen_notifications,
                    "{at}: notifications"
                );
                let answer: Value = serde_json::from_str(&names.fresh(&got.line)).unwrap();
                let frozen = &result["answer"];
                assert_eq!(envelope(&answer), envelope(frozen), "{at}");
                // A tool answer's key order is part of the contract.
                assert_eq!(
                    envelope(&answer).to_string(),
                    envelope(frozen).to_string(),
                    "{at}: key order"
                );

                if name == "invalid-tool-input-list" {
                    assert_eq!(answer["result"]["isError"], true, "{at}: must be invalid");
                }
                if frozen["result"].get("content").is_some() {
                    coverage.tools += 1;
                }
                if frozen["result"].get("structuredContent").is_some() {
                    coverage.structured += 1;
                }
            }
            let (code, stderr) = mcp.close();
            no_leak(&stderr, &at);
            assert_eq!(
                json!({"code": code, "stderr": names.fresh(&stderr)}),
                json!({"code": step["code"], "stderr": step["stderr"]}),
                "{at} exit"
            );
        }
    }
    let at = format!("{name} home {number}");
    assert_eq!(served as u64, spec["requests"].as_u64().unwrap(), "{at}");
    assert_eq!(upstream.count(), served, "{at}: requests");
    assert_eq!(home.stored(&mut names), spec["store"], "{at}: store");
    assert_eq!(home.files(&mut names), spec["files"], "{at}: files");
}

/// A JSON-RPC message without its envelope: `{result}` or `{error}`.
fn envelope(message: &Value) -> Value {
    let mut answer = message.as_object().unwrap().clone();
    assert_eq!(answer.remove("jsonrpc"), Some(json!("2.0")));
    assert!(answer.remove("id").is_some());
    Value::Object(answer)
}

/// The `.json` files in `fixtures/<set>`, which must be exactly `names`.
fn fixtures_are(set: &str, names: &[&str]) {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(set);
    let mut files: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".json"))
        .collect();
    files.sort();
    let mut expected: Vec<String> = names.iter().map(|name| format!("{name}.json")).collect();
    expected.sort();
    assert_eq!(files, expected);
}

fn replay(set: &str, names: &[&str]) -> Coverage {
    fixtures_are(set, names);
    let mut coverage = Coverage::default();

    for name in names {
        let case = frozen(set, name);
        assert_eq!(case["id"], format!("dominos/{set}/{name}"));

        for (number, spec) in case["homes"].as_array().unwrap().iter().enumerate() {
            replay_home(name, number, spec, &mut coverage);
        }
    }
    coverage
}

#[test]
fn every_frozen_parity_scenario_replays() {
    let coverage = replay("parity", &CASES);
    // What the freeze recorded, so a fixture emptied by mistake fails here.
    assert_eq!(
        (
            coverage.homes,
            coverage.cli,
            coverage.tools,
            coverage.structured
        ),
        (71, 34, 124, 46),
        "{coverage:?}"
    );
}

#[test]
fn every_frozen_contract_case_replays() {
    let coverage = replay("contract", &CONTRACTS);
    assert_eq!(
        (
            coverage.homes,
            coverage.cli,
            coverage.tools,
            coverage.structured
        ),
        (8, 5, 9, 3),
        "{coverage:?}"
    );
}
