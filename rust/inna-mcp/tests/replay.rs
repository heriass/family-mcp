//! The frozen TypeScript outcomes of every tests/ts/parity.ts scenario and of the drop-in cases the
//! ledger names `inna/contract/*`, replayed through this binary step by step (fixtures/parity and
//! fixtures/contract, PROVENANCE.md in each). Each case runs in one fresh home against a scripted
//! fake Inna, which serves the recorded responses in order and requires each request to equal the
//! recorded one (method, URL with its query, the eight headers fake-inna.ts compared, body), so
//! every step checks the full request list, its count and its order, not only the output. The one
//! difference is parity.ts's own: right after a GET answered 401 the binary renews, then retries
//! after a 200 (renewal.rs executes that recovery). Those hops are required, in that order, and
//! compared whole like a recorded request: the renewal is the 401 request's headers with an empty
//! body sent to the renewal URL, and the retry is the 401 request again, as inna-parity.ts
//! forwarded them. The fake answers them as the TypeScript fake's state would have; a missing,
//! extra or different hop fails, and each case checks the count its recorded 401s require
//! (the_fake_rejects_a_recovery_unlike_the_recorded_request checks the fake). Each CLI run
//! compares its exit code, stdout and stderr; each tool call its raw JSON-RPC answer, key order
//! included; each step the text
//! of every absence record; each case ends by comparing the decrypted stored session and
//! every path left with its mode. A `swap` step first plants the home exactly as the TypeScript
//! side had written it, so the binary continues from TypeScript-written records and previews. A
//! cancelled call holds the fake's reply to the awaited request until the cancel, end of input or
//! signal has been delivered (no sleeps), and the strict script of the steps after it fails on any
//! request sent late. A lost response, a kill mid-send, renewal timers and lock takeover are not
//! replayed: absence.rs, renewal.rs, keep_alive.rs and session.rs execute them. No output may hold
//! a cookie value or what fake-inna.ts marks as never returned. Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use support::{
    Answer, Home, Mcp, Names, RENEWAL, Request, Script, Upstream, cli, ended, no_leak, unpack,
};

/// The drop-in cases the ledger names `inna/contract/*`, one file each, and the case that freezes an
/// invalid input for every tool no other case gives one.
const CONTRACTS: [&str; 15] = [
    "read-tools-round-trip",
    "message-continuation",
    "illness-history-blocks-requests",
    "dated-feeds",
    "default-user-id",
    "students-listed-without-switching",
    "single-default-context",
    "unknown-student-keys",
    "sibling-tool-round-trips",
    "keep-alive-opt-out-flag",
    "too-many-saved-students",
    "phone-login-refusals",
    "phone-login-student-context",
    "browser-options-only-with-google-login",
    "strict-inputs-of-every-other-tool",
];

/// tests/ts/parity.ts's scenarios, one file each, under their ledger names.
const SCENARIOS: [&str; 38] = [
    "surface",
    "surface-writes",
    "surface-no-keep-alive",
    "write-tools-hidden",
    "cli",
    "reads",
    "reads-odd",
    "reads-signed-out",
    "reads-invalid",
    "siblings",
    "siblings-refused",
    "failures",
    "session-commands",
    "session-legacy-unreadable",
    "session-relative-path",
    "import-refusals",
    "import-accounts",
    "login-input",
    "google-refusals",
    "absence-writes",
    "absence-history",
    "absence-uncertain",
    "absence-interop",
    "absence-interop-unknown",
    "absence-refinements",
    "absence-429-post",
    "absence-429-prepare",
    "absence-429-submit-checks",
    "absence-cancel-submit",
    "absence-cancel-prepare",
    "absence-cancel-post",
    "absence-close-submit",
    "absence-sigint-submit",
    "absence-sigterm-submit",
    "login-store-unusable",
    "login-marker-unusable",
    "store-file-open",
    "store-folder-open",
];

/// The binary's wording of the keep-alive opt-out, which parity.ts mapped to the TypeScript one
/// before comparing (the binary's also says that the mandatory renewal still runs).
const KEEP_ALIVE_TEXT: [&str; 2] = [
    "Disable optional 10-minute touches; mandatory renewal still runs before 30 minutes",
    "Do not touch the saved session every 10 minutes while serving",
];

