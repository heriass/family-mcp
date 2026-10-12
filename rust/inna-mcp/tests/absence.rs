//! The absence submission, executed: the drop-in cases of tests/ts/integration.test.ts that
//! prepare and submit whole-day absences, against the binary's `serve`, with the provider those
//! cases used (support/provider.rs). Each submission is sent at most once: a preview is checked
//! again (context, permission, expiry, UTC day) before the one POST, a lost response or a kill
//! mid-send leaves the operation uncertain across restarts and logout, and an uncertain operation
//! blocks every new preview. Needs no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use support::case::{Case, Gate, refused};
use support::provider::{Entry, SIBLING, request, sibling, student};
use support::{NOW, Reply, Request, ended};

fn with(base: Value, extra: Value) -> Value {
    let mut base = base;
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn path_of(request: &Request) -> String {
    let line = request.line();
    line.split_once(' ').unwrap().1.to_owned()
}

/// The provider answers as the TypeScript one did: every exchange of two frozen contract cases
/// that ran against it, replayed into a fresh port, gets the recorded status, headers and body.
#[test]
fn the_fake_inna_answers_as_the_typescript_provider_did() {
    for name in ["read-tools-round-trip", "sibling-tool-round-trips"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contract")
            .join(format!("{name}.json"));
        let case: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let provider = support::provider::Provider::default();
        let mut compared = 0;

        for step in case["steps"].as_array().unwrap() {
            for exchange in step["exchanges"].as_array().into_iter().flatten() {
                let recorded = &exchange["request"];
                let request = Request {
                    method: recorded["method"].as_str().unwrap().to_owned(),
                    url: recorded["url"].as_str().unwrap().to_owned(),
                    headers: recorded["headers"].as_object().unwrap().clone(),
                    body: recorded["body"].as_str().unwrap().to_owned(),
                };
                let reply = provider.answer(&request);
                let frozen = Reply::frozen(&exchange["response"]);
                let at = format!("{name}: {}", request.url);
                assert_eq!(reply.status, frozen.status, "{at}");
                assert_eq!(reply.headers, frozen.headers, "{at}");
                assert_eq!(
                    String::from_utf8(reply.body).unwrap(),
                    String::from_utf8(frozen.body).unwrap(),
                    "{at}"
                );
                compared += 1;
            }
        }
        provider.finish(name);
        assert!(compared > 40, "{name}: {compared} exchanges");
    }
}

/// integration.test.ts:534
#[test]
fn both_absence_tools_round_trip_including_confirmed_inclusive_leave_dates() {
    let case = Case::new("round-trip", true);
    let mut client = case.client();
    let tools = client.mcp.request("tools/list", json!({})).value();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 16);

    let preview = client
        .prepare(with(
            request(),
            json!({"kind": "leave", "dateFrom": "2040-02-01", "dateTo": "2040-02-03"}),
        ))
        .unwrap();
    let id = preview["operationId"].as_str().unwrap().to_owned();
    assert_eq!(case.provider.state().posts, 0);

    refused(
        client.call(
            "inna_submit_absence",
            json!({"operationId": id, "confirm": false}),
        ),
        "",
    );
    assert_eq!(case.provider.state().posts, 0);

    let submitted = client.submit(&id).unwrap();
    assert_eq!(submitted["state"], "submitted");
    assert_eq!(submitted["timeZone"], "UTC");
    client.submit(&id).unwrap();
    assert_eq!(case.provider.state().posts, 1);

    let state = case.provider.state();
    let post = state
        .calls
        .iter()
        .find(|call| call.method == "POST")
        .unwrap();
    let body: Value = serde_json::from_str(post.body.as_deref().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "firstDay": "01.02.2040",
            "lastDay": "03.02.2040",
            "leaveStatus": 0,
            "leaveType": 3,
            "allDay": 1,
            "comment": "Synthetic reason",
        })
    );
}

