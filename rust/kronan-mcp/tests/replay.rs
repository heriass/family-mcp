//! The frozen TypeScript outcomes of every tests/ts/parity.ts scenario, and of the contract cases
//! of tests/ts/integration.test.ts, replayed through this binary step by step (fixtures/parity and
//! fixtures/contract, PROVENANCE.md in each). A fake Krónan serves the recorded
//! responses in order and requires each request to equal the recorded one (method, path and query,
//! the four headers, body), so every step checks the full request list, its count and its order,
//! not only the output. Each CLI run compares its exit code, stdout and stderr; each MCP request
//! its raw JSON-RPC answer; each scenario ends by comparing the stored record and the files left
//! under `.config/kronan-mcp`. Timing and restart behaviour (a lost response, a kill mid-send, lock
//! takeover) is not replayed: money.rs and session.rs execute it. Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::panic::catch_unwind;
use std::path::Path;

use serde_json::{Value, json};
use support::{Home, Mcp, Script, cli, fresh, unpack};

/// The scenarios parity.ts ran, which the freeze wrote one file each.
const SCENARIOS: [&str; 22] = [
    "surface",
    "cli",
    "reads",
    "writes",
    "write-inputs",
    "write-statuses",
    "orders",
    "orders-store",
    "orders-migrated",
    "order-failures",
    "inputs",
    "edge",
    "mismatch",
    "statuses",
    "store",
    "tokens",
    "auth",
    "legacy",
    "legacy-invalid",
    "attempts",
    "attempts-file",
    "token-file",
];

/// The integration.test.ts cases the ledger names `kronan/contract/*`, one file each, and the
/// unknown-key inputs of the seven tools no other frozen case gives an invalid input.
const CONTRACTS: [&str; 10] = [
    "offset-pages",
    "oversized-token-input",
    "published-paths-and-bodies",
    "write-inputs",
    "safe-errors-and-strict-inputs",
    "tool-annotations-and-results",
    "nested-nutrition-and-encoded-slugs",
    "stdio-missing-token",
    "cli-offline-commands",
    "strict-inputs-of-every-other-tool",
];

/// The package version the freeze recorded at 41d6f98: `--version` printed it and `initialize`
/// named it in `serverInfo.version`. The replay expects this binary's own version there instead.
const RECORDED_VERSION: &str = "0.3.1";