fn frozen(set: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(set)
        .join(format!("{name}.json"));
    unpack(serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
}

/// What the replay covered: CLI runs, tool results, those with structured content, recorded
/// requests, and the recovery hops the binary added.
#[derive(Default, Debug, PartialEq)]
struct Coverage {
    cli: usize,
    tools: usize,
    structured: usize,
    requests: usize,
    hops: usize,
}

/// One output as the freeze named it.
fn named(names: &Names, text: &str) -> String {
    names
        .fresh(text)
        .replace(KEEP_ALIVE_TEXT[0], KEEP_ALIVE_TEXT[1])
}

/// A JSON-RPC answer line as the freeze named it, without its envelope: `{result}` or `{error}`.
fn envelope(names: &Names, line: &str) -> Value {
    let message: Value = serde_json::from_str(&named(names, line)).unwrap();
    let mut answer = message.as_object().unwrap().clone();
    assert_eq!(answer.remove("jsonrpc"), Some(json!("2.0")));
    assert!(answer.remove("id").is_some());
    Value::Object(answer)
}

/// `ordered`: also compare key order, part of a tool answer's contract. parity.ts read every answer
/// through the SDK client, which rebuilds `initialize` and `tools/list` in its schemas' key order,
/// so there only the values were ever compared (and rmcp's schema key order is not zod's).
fn same_answer(names: &Names, got: &str, frozen: &Value, at: &str, ordered: bool) {
    let frozen = frozen.as_str().unwrap();
    let frozen_answer = {
        let message: Value = serde_json::from_str(frozen).unwrap();
        let mut answer = message.as_object().unwrap().clone();
        answer.remove("jsonrpc");
        answer.remove("id");
        Value::Object(answer)
    };
    let got = envelope(names, got);
    assert_eq!(got, frozen_answer, "{at}");

    if ordered {
        assert_eq!(
            got.to_string(),
            frozen_answer.to_string(),
            "{at}: key order"
        );
    }
}

/// `<home>` in a frozen argument or value is this home.
fn placed_home(home: &Home, text: &str) -> String {
    text.replace("<home>", &home.0.display().to_string())
}

fn replay_case(set: &str, name: &str, coverage: &mut Coverage, tz: Option<&str>) {
    let case = frozen(set, name);
    assert_eq!(case["id"], format!("inna/{set}/{name}"));
    let home = Home::new(name);
    let names = Names::new(&home, case["rotation"] == true);
    let script = Script::new(names.clone());
    let upstream = script.upstream();
    let origin = upstream.origin.clone();
    let mut extra: Vec<(String, String)> = case
        .get("env")
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .map(|(key, value)| (key.clone(), placed_home(&home, value.as_str().unwrap())))
                .collect()
        })
        .unwrap_or_default();

    if let Some(tz) = tz {
        extra.push(("TZ".into(), tz.into()));
    }

    // The scenarios were frozen on macOS, where Google sign-in needs no desktop session. On Linux
    // a display name (nothing connects to it) keeps the same path; the missing-display refusal is
    // browser_login.rs::a_missing_display_on_linux_fails_before_anything_is_launched.
    if cfg!(target_os = "linux") {
        extra.push(("DISPLAY".into(), ":0".into()));
    }
    let mut serving: Option<Mcp> = None;

    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        let at = format!("{name} step {index}");

        if step.get("upstream").is_some() {
            // The recorded exchanges carry what the fake answered.
        } else if let Some(time) = step.get("clock") {
            home.set_clock(time.as_i64().unwrap());
        } else if let Some(directory) = step["directory"].as_str() {
            home.chmod(directory, step["permissions"].as_u64().unwrap() as u32);
        } else if let Some(args) = step["cli"].as_array() {
            coverage.cli += 1;
            let args: Vec<String> = args
                .iter()
                .map(|arg| placed_home(&home, arg.as_str().unwrap()))
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            script.expect(&step["exchanges"]);
            let run = cli(&home, &origin, &extra, &args, step["stdin"].as_str());
            script.finish(&format!("{at} {args:?}"));
            no_leak(&format!("{}{}", run.stdout, run.stderr), &at);

            if let Some(absent) = step["absent"].as_str() {
                assert!(
                    !format!("{}{}", run.stdout, run.stderr).contains(absent),
                    "{at}: printed {absent:?}"
                );
            }
            assert_eq!(
                json!({"code": run.code, "stdout": named(&names, &run.stdout), "stderr": named(&names, &run.stderr)}),
                json!({"code": step["code"], "stdout": step["stdout"], "stderr": step["stderr"]}),
                "{at} {args:?}"
            );
        } else if step.get("serve").is_some() {
            if let Some(plant) = step.get("plant") {
                planted(&home, &names, plant, step);
            }
            let args: Vec<&str> = step
                .get("args")
                .and_then(Value::as_array)
                .map(|args| args.iter().map(|arg| arg.as_str().unwrap()).collect())
                .unwrap_or_default();
            let (mut mcp, initialized) = Mcp::start(&home, &origin, &extra, &args);

            surface(&names, &mut mcp, &initialized, step, &at);
            for (number, result) in step["results"].as_array().unwrap().iter().enumerate() {
                let at = format!("{at} call {number} {}", result["call"][0]);
                one_call(&names, &script, &mut mcp, result, &at, coverage);
            }
            closed(&names, mcp, step, &at);
        } else if step.get("cancel").is_some() {
            cancel(&home, &names, &script, &upstream, &extra, step, &at);
        } else if step.get("open").is_some() {
            // A contract case's long-lived `serve`, as the drop-in client's.
            assert!(serving.is_none(), "{at}: serve is already open");
            let args = strings(step.get("args"));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (mut mcp, initialized) = Mcp::start(&home, &origin, &extra, &args);
            surface(&names, &mut mcp, &initialized, step, &at);
            serving = Some(mcp);
        } else if step.get("call").is_some() {
            let mcp = serving
                .as_mut()
                .unwrap_or_else(|| panic!("{at}: no serve is open"));
            one_call(
                &names,
                &script,
                mcp,
                step,
                &format!("{at} {}", step["call"][0]),
                coverage,
            );
        } else if step.get("close").is_some() {
            let mcp = serving
                .take()
                .unwrap_or_else(|| panic!("{at}: no serve is open"));
            closed(&names, mcp, step, &at);
        } else if let Some(path) = step["remove"].as_str() {
            fs::remove_file(home.path(path)).unwrap();
        } else if let Some(plant) = step.get("plant") {
            planted(&home, &names, plant, step);
        } else if let Some(expected) = step.get("stored") {
            assert_eq!(&home.stored(&names), expected, "{at}: store");
            assert_eq!(
                home.generation().map(Value::from),
                step.get("generation").cloned(),
                "{at}: marker generation"
            );
        } else if let Some(file) = step["file"].as_str() {
            let mode = step
                .get("permissions")
                .and_then(Value::as_u64)
                .unwrap_or(0o600);
            home.write(file, step["text"].as_str().unwrap(), mode as u32);
        } else {
            panic!("{at}: unknown step {step}");
        }
        if let Some(journal) = step.get("journal") {
            assert_eq!(&home.journals(&names), journal, "{at}: absence records");
        }
    }
    assert!(serving.is_none(), "{name}: serve left open");
    let (served, hops) = script.served();
    assert_eq!(
        served as u64,
        case["requests"].as_u64().unwrap(),
        "{name}: requests"
    );
    assert_eq!(hops, script.required_hops(), "{name}: recovery hops");
    assert_eq!(
        upstream.count(),
        served + hops,
        "{name}: requests and recovery hops"
    );
    coverage.requests += served;
    coverage.hops += hops;
    assert_eq!(home.files(&names), case["files"], "{name}: files");
    let stored = home.stored(&names);
    let frozen_store = match case.get("record") {
        Some(record) => json!({ "record": record }),
        None => match case.get("error") {
            Some(code) => json!({ "error": code }),
            None => json!({ "store": case["store"] }),
        },
    };
    assert_eq!(stored, frozen_store, "{name}: store");
}