/// integration.test.ts:754: the clock moves past the preview's expiry, or to the next UTC day,
/// while the submission checks run (when Inna answers the leave history); nothing is posted.
#[test]
fn utc_midnight_or_preview_expiry_during_submission_checks_prevents_a_post() {
    for (delay, expected) in [
        (10 * 60_000, "preview expired"),
        (12 * 60 * 60_000, "UTC day changed"),
    ] {
        let case = Case::new("midnight", true);
        let preview = case.client().prepare(request()).unwrap();
        let id = preview["operationId"].as_str().unwrap().to_owned();
        let (provider, clock) = (case.provider.clone(), case.home.clock());
        let mut concurrent = case.client_with(move |request| {
            let reply = provider.answer(request);

            if path_of(request) == "/api/RegisterAbsence/GetLeaves" {
                fs::write(&clock, (NOW + delay).to_string()).unwrap();
            }
            reply
        });

        refused(concurrent.submit(&id), expected);
        assert_eq!(case.provider.state().posts, 0);
        case.home.set_clock(NOW);
        assert_eq!(case.client().status()["operation"]["state"], "prepared");
    }
}

/// integration.test.ts:1018. Without `--allow-absence-writes` the binary neither lists nor
/// accepts the tool, which is the drop-in's refusal.
#[test]
fn whole_day_writes_require_preview_confirmation_permissions_and_the_same_student() {
    let case = Case::new("whole-day", true);
    refused(
        case.read_only_client().prepare(request()),
        "inna_prepare_absence",
    );

    let mut client = case.client();
    let preview = client.prepare(request()).unwrap();
    assert_eq!(case.provider.state().posts, 0);
    assert_eq!(preview["studentName"], "Synthetic student");
    let id = preview["operationId"].as_str().unwrap().to_owned();
    let submitted = client.submit(&id).unwrap();
    assert_eq!(submitted["state"], "submitted");
    assert_eq!(submitted["upstreamId"], 123);
    client.submit(&id).unwrap();
    assert_eq!(case.provider.state().posts, 1);

    {
        let state = case.provider.state();
        let post = state
            .calls
            .iter()
            .find(|call| call.method == "POST")
            .unwrap();
        let body: Value = serde_json::from_str(post.body.as_deref().unwrap()).unwrap();
        let picked: Value = ["firstDay", "lastDay", "leaveType", "allDay", "comment"]
            .iter()
            .map(|key| ((*key).to_owned(), body[*key].clone()))
            .collect::<serde_json::Map<_, _>>()
            .into();
        assert_eq!(
            picked,
            json!({
                "firstDay": "02.01.2040",
                "lastDay": "02.01.2040",
                "leaveType": 1,
                "allDay": 1,
                "comment": "Synthetic reason",
            })
        );
    }

    let leave = client
        .prepare(with(
            request(),
            json!({"kind": "leave", "dateFrom": "2040-02-01", "dateTo": "2040-02-03"}),
        ))
        .unwrap();
    case.provider.state().user["studentId"] = json!("99");
    refused(
        client.submit(leave["operationId"].as_str().unwrap()),
        "changed account",
    );
    assert_eq!(case.provider.state().posts, 1);
}

/// integration.test.ts:1063
#[test]
fn a_lost_write_response_survives_restart_and_logout_without_another_post() {
    let case = Case::new("lost-write", true);
    let mut client = case.client();
    let preview = client.prepare(request()).unwrap();
    let id = preview["operationId"].as_str().unwrap().to_owned();
    case.provider.state().fail_post = true;
    refused(client.submit(&id), "uncertain");
    drop(client);

    let mut restarted = case.client();
    assert_eq!(restarted.status()["operation"]["state"], "unknown");
    refused(restarted.submit(&id), "will not be replayed");
    refused(restarted.prepare(request()), "earlier absence");
    assert_eq!(case.provider.state().posts, 1);
    drop(restarted);

    assert_eq!(case.cli(&["auth", "logout"]).code, Some(0));
    assert!(case.journal().unwrap().contains("unknown"));
}