fn frozen(set: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(set)
        .join(format!("{name}.json"));
    unpack(serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
}

/// What one scenario covers: CLI runs, tool results, and tool results with structured content.
#[derive(Default, Debug)]
struct Coverage {
    cli: usize,
    tools: usize,
    structured: usize,
}

/// Replay `name`'s steps, failing at the first difference.
fn replay(set: &str, name: &str, coverage: &mut Coverage) {
    let scenario = frozen(set, name);
    assert_eq!(scenario["id"], format!("kronan/{set}/{name}"));
    let home = Home::new(name);
    let script = Script::default();
    let upstream = script.upstream();
    let origin = upstream.origin.clone();
    let extra: Vec<(String, String)> = scenario["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
        .collect();
    let mut served = 0;

    for (index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
        let at = format!("{name} step {index}");

        if let Some(args) = step["cli"].as_array() {
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
            script.expect(&step["exchanges"], &origin);
            let run = cli(&home, &origin, &extra, &args, step["stdin"].as_str());
            script.finish(&format!("{at} {args:?}"));
            served += step["exchanges"].as_array().unwrap().len();
            let stdout = if asks_version(&args) {
                own_version_line(&step["stdout"])
            } else {
                step["stdout"].clone()
            };
            assert_eq!(
                json!({"code": run.code, "stdout": fresh(&run.stdout), "stderr": run.stderr}),
                json!({"code": step["code"], "stdout": stdout, "stderr": step["stderr"]}),
                "{at} {args:?}"
            );
        } else if step["serve"] == true {
            let (mut mcp, initialized) = Mcp::start(&home, &origin, &extra);

            if let Some(expected) = step.get("initialize") {
                assert_eq!(
                    initialized.answer,
                    answer(&own_server_info(expected)),
                    "{at} initialize"
                );
                let listed = mcp.request("tools/list", json!({}));
                // Schema key order differs between rmcp and zod; parity.ts never compared it. This
                // comparison with the frozen TypeScript list is the surface check apart from
                // src/surface.json: a change to that file fails here.
                assert_eq!(listed.answer, answer(&step["list"]), "{at} tools/list");
            }
            for (number, result) in step["results"].as_array().unwrap().iter().enumerate() {
                let call = &result["call"];
                let at = format!("{at} call {number} {call}");
                script.expect(&result["exchanges"], &origin);
                let got = mcp.call(call[0].as_str().unwrap(), call[1].clone());
                script.finish(&at);
                served += result["exchanges"].as_array().unwrap().len();
                let notifications: Vec<Value> = result
                    .get("notifications")
                    .map(|frozen| frozen.as_array().unwrap().clone())
                    .unwrap_or_default();
                assert_eq!(got.notifications, notifications, "{at}: notifications");
                let frozen = answer(&result["answer"]);
                same(&got.answer, &frozen, &at);

                if let Some(content) = frozen["result"].get("content") {
                    assert!(content.is_array());
                    coverage.tools += 1;
                }
                if frozen["result"].get("structuredContent").is_some() {
                    coverage.structured += 1;
                }
            }
            let (code, stderr) = mcp.close();
            assert_eq!(
                json!({"code": code, "stderr": home.local(&stderr)}),
                json!({"code": step["code"], "stderr": step["stderr"]}),
                "{at} exit"
            );
        } else if step.get("mode").is_some() {
            // The recorded exchanges already carry what the fake's mode answered.
        } else if let Some(token) = step.get("save") {
            home.save(token.as_str());
        } else if let Some(file) = step["remove"].as_str() {
            match fs::remove_file(home.path(file)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => panic!("{error}"),
                _ => {}
            }
        } else if let Some(link) = step["link"].as_str() {
            std::os::unix::fs::symlink(step["target"].as_str().unwrap(), home.path(link)).unwrap();
        } else if let Some(paths) = step.get("snapshot") {
            assert_eq!(&home.tree(), paths, "{at}: paths under the home");
        } else {
            let mode = step["permissions"].as_u64().map(|mode| mode as u32);
            home.write(
                step["file"].as_str().unwrap(),
                step["text"].as_str().unwrap(),
                mode,
            );
        }
    }
    assert_eq!(
        served as u64,
        scenario["requests"].as_u64().unwrap(),
        "{name}"
    );
    assert_eq!(upstream.count(), served, "{name}: requests");
    assert_eq!(home.stored(), scenario["store"], "{name}: store");
    assert_eq!(home.files(), scenario["files"], "{name}: files");

    if let Some(paths) = scenario.get("tree") {
        assert_eq!(&home.tree(), paths, "{name}: paths under the home");
    }
}

/// A version request, whose stdout is the only CLI output that names the version.
fn asks_version(args: &[&str]) -> bool {
    matches!(args, ["--version"] | ["-v"])
}

/// The recorded `--version` stdout, which must be exactly the recorded version and a newline, as
/// this binary's version in that format.
fn own_version_line(recorded: &Value) -> Value {
    assert_eq!(
        recorded,
        &json!(format!("{RECORDED_VERSION}\n")),
        "recorded --version stdout"
    );
    json!(format!("{}\n", env!("CARGO_PKG_VERSION")))
}

/// The recorded `initialize` answer with this binary's version in `result.serverInfo.version`,
/// which must hold exactly the recorded version. Nothing else changes.
fn own_server_info(recorded: &Value) -> Value {
    let mut answer = recorded.clone();
    let version = answer
        .pointer_mut("/result/serverInfo/version")
        .expect("recorded serverInfo.version");
    assert_eq!(
        version,
        &json!(RECORDED_VERSION),
        "recorded serverInfo.version"
    );
    *version = json!(env!("CARGO_PKG_VERSION"));
    answer
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

/// A frozen JSON-RPC answer without its envelope: `{result}` or `{error}`.
fn answer(frozen: &Value) -> Value {
    let mut answer = frozen.as_object().unwrap().clone();
    assert_eq!(answer.remove("jsonrpc"), Some(json!("2.0")));
    assert!(answer.remove("id").is_some());
    Value::Object(answer)
}

/// Equal values, and equal as text: a tool answer's key order is part of the contract.
fn same(got: &Value, frozen: &Value, at: &str) {
    assert_eq!(got, frozen, "{at}");
    assert_eq!(got.to_string(), frozen.to_string(), "{at}: key order");
}

#[test]
fn every_frozen_parity_scenario_replays_through_the_binary() {
    // Every fixture is replayed, and none is missing.
    fixtures_are("parity", &SCENARIOS);
    let mut coverage = Coverage::default();

    for name in SCENARIOS {
        replay("parity", name, &mut coverage);
    }
    // parity.ts's own floor, so the replay is never vacuous.
    assert!(
        coverage.cli >= 95 && coverage.tools >= 460 && coverage.structured >= 80,
        "{coverage:?}"
    );
}

#[test]
fn every_frozen_contract_case_replays_through_the_binary() {
    fixtures_are("contract", &CONTRACTS);
    let mut coverage = Coverage::default();

    for name in CONTRACTS {
        replay("contract", name, &mut coverage);
    }
    // What the freeze recorded, so a fixture emptied by mistake fails here.
    assert!(
        coverage.cli >= 17 && coverage.tools >= 174 && coverage.structured >= 93,
        "{coverage:?}"
    );
}

/// Only the recorded version becomes the binary's: a recorded value in another format or of another
/// version, a missing field, any other `serverInfo` difference, or a wrong binary version still fails.
#[test]
fn only_the_recorded_version_becomes_the_binary_version() {
    let own = env!("CARGO_PKG_VERSION");
    assert!(asks_version(&["--version"]) && asks_version(&["-v"]));
    assert!(!asks_version(&["--help"]) && !asks_version(&["--version", "serve"]));

    let line = own_version_line(&json!(format!("{RECORDED_VERSION}\n")));
    assert_eq!(line, json!(format!("{own}\n")));
    assert_ne!(line, json!("9.9.9\n"));

    for recorded in [
        json!(RECORDED_VERSION),
        json!(format!("v{RECORDED_VERSION}\n")),
        json!("9.9.9\n"),
        Value::Null,
    ] {
        assert!(
            catch_unwind(|| own_version_line(&recorded)).is_err(),
            "{recorded}"
        );
    }

    let initialize = |name: &str, version: Value| json!({"jsonrpc": "2.0", "id": 0, "result": {"serverInfo": {"name": name, "version": version}}});
    let answer = own_server_info(&initialize("kronan-mcp", json!(RECORDED_VERSION)));
    assert_eq!(answer, initialize("kronan-mcp", json!(own)));
    assert_ne!(answer, initialize("kronan-mcp", json!("9.9.9")));
    assert_ne!(
        own_server_info(&initialize("other-mcp", json!(RECORDED_VERSION))),
        initialize("kronan-mcp", json!(own))
    );

    for recorded in [
        initialize("kronan-mcp", json!(format!("{RECORDED_VERSION}-rc.1"))),
        initialize("kronan-mcp", json!("9.9.9")),
        json!({"jsonrpc": "2.0", "id": 0, "result": {"serverInfo": {"name": "kronan-mcp"}}}),
    ] {
        assert!(
            catch_unwind(|| own_server_info(&recorded)).is_err(),
            "{recorded}"
        );
    }
}