/// The `initialize` and `tools/list` answers of a surface step, when it recorded them.
fn surface(names: &Names, mcp: &mut Mcp, initialized: &Answer, step: &Value, at: &str) {
    if let Some(expected) = step.get("initialize") {
        same_answer(
            names,
            &initialized.line,
            expected,
            &format!("{at} initialize"),
            false,
        );
        let listed = mcp.request("tools/list", json!({}));
        same_answer(
            names,
            &listed.line,
            &step["list"],
            &format!("{at} tools/list"),
            false,
        );
    }
}

/// One recorded tool call on the open `serve`: its requests, notifications and answer.
fn one_call(
    names: &Names,
    script: &Script,
    mcp: &mut Mcp,
    result: &Value,
    at: &str,
    coverage: &mut Coverage,
) {
    let call = &result["call"];
    script.expect(&result["exchanges"]);
    let got = mcp.call(call[0].as_str().unwrap(), names.placed(&call[1]));
    script.finish(at);
    no_leak(&format!("{}{}", got.line, got.notifications.join("")), at);
    names.learn(&got.value());
    let notifications: Vec<Value> = got
        .notifications
        .iter()
        .map(|line| serde_json::from_str(&named(names, line)).unwrap())
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
    same_answer(names, &got.line, &result["answer"], at, true);
    let answer: Value = serde_json::from_str(result["answer"].as_str().unwrap()).unwrap();

    if answer["result"].get("content").is_some() {
        coverage.tools += 1;
    }
    if answer["result"].get("structuredContent").is_some() {
        coverage.structured += 1;
    }
}