/// Condition 3's kill mid-send: the server is killed while Inna holds its one write, so the
/// operation was marked submitting before the send; a restart reports it uncertain, never sends
/// it again and blocks new previews.
#[test]
fn a_kill_mid_send_leaves_the_operation_uncertain_without_another_post() {
    let case = Case::new("kill-mid-send", true);
    let preview = case.client().prepare(request()).unwrap();
    let id = preview["operationId"].as_str().unwrap().to_owned();
    let gate = Gate::default();
    let (provider, held) = (case.provider.clone(), gate.clone());
    let mut sending = case.client_with(move |request| {
        if path_of(request) == "/api/RegisterAbsence/AddNewLeave" {
            held.hold();
        }
        provider.answer(request)
    });
    sending.mcp.begin(
        "tools/call",
        json!({"name": "inna_submit_absence", "arguments": {"operationId": id, "confirm": true}}),
    );
    gate.reached();
    assert!(case.journal().unwrap().contains(r#""state":"submitting""#));
    sending.mcp.signal("KILL");
    let status = sending.mcp.exit();
    assert_eq!(ended(status)["signal"], "SIGKILL");
    gate.release();
    drop(sending);

    let mut restarted = case.client();
    let operation = restarted.status()["operation"].clone();
    assert!(
        matches!(operation["state"].as_str(), Some("submitting" | "unknown")),
        "{operation}"
    );
    refused(restarted.submit(&id), "will not be replayed");
    refused(restarted.prepare(request()), "earlier absence");
    assert_eq!(case.provider.state().posts, 1);
}

/// A host cancel of the submit while Inna holds its one write, in both orders (lead 82367; the
/// replay's absence-cancel-post takes only the cancel first). Taken before the reply, the cancel
/// leaves the operation uncertain (`unknown`) with no answer. A reply taken first leaves it
/// `submitted` with Inna's id, whatever the cancel does after. Either way the record survives a
/// restart, is never sent again, and Inna got exactly one POST.
#[test]
fn a_cancelled_submit_is_unknown_or_submitted_by_order_and_never_sent_again() {
    for reply_first in [false, true] {
        let case = Case::new(&format!("cancel-submit-{reply_first}"), true);
        let preview = case.client().prepare(request()).unwrap();
        let id = preview["operationId"].as_str().unwrap().to_owned();
        let gate = Gate::default();
        let (provider, held) = (case.provider.clone(), gate.clone());
        let mut sending = case.client_with(move |request| {
            if path_of(request) == "/api/RegisterAbsence/AddNewLeave" {
                held.hold();
            }
            provider.answer(request)
        });
        let call = sending.mcp.begin(
            "tools/call",
            json!({"name": "inna_submit_absence", "arguments": {"operationId": id, "confirm": true}}),
        );
        gate.reached();
        assert!(case.journal().unwrap().contains(r#""state":"submitting""#));
        let cancel = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": call, "reason": "AbortError"}});
        let mut answers = Vec::new();

        if reply_first {
            gate.release();
            // The reply is taken once the record says so; only then does the cancel come.
            let deadline = std::time::Instant::now() + support::PATIENCE;

            while !case.journal().unwrap().contains(r#""state":"submitted""#) {
                assert!(std::time::Instant::now() < deadline, "{:?}", case.journal());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            sending.mcp.send(&cancel);
        } else {
            sending.mcp.send(&cancel);
            // The server reads its input in order: a ping answered means the cancel was taken.
            let ping = sending.mcp.begin("ping", json!({}));
            assert_eq!(sending.mcp.answer("ping").value()["id"], json!(ping));
            gate.release();
        }
        let ping = sending.mcp.begin("ping", json!({}));

        loop {
            let answer = sending.mcp.answer("ping").value();

            if answer["id"] == json!(ping) {
                break;
            }
            answers.push(answer);
        }
        let (code, _) = sending.mcp.close();
        assert_eq!(code, Some(0));
        let journal: Value = serde_json::from_str(&case.journal().unwrap()).unwrap();

        if reply_first {
            assert_eq!(journal["state"], "submitted", "{journal}");
            assert_eq!(journal["upstreamId"], 123, "{journal}");
        } else {
            assert_eq!(journal["state"], "unknown", "{journal}");
            assert!(answers.is_empty(), "{answers:?}");
        }

        let mut restarted = case.client();
        let state = if reply_first { "submitted" } else { "unknown" };
        assert_eq!(
            restarted.status()["operation"]["state"],
            state,
            "{reply_first}"
        );
        if reply_first {
            // A submitted operation answers with its record; nothing is sent.
            let again = restarted.submit(&id).unwrap();
            assert_eq!(again["state"], "submitted", "{again}");
            assert_eq!(again["upstreamId"], 123, "{again}");
        } else {
            refused(restarted.submit(&id), "will not be replayed");
        }
        assert_eq!(case.provider.state().posts, 1, "{reply_first}");
    }
}

/// integration.test.ts:1077
#[test]
fn a_student_switch_during_preflight_prevents_the_post() {
    let case = Case::new("switch-preflight", true);
    let preview = case.client().prepare(request()).unwrap();
    let provider = case.provider.clone();
    let mut concurrent = case.client_with(move |request| {
        let reply = provider.answer(request);

        if path_of(request) == "/api/RegisterAbsence/GetLeaves" {
            provider.state().user["studentId"] = json!("99");
        }
        reply
    });
    refused(
        concurrent.submit(preview["operationId"].as_str().unwrap()),
        "changed during absence checks",
    );
    assert_eq!(case.provider.state().posts, 0);
}

/// integration.test.ts:1100
#[test]
fn expired_previews_and_revoked_permissions_never_reach_the_write_endpoint() {
    let case = Case::new("expired", true);
    let mut client = case.client();
    let preview = client.prepare(request()).unwrap();
    let id = preview["operationId"].as_str().unwrap().to_owned();
    case.home.set_clock(NOW + 10 * 60_000);
    refused(case.client().submit(&id), "preview expired");
    case.home.set_clock(NOW);
    case.provider.state().user["registerAbsenceGuardian"] = json!("0");
    refused(client.submit(&id), "does not permit");
    assert_eq!(case.provider.state().posts, 0);
}

/// integration.test.ts:1529
#[test]
fn a_sibling_preview_is_submitted_once_after_switching_back() {
    let case = Case::new("sibling-preview", true);
    let mut client = case.client();
    let preview = client
        .prepare(with(request(), json!({"studentKey": SIBLING})))
        .unwrap();
    assert_eq!(preview["studentName"], "Synthetic sibling");
    assert_eq!(preview["schoolName"], "Synthetic second school");
    assert_eq!(preview["studentKey"], SIBLING);
    assert_eq!(preview["account"]["studentId"], "6");
    assert_eq!(preview["request"], request());

    let overview = client.call("inna_get_overview", json!({})).unwrap();
    assert_eq!(overview["context"]["studentId"], "2");
    let status = client.status();
    assert_eq!(status["context"]["studentId"], "2");
    assert_eq!(status["operation"]["account"]["studentId"], "6");
    assert_eq!(case.provider.state().selected, "1");

    let from = case.provider.state().calls.len();
    let submitted = client
        .submit(preview["operationId"].as_str().unwrap())
        .unwrap();
    assert_eq!(submitted["state"], "submitted");
    assert_eq!(submitted["studentKey"], SIBLING);
    let paths = case.provider.paths(from);
    assert_eq!(
        paths[..4],
        [
            "/api/UserData/GetLoggedInUser",
            "/auth/system",
            "/Components/Students/Students.html",
            "/api/UserData/GetLoggedInUser",
        ]
    );
    assert_eq!(paths.last().unwrap(), "/api/RegisterAbsence/AddNewLeave");
    assert_eq!(case.provider.state().selected, SIBLING);
    client.call("inna_get_overview", json!({})).unwrap();
    client
        .submit(preview["operationId"].as_str().unwrap())
        .unwrap();
    assert_eq!(case.provider.state().posts, 1);
}

/// integration.test.ts:1561: Inna moves back to the default student while the checks run, with
/// its context (the binding changes) or only its selection (the access entry changes).
#[test]
fn a_switch_away_from_the_sibling_during_preflight_prevents_the_post() {
    for move_context in [true, false] {
        let case = Case::new("switch-away", true);
        let preview = case
            .client()
            .prepare(with(request(), json!({"studentKey": SIBLING})))
            .unwrap();
        let provider = case.provider.clone();
        let mut concurrent = case.client_with(move |request| {
            let reply = provider.answer(request);

            if path_of(request) == "/api/RegisterAbsence/GetLeaves" {
                let mut state = provider.state();
                state.selected = "1".into();

                if move_context {
                    state.user = student();
                }
            }
            reply
        });
        refused(
            concurrent.submit(preview["operationId"].as_str().unwrap()),
            "changed during absence checks",
        );
        assert_eq!(case.provider.state().posts, 0);
        assert_eq!(case.client().status()["operation"]["state"], "prepared");
    }
}

/// integration.test.ts:1590: the sibling's access entry stops agreeing on its school (another
/// school, or none) while a read or the submission checks run.
#[test]
fn a_changed_or_missing_school_discards_reads_and_prevents_the_post() {
    let changed = Entry {
        system: "1",
        status: "2",
        skoli_id: Some("99"),
        skoli_heiti: "Synthetic other",
    };
    let missing = Entry {
        system: "1",
        status: "2",
        skoli_id: None,
        skoli_heiti: "Synthetic second school",
    };

    for entry in [changed, missing] {
        for endpoint in [
            "/api/Timetable/GetTimetable",
            "/api/RegisterAbsence/GetLeaves",
        ] {
            let case = Case::new("school", true);
            let preview = case
                .client()
                .prepare(with(request(), json!({"studentKey": SIBLING})))
                .unwrap();
            let (provider, entry) = (case.provider.clone(), entry.clone());
            let mut client = case.client_with(move |request| {
                let reply = provider.answer(request);

                if path_of(request) == endpoint {
                    provider
                        .state()
                        .entries
                        .insert(SIBLING.into(), entry.clone());
                }
                reply
            });

            match endpoint {
                "/api/Timetable/GetTimetable" => {
                    let text = refused(
                        client.call(
                            "inna_get_timetable",
                            json!({"dateFrom": "2040-01-02", "dateTo": "2040-01-03", "studentKey": SIBLING}),
                        ),
                        "during the read",
                    );
                    assert!(text.contains("discarded"), "{text}");
                }
                _ => {
                    refused(
                        client.submit(preview["operationId"].as_str().unwrap()),
                        "changed during absence checks",
                    );
                }
            }
            assert_eq!(
                case.provider.state().user["studentId"],
                sibling()["studentId"]
            );
            assert_eq!(case.provider.state().selected, SIBLING);
            assert_eq!(case.provider.state().posts, 0);
            assert_eq!(case.client().status()["operation"]["state"], "prepared");
        }
    }
}

/// integration.test.ts:1629
#[test]
fn one_uncertain_operation_blocks_new_previews_for_every_student() {
    let case = Case::new("uncertain", true);
    let mut client = case.client();
    let preview = client
        .prepare(with(request(), json!({"studentKey": SIBLING})))
        .unwrap();
    case.provider.state().fail_post = true;
    refused(
        client.submit(preview["operationId"].as_str().unwrap()),
        "uncertain",
    );
    refused(client.prepare(request()), "earlier absence");
    refused(
        client.prepare(with(request(), json!({"studentKey": SIBLING}))),
        "earlier absence",
    );
    assert_eq!(case.provider.state().posts, 1);
}