/// A `serve` closed by the end of its input: its exit code and stderr.
fn closed(names: &Names, mcp: Mcp, step: &Value, at: &str) {
    let (code, stderr) = mcp.close();
    no_leak(&stderr, at);
    assert_eq!(
        json!({"code": code, "stderr": named(names, &stderr)}),
        json!({"code": step["code"], "stderr": step["stderr"]}),
        "{at} exit"
    );
}

/// The home as the TypeScript side had written it, and the operations it knew.
fn planted(home: &Home, names: &Names, plant: &Value, step: &Value) {
    home.plant(plant);
    names.set_operations(strings(step.get("operations")));
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// parity.ts's cancelled call: the reply to `when` is held until the host's cancel, the end of
/// the server's input or the signal has been delivered, then released.
fn cancel(
    home: &Home,
    names: &Names,
    script: &Script,
    upstream: &Upstream,
    extra: &[(String, String)],
    step: &Value,
    at: &str,
) {
    let args: Vec<&str> = step
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().map(|arg| arg.as_str().unwrap()).collect())
        .unwrap_or_default();
    let call = &step["cancel"];
    let before = upstream.count();
    script.expect(&step["exchanges"]);
    script.hold(step["when"].as_str().unwrap());
    let (mut mcp, _) = Mcp::start(home, &upstream.origin, extra, &args);
    let id = mcp.begin(
        "tools/call",
        json!({"name": call[0], "arguments": names.placed(&call[1])}),
    );
    script.reached(at);
    let outcome = match step["by"].as_str().unwrap() {
        "host" => {
            mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id, "reason": "AbortError"}}));
            // TypeScript acts on the cancel at once. The server reads its input in order, so a ping
            // answered (the held call cannot answer yet) shows the cancel was taken before the fake
            // replies; without this, Linux saw the reply win.
            let taken = mcp.begin("ping", json!({}));
            let next = mcp.answer("ping").value();
            assert_eq!(next["id"], json!(taken), "{at}: {next}");
            script.release();
            // A ping answered before any answer to the call shows the call was dropped.
            let ping = mcp.begin("ping", json!({}));
            let next = mcp.answer("ping").value();
            let call = match next["id"] == json!(ping) {
                true => "no answer".to_owned(),
                false => format!("An answer to {}, not {ping}.", next["id"]),
            };
            let (code, stderr) = mcp.close();
            no_leak(&stderr, at);
            json!({"call": call, "code": code, "stderr": named(names, &stderr)})
        }
        "stdin" => {
            mcp.end_input();
            script.release();
            ended(mcp.finish().0)
        }
        signal => {
            mcp.signal(signal.trim_start_matches("SIG"));
            script.release();
            // Input stays open, so only the signal can have ended the server.
            ended(mcp.exit())
        }
    };
    script.finish(at);
    assert_eq!(outcome, step["outcome"], "{at}: outcome");
    // What reached the fake, not what the fixture lists.
    let writes = upstream.seen()[before..]
        .iter()
        .filter(|request| request.url.ends_with("/api/RegisterAbsence/AddNewLeave"))
        .count();
    assert_eq!(
        writes as u64,
        step["writes"].as_u64().unwrap(),
        "{at}: writes"
    );
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
        replay_case(set, name, &mut coverage, None);
    }
    coverage
}

#[test]
fn every_frozen_parity_scenario_replays() {
    let coverage = replay("parity", &SCENARIOS);
    // What the freeze recorded, so a fixture emptied by mistake fails here.
    assert_eq!(
        coverage,
        Coverage {
            cli: 132,
            tools: 230,
            structured: 119,
            requests: 558,
            hops: 6,
        }
    );
}

#[test]
fn every_frozen_contract_case_replays() {
    let coverage = replay("contract", &CONTRACTS);
    // What the freeze recorded, so a fixture emptied by mistake fails here.
    assert_eq!(
        coverage,
        Coverage {
            cli: 55,
            tools: 88,
            structured: 51,
            requests: 406,
            hops: 0,
        }
    );
}

/// dates.test.ts:61: unzoned Inna timestamps stay UTC whatever the host's timezone. The scenarios
/// whose feeds hold unzoned times (lessons, homework, messages, absence and leave history), each
/// replayed with the binary's TZ set to a zone west and two east of UTC, give the frozen UTC
/// answers.
#[test]
fn reads_are_identical_under_another_host_timezone() {
    for tz in ["Pacific/Honolulu", "Europe/Berlin", "Pacific/Auckland"] {
        let mut coverage = Coverage::default();

        for (set, name) in [
            ("parity", "reads"),
            ("parity", "siblings"),
            ("parity", "absence-history"),
            ("contract", "dated-feeds"),
            ("contract", "message-continuation"),
        ] {
            replay_case(set, name, &mut coverage, Some(tz));
        }
        assert!(coverage.structured > 20, "{tz}: {coverage:?}");
    }
}

/// The fake's own check (review of PR #27, finding 1): after a recorded 401 the renewal and the
/// retry are compared whole, in order, and a missing or extra one fails, like a recorded request.
#[test]
fn the_fake_rejects_a_recovery_unlike_the_recorded_request() {
    let get = |url: &str| Request {
        method: "GET".into(),
        url: url.into(),
        headers: [("cookie".to_owned(), json!("SESSION=synthetic-school"))]
            .into_iter()
            .collect(),
        body: String::new(),
    };
    let first = get("https://nam.inna.is/api/Announcements/GetStudentAnnouncements");
    let renewal = get(RENEWAL);
    let response = |status: u16| json!({"status": status, "headers": {}, "body": "{}"});
    // Plays `requests` against one recorded 401 with its recovery; whether `finish` passed.
    let play = |label: &str, requests: &[Request]| {
        let home = Home::new(&format!("fake-recovery-{label}"));
        let script = Script::new(Names::new(&home, false));
        script.expect(&json!([{
            "request": first.json(),
            "response": response(401),
            "recovery": {"renewal": response(200), "retry": response(200)},
        }]));

        for request in requests {
            script.answer(request);
        }
        let counts = (script.served(), script.required_hops());
        let finished =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| script.finish(label)));
        (finished.is_ok(), counts.0, counts.1)
    };
    assert_eq!(
        play("recorded", &[first.clone(), renewal.clone(), first.clone()]),
        (true, (1, 2), 2)
    );

    let mut headers = renewal.clone();
    headers
        .headers
        .insert("authorization".into(), json!("Bearer synthetic-unexpected"));
    let mut cookie = renewal.clone();
    cookie
        .headers
        .insert("cookie".into(), json!("synthetic-wrong-cookie"));
    let mut body = first.clone();
    body.body = "unexpected synthetic GET body".into();
    let rejected: [(&str, Vec<Request>); 6] = [
        (
            "renewal-header",
            vec![first.clone(), headers, first.clone()],
        ),
        ("renewal-cookie", vec![first.clone(), cookie, first.clone()]),
        ("retry-body", vec![first.clone(), renewal.clone(), body]),
        ("no-retry", vec![first.clone(), renewal.clone()]),
        ("no-recovery", vec![first.clone()]),
        (
            "extra-renewal",
            vec![
                first.clone(),
                renewal.clone(),
                first.clone(),
                renewal.clone(),
            ],
        ),
    ];

    for (label, requests) in rejected {
        assert!(!play(label, &requests).0, "{label} passed");
    }
}
