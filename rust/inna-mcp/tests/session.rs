//! The saved session, executed: the drop-in cases of tests/ts/integration.test.ts on the saved
//! record and its lock, the student switch and its verification, rate-limit pauses, and the
//! store's refusals, against the binary's `serve` and `auth` commands with the provider those
//! cases used (support/provider.rs). The drop-ins' `defaultUserId()` and `checkStore()` ran the
//! TypeScript store, never the binary, so their subassertions are not ported. Needs no Bun; needs
//! the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use support::browser::{Login, START_MESSAGE, USER_PATH, collect};
use support::case::{Case, Gate, refused};
use support::provider::{Provider, SIBLING, expect_clean, request, sibling, student};
use support::{NOW, PATIENCE, Reply, Request};

const USER: &str = "/api/UserData/GetLoggedInUser";
const SWITCH: &str = "/auth/system";
const STUDENTS: &str = "/Components/Students/Students.html";
const RANGE: (&str, &str) = ("2040-01-02", "2040-01-03");
const UNCERTAIN: &str = "The last write to the Inna session store did not complete";
const OVERLAP: &str =
    "INNA_SESSION_FILE overlaps the encrypted Inna session store or its key. Choose another path.";

fn with(base: Value, extra: Value) -> Value {
    let mut base = base;
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn range(extra: Value) -> Value {
    with(json!({"dateFrom": RANGE.0, "dateTo": RANGE.1}), extra)
}

fn path_of(request: &Request) -> String {
    request.line().split_once(' ').unwrap().1.to_owned()
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A store failure's fixed text starts the error: a tool error, or the command's stderr line.
#[track_caller]
fn starts(outcome: Result<Value, String>, text: &str) {
    let error = refused(outcome, "");
    assert!(error.starts_with(text), "expected {text:?}, got {error:?}");
}

fn cli_error(run: support::Run) -> Result<Value, String> {
    match run.code {
        Some(0) => Ok(json!(run.stdout)),
        _ => Err(run.stderr.trim_end().to_owned()),
    }
}

/// integration.test.ts:647: eight concurrent timetable calls on one server reach Inna one at a
/// time, each with its own fresh answer.
#[test]
fn repeated_and_concurrent_reads_stay_serialized_fresh_utc_and_free_of_writes() {
    let case = Case::new("concurrent", false);
    let (active, maximum, reads) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (provider, counted, highest, served) = (
        case.provider.clone(),
        active.clone(),
        maximum.clone(),
        reads.clone(),
    );
    let mut client = case.client_with(move |request| {
        let now = counted.fetch_add(1, Ordering::SeqCst) + 1;
        highest.fetch_max(now, Ordering::SeqCst);
        // The case's fetch waits 2 ms before Inna answers, so overlapping requests would meet.
        std::thread::sleep(Duration::from_millis(2));
        let reply = provider.answer(request);
        let reply = match path_of(request).as_str() {
            "/api/Timetable/GetTimetable" => {
                let read = served.fetch_add(1, Ordering::SeqCst) + 1;
                Reply::json(
                    200,
                    &json!([{
                        "start": "2040-01-02T10:00:00",
                        "end": "2040-01-02T11:00:00",
                        "titleShort": format!("Synthetic lesson {read}"),
                        "allDay": false,
                    }]),
                )
            }
            _ => reply,
        };
        counted.fetch_sub(1, Ordering::SeqCst);
        reply
    });
    let ids: Vec<u64> = (0..8)
        .map(|_| {
            client.mcp.begin(
                "tools/call",
                json!({"name": "inna_get_timetable", "arguments": range(json!({}))}),
            )
        })
        .collect();
    let mut results = Vec::new();

    for _ in &ids {
        let answer = client.mcp.answer("a timetable call").value();
        assert!(ids.contains(&answer["id"].as_u64().unwrap()));
        results.push(answer["result"]["structuredContent"].clone());
    }
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    let titles: HashSet<String> = results
        .iter()
        .map(|result| {
            result["entries"][0]["titleShort"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(titles.len(), 8);

    for result in &results {
        assert_eq!(
            result["entries"][0]["dates"]["start"]["iso"],
            "2040-01-02T10:00:00.000Z"
        );
        assert_eq!(result["timeZone"], "UTC");
    }
    let state = case.provider.state();
    assert_eq!(state.posts, 0);
    assert!(state.calls.iter().all(|call| call.method == "GET"));
    assert_eq!(mode(&case.store()), 0o600);
}

/// integration.test.ts:694
#[test]
fn a_student_switch_during_a_read_discards_the_fetched_records() {
    let case = Case::new("switch-during-read", false);
    let provider = case.provider.clone();
    let mut client = case.client_with(move |request| {
        let reply = provider.answer(request);

        if path_of(request) == "/api/Timetable/GetTimetable" {
            provider.state().user["studentId"] = json!("99");
        }
        reply
    });
    let text = refused(
        client.call("inna_get_timetable", range(json!({}))),
        "during the read",
    );
    assert!(text.contains("discarded"), "{text}");
    assert_eq!(case.provider.state().posts, 0);
}

/// integration.test.ts:716: numeric and HTTP-date Retry-After values, and unusable ones as the
/// default minute.
#[test]
fn rate_limit_pauses_survive_restart_and_expire_at_the_saved_utc_deadline() {
    for (retry, wait) in [
        ("1", 60_000),
        ("120", 120_000),
        ("Mon, 02 Jan 2040 12:02:00 GMT", 120_000),
        ("7200", 7_200_000),
        ("bad", 60_000),
        ("-1", 60_000),
    ] {
        let case = Case::new("pause", false);
        let provider = case.provider.clone();
        let mut limited = case.client_with(move |request| {
            provider.answer(request);
            Reply {
                status: 429,
                headers: vec![("retry-after".into(), retry.into())],
                body: Vec::new(),
            }
        });
        let messages = json!({"rowFrom": 1, "rowTo": 21});
        refused(
            limited.call("inna_get_messages", messages.clone()),
            "rate limited",
        );
        assert_eq!(case.saved()["pauseUntil"], json!(NOW + wait), "{retry}");
        let mut restarted = case.client();
        let calls = case.provider.state().calls.len();
        refused(
            restarted.call("inna_get_messages", messages.clone()),
            "requested a pause",
        );
        assert_eq!(case.provider.state().calls.len(), calls);
        case.home.set_clock(NOW + wait);
        let read = restarted.call("inna_get_messages", messages).unwrap();
        assert_eq!(read["count"], 1, "{retry}");
    }
}

/// integration.test.ts:995
#[test]
fn private_import_principal_binding_redirect_refusal_and_shared_pause() {
    let case = Case::new("private-import", false);
    assert_eq!(mode(&case.store()), 0o600);
    let source = case.home.path("cookies.json");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(case.import(false).is_err());
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let before = case.home.record();
    case.provider.state().user["studentId"] = json!("99");
    let error = case.import(false).unwrap_err();
    assert!(error.contains("changes the account"), "{error}");
    assert_eq!(case.home.record(), before);
    let mut client = case.client();
    refused(
        client.call("inna_get_overview", json!({})),
        "changed account",
    );
    case.provider.state().user["studentId"] = json!("2");
    case.provider.state().redirect = true;
    refused(
        client.call("inna_get_overview", json!({})),
        "sign-in is required",
    );
    case.provider.state().redirect = false;
    case.provider.state().rate_limit = true;
    refused(client.call("inna_get_overview", json!({})), "rate limited");
    case.provider.state().rate_limit = false;
    let count = case.provider.state().calls.len();
    refused(
        case.client().call("inna_get_overview", json!({})),
        "requested a pause",
    );
    assert_eq!(case.provider.state().calls.len(), count);
}

/// integration.test.ts:1110
#[test]
fn rate_limiting_during_a_replacement_import_preserves_the_saved_pause() {
    let case = Case::new("import-pause", false);
    case.provider.state().rate_limit = true;
    let error = case.import(false).unwrap_err();
    assert!(error.contains("rate limited"), "{error}");
    case.provider.state().rate_limit = false;
    let count = case.provider.state().calls.len();
    refused(
        case.client().call("inna_get_overview", json!({})),
        "requested a pause",
    );
    assert_eq!(case.provider.state().calls.len(), count);
}

/// integration.test.ts:1313
#[test]
fn reading_the_sibling_then_the_default_switches_there_and_back_with_verification() {
    let case = Case::new("there-and-back", false);
    let mut client = case.client();
    let mut from = case.provider.state().calls.len();
    let timetable = client
        .call("inna_get_timetable", range(json!({"studentKey": SIBLING})))
        .unwrap();
    assert_eq!(timetable["context"]["studentId"], "6");
    assert_eq!(timetable["context"]["studentName"], "Synthetic sibling");
    assert_eq!(
        case.provider.paths(from),
        [
            USER,
            SWITCH,
            STUDENTS,
            USER,
            "/api/Timetable/GetTimetable",
            USER
        ]
    );
    {
        let state = case.provider.state();
        assert_eq!(
            state.calls[from + 1].url.query(),
            Some("i=1&system=1&status=2&user_id=5")
        );
        assert_eq!(state.calls[from + 2].url.scheme(), "https");
        let student_id = state.calls[from + 4]
            .url
            .query_pairs()
            .find(|(key, _)| key == "student_id")
            .map(|(_, value)| value.into_owned());
        assert_eq!(student_id.as_deref(), Some("6"));
    }

    from = case.provider.state().calls.len();
    let messages = client
        .call(
            "inna_get_messages",
            json!({"rowFrom": 1, "rowTo": 21, "studentKey": SIBLING}),
        )
        .unwrap();
    assert_eq!(messages["context"]["studentId"], "6");
    assert_eq!(
        case.provider.paths(from),
        [USER, "/api/Messages/GetReceivedMessages", USER]
    );

    from = case.provider.state().calls.len();
    let overview = client.call("inna_get_overview", json!({})).unwrap();
    assert_eq!(overview["context"]["studentId"], "2");
    assert_eq!(
        case.provider.paths(from)[..4],
        [USER, SWITCH, STUDENTS, USER]
    );
    {
        let state = case.provider.state();
        assert_eq!(
            state.calls[from + 1].url.query(),
            Some("i=0&system=1&status=1&user_id=1")
        );
        assert_eq!(state.switches, 2);
        assert!(state.calls.iter().all(|call| call.method == "GET"));
    }
    let saved = case.saved();
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["account"]["studentId"], "2");
    let mut keys: Vec<&String> = saved["students"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["1", SIBLING]);
    assert_eq!(
        saved["students"][SIBLING]["studentName"],
        "Synthetic sibling"
    );
    assert!(saved["jar"].as_str().unwrap().contains("synthetic-rotated"));
}

/// integration.test.ts:1361
#[test]
fn a_changed_access_order_still_switches_by_the_current_index() {
    let case = Case::new("access-order", false);
    let mut client = case.client();
    client
        .call("inna_get_timetable", range(json!({"studentKey": SIBLING})))
        .unwrap();
    case.provider.state().order = vec!["9".into(), SIBLING.into(), "1".into()];
    let from = case.provider.state().calls.len();
    let overview = client.call("inna_get_overview", json!({})).unwrap();
    assert_eq!(overview["context"]["studentId"], "2");
    assert_eq!(
        case.provider.state().calls[from + 1].url.query(),
        Some("i=2&system=1&status=1&user_id=1")
    );
    case.provider.state().order = vec![SIBLING.into(), "9".into(), "1".into()];
    let grades = client
        .call("inna_get_grades", json!({"studentKey": SIBLING}))
        .unwrap();
    assert_eq!(grades["context"]["studentId"], "6");
    let state = case.provider.state();
    let last = state
        .calls
        .iter()
        .rfind(|call| call.url.path() == SWITCH)
        .unwrap();
    assert_eq!(last.url.query(), Some("i=0&system=1&status=2&user_id=5"));
}

/// integration.test.ts:1376
#[test]
fn an_ignored_misdirected_or_rate_limited_switch_returns_no_data() {
    let ignored = Case::new("ignored-switch", false);
    ignored.provider.state().ignore_switch = true;
    refused(
        ignored
            .client()
            .call("inna_get_timetable", range(json!({"studentKey": SIBLING}))),
        "did not select the requested student",
    );
    assert!(
        !ignored
            .provider
            .paths(0)
            .contains(&"/api/Timetable/GetTimetable".to_owned())
    );
    assert!(ignored.saved()["students"].get(SIBLING).is_none());

    for location in [
        "https://r.inna.is/login",
        "http://example.invalid/Components/Students/Students.html",
        "https://nam.inna.is/Components/Other/Other.html",
        "https://nam.inna.is:8443/Components/Students/Students.html",
    ] {
        let case = Case::new("misdirected", false);
        case.provider.state().switch_location = location.into();
        refused(
            case.client()
                .call("inna_get_overview", json!({"studentKey": SIBLING})),
            "refused the student switch",
        );
        assert_eq!(case.provider.paths(0).last().unwrap(), SWITCH, "{location}");
    }

    let refusing = Case::new("refused-switch", false);
    refusing.provider.state().switch_unauthorized = true;
    let mut client = refusing.client();
    let text = refused(
        client.call("inna_get_overview", json!({"studentKey": SIBLING})),
        "",
    );
    assert_eq!(
        text,
        "Inna refused the student switch and asked for sign-in. The session may have ended; sign in again and report this."
    );
    assert_eq!(refusing.provider.paths(0).last().unwrap(), SWITCH);
    refusing.provider.state().unauthorized = true;
    starts(
        client.call("inna_get_overview", json!({})),
        "Inna sign-in is required.",
    );

    let limited = Case::new("limited-switch", false);
    limited.provider.state().switch_rate_limit = true;
    refused(
        limited
            .client()
            .call("inna_get_overview", json!({"studentKey": SIBLING})),
        "rate limited",
    );
    assert_eq!(limited.saved()["pauseUntil"], json!(NOW + 120_000));
    let count = limited.provider.state().calls.len();
    refused(
        limited.client().call("inna_get_overview", json!({})),
        "requested a pause",
    );
    assert_eq!(limited.provider.state().calls.len(), count);
}

fn terms_read(provider: &Provider, from: usize) -> bool {
    provider
        .paths(from)
        .contains(&"/api/StudentTerms/GetStudentTerms".to_owned())
}

/// integration.test.ts:1465
#[test]
fn a_learned_binding_that_later_differs_is_refused() {
    let case = Case::new("learned-binding", false);
    let mut client = case.client();
    client
        .call("inna_get_overview", json!({"studentKey": SIBLING}))
        .unwrap();
    client.call("inna_get_overview", json!({})).unwrap();
    case.provider
        .state()
        .contexts
        .insert(SIBLING.into(), with(sibling(), json!({"studentId": "77"})));
    let from = case.provider.state().calls.len();
    refused(
        client.call("inna_get_overview", json!({"studentKey": SIBLING})),
        "changed account",
    );
    assert!(!terms_read(&case.provider, from));
    assert_eq!(case.saved()["students"][SIBLING]["studentId"], "6");
    support::provider::expect_clean(&case.home.record().unwrap(), "the stored record");

    // The selected entry must agree with the returned context on user and school.
    for wrong in [
        with(sibling(), json!({"userId": 7})),
        with(sibling(), json!({"schoolId": "3"})),
    ] {
        let strict = Case::new("strict-binding", false);
        strict
            .provider
            .state()
            .contexts
            .insert(SIBLING.into(), wrong);
        let mut client = strict.client();
        refused(
            client.call("inna_get_overview", json!({"studentKey": SIBLING})),
            "did not select the requested student",
        );
        assert!(!terms_read(&strict.provider, 0));
        assert!(strict.saved()["students"].get(SIBLING).is_none());
        {
            let mut state = strict.provider.state();
            state.user = with(student(), json!({"schoolId": "8"}));
            state.selected = "1".into();
        }
        refused(
            client.call("inna_get_overview", json!({})),
            "changed account",
        );
        let error = strict.import(true).unwrap_err();
        assert!(error.contains("one selected student"), "{error}");
    }

    let numeric = Case::new("numeric-access", false);
    numeric.provider.state().numeric = true;
    let mut client = numeric.client();
    let overview = client
        .call("inna_get_overview", json!({"studentKey": SIBLING}))
        .unwrap();
    assert_eq!(overview["context"]["studentId"], "6");
    let listed = client.call("inna_list_students", json!({})).unwrap();
    let selected: Vec<&Value> = listed["students"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["selected"])
        .collect();
    assert_eq!(selected, [&json!(false), &json!(true)]);

    let duplicate = Case::new("duplicate-student", false);
    duplicate
        .provider
        .state()
        .contexts
        .insert(SIBLING.into(), with(sibling(), json!({"studentId": "2"})));
    refused(
        duplicate
            .client()
            .call("inna_get_overview", json!({"studentKey": SIBLING})),
        "already saved under another studentKey",
    );
    assert!(duplicate.saved()["students"].get(SIBLING).is_none());
}

/// integration.test.ts:1513
#[test]
fn import_onto_a_learned_sibling_is_refused_unless_deliberate() {
    let case = Case::new("import-onto-sibling", true);
    let mut client = case.client();
    client.prepare(request()).unwrap();
    client
        .call("inna_get_overview", json!({"studentKey": SIBLING}))
        .unwrap();
    let before = case.home.record();
    let error = case.import(false).unwrap_err();
    assert!(error.contains("Select the default student"), "{error}");
    assert_eq!(case.home.record(), before);
    assert_eq!(client.status()["operation"]["account"]["studentId"], "2");

    case.import(true).unwrap();
    let replaced = case.saved();
    assert_eq!(replaced["account"]["studentId"], "6");
    let keys: Vec<&String> = replaced["students"].as_object().unwrap().keys().collect();
    assert_eq!(keys, [SIBLING]);
    refused(
        client.call("inna_absence_status", json!({})),
        "belongs to another account",
    );
}

/// integration.test.ts:1971
#[test]
fn the_session_is_saved_only_encrypted_after_import_keep_alive_and_reads() {
    let case = Case::new("encrypted-only", true);
    assert!(case.plaintext_cookies().is_empty());
    let mut client = case.client();
    let status = client.call("inna_session_status", json!({})).unwrap();
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["storage"], "Saved in an encrypted file.");
    assert!(!case.session().exists());
    assert_eq!(mode(&case.store()), 0o600);
    assert_eq!(mode(&case.key()), 0o600);

    case.provider.state().rotation = "synthetic-kept".into();
    assert_eq!(client.keep_alive(), "kept");
    assert!(case.home.record().unwrap().contains("synthetic-kept"));
    client.prepare(request()).unwrap();
    client
        .call("inna_get_overview", json!({"studentKey": SIBLING}))
        .unwrap();
    assert!(
        case.plaintext_cookies().is_empty(),
        "{:?}",
        case.plaintext_cookies()
    );
    // The absence record stays a plaintext private file beside the legacy session path.
    let absence = fs::read_to_string(case.home.path("session.json.absence.json")).unwrap();
    let absence: Value = serde_json::from_str(&absence).unwrap();
    assert_eq!(absence["state"], "prepared");
}

/// integration.test.ts:2131: a rotated cookie past the record's bound cannot be written back,
/// so the record is removed, and every later operation refuses without a request or a write.
#[test]
fn a_jar_that_cannot_be_written_back_removes_the_record() {
    let case = Case::new("unwritable-jar", true);
    let mut client = case.client();
    client.prepare(request()).unwrap();
    let absence = fs::read(case.home.path("session.json.absence.json")).unwrap();
    case.provider.state().rotation = format!("synthetic-huge-{}", "x".repeat(262_144));
    starts(client.call("inna_get_overview", json!({})), UNCERTAIN);
    case.provider.state().rotation = "synthetic-rotated".into();
    assert!(!case.store().exists());
    case.home
        .write("session.json", "planted plaintext session", 0o600);
    let files = case.store_files();
    let calls = case.provider.state().calls.len();

    starts(client.call("inna_get_overview", json!({})), UNCERTAIN);
    starts(client.call("inna_session_status", json!({})), UNCERTAIN);
    starts(client.call("inna_absence_status", json!({})), UNCERTAIN);
    starts(cli_error(case.cli(&["auth", "migrate"])), UNCERTAIN);
    starts(cli_error(case.cli(&["auth", "logout"])), UNCERTAIN);
    starts(case.import(false).map(Value::from), UNCERTAIN);
    assert_eq!(client.keep_alive(), "failed");
    assert_eq!(case.provider.state().calls.len(), calls);
    assert_eq!(case.store_files(), files);
    assert_eq!(
        fs::read(case.home.path("session.json.absence.json")).unwrap(),
        absence
    );
}

/// integration.test.ts:2161: a directory in place of the marker makes the write-back fail before
/// any file changed; Inna returned the same cookie, so the record stays for the next read.
#[test]
fn a_failed_write_back_with_unchanged_cookies_keeps_the_record() {
    let case = Case::new("failed-write-back", true);
    case.client().call("inna_get_overview", json!({})).unwrap();
    let record = fs::read(case.store()).unwrap();
    let marker = case.home.path(".config/inna-mcp/session.enc.marker");
    let marker_bytes = fs::read(&marker).unwrap();
    let blocked = Arc::new(AtomicBool::new(false));
    let (provider, block, at) = (case.provider.clone(), blocked.clone(), marker.clone());
    let mut client = case.client_with(move |request| {
        if block.swap(false, Ordering::SeqCst) {
            fs::remove_file(&at).unwrap();
            fs::create_dir(&at).unwrap();
        }
        provider.answer(request)
    });
    blocked.store(true, Ordering::SeqCst);
    starts(
        client.call("inna_get_overview", json!({})),
        "Cannot access the private Inna files.",
    );
    assert_eq!(fs::read(case.store()).unwrap(), record);

    fs::remove_dir(&marker).unwrap();
    case.home.write(
        ".config/inna-mcp/session.enc.marker",
        std::str::from_utf8(&marker_bytes).unwrap(),
        0o600,
    );
    client.call("inna_get_overview", json!({})).unwrap();
}

/// integration.test.ts:2201: a lost key refuses everything except the sign-in that replaces
/// the store.
#[test]
fn only_a_login_or_import_replaces_a_lost_key() {
    let case = Case::new("lost-key", true);
    let mut client = case.client();
    client.prepare(request()).unwrap();
    drop(client);
    case.home
        .write("session.json", "planted plaintext session", 0o600);
    let files = case.store_files();
    let calls = case.provider.state().calls.len();
    fs::remove_file(case.key()).unwrap();
    let missing = "The Inna store key is missing. Run inna-mcp auth";
    let mut client = case.client();
    starts(client.call("inna_get_overview", json!({})), missing);
    starts(client.call("inna_session_status", json!({})), missing);
    starts(client.call("inna_absence_status", json!({})), missing);
    starts(cli_error(case.cli(&["auth", "migrate"])), missing);
    starts(cli_error(case.cli(&["auth", "logout"])), missing);
    assert_eq!(client.keep_alive(), "failed");
    assert_eq!(case.provider.state().calls.len(), calls);
    let lost = vec![
        files[0].clone(),
        files[1].clone(),
        None,
        files[3].clone(),
        files[4].clone(),
    ];
    assert_eq!(case.store_files(), lost);

    // A failed sign-in replaces nothing either.
    case.provider.state().unauthorized = true;
    let error = case.import(false).unwrap_err();
    assert!(error.contains("sign-in is required"), "{error}");
    case.provider.state().unauthorized = false;
    assert_eq!(case.store_files(), lost);

    let stdout = case.import(false).unwrap();
    assert!(
        stdout.starts_with("The old Inna session store could not be read"),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "Signed in. Saved in an encrypted file."),
        "{stdout}"
    );
    let status = case
        .client()
        .call("inna_session_status", json!({}))
        .unwrap();
    assert_eq!(status["authenticated"], true);
    // The sign-in that replaced the store also removes the leftover plaintext file.
    assert!(!case.session().exists());
    assert_eq!(case.store_files()[4], files[4]);
    assert!(case.plaintext_cookies().is_empty());
}

/// integration.test.ts:2302. Only the session path moves here: the binary's store and key follow
/// its own layout (see the ledger row).
#[test]
fn a_session_path_overlapping_the_store_its_key_or_namespaces_is_refused() {
    let case = Case::new("overlap", true);
    case.client().prepare(request()).unwrap();
    let files = case.store_files();
    let link = case.home.path("link");
    std::os::unix::fs::symlink(case.home.path(".config"), &link).unwrap();
    let store = case.store().display().to_string();
    let aliases = [
        case.store(),
        format!("{store}.marker").into(),
        format!("{store}.lock").into(),
        case.key(),
        std::path::Path::new(&format!("{store}.lock")).join("session.json"),
        link.join("inna-mcp/session.enc"),
        store.trim_end_matches(".enc").into(),
    ];

    for alias in &aliases {
        let mut client = case.client_at(alias);
        let at = alias.display().to_string();

        for outcome in [
            client.call("inna_get_overview", json!({})),
            client.call("inna_session_status", json!({})),
            client.call("inna_absence_status", json!({})),
            cli_error(case.cli_at(
                alias,
                &[
                    "auth",
                    "import",
                    &case.home.path("cookies.json").display().to_string(),
                ],
            )),
            cli_error(case.cli_at(alias, &["auth", "migrate"])),
            cli_error(case.cli_at(alias, &["auth", "logout"])),
        ] {
            assert_eq!(outcome, Err(OVERLAP.to_owned()), "{at}");
        }
        assert_eq!(client.keep_alive(), "failed", "{at}");
        assert_eq!(case.store_files(), files, "{at}");
    }
    let mut listed: Vec<String> = fs::read_dir(case.home.path(".config/inna-mcp"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    listed.sort();
    assert_eq!(listed, ["session.enc", "session.enc.marker"]);
    assert_eq!(case.client().status()["operation"]["state"], "prepared");
}

/// integration.test.ts:2444: three servers on one store reach Inna one at a time, each
/// operation writing its jar back once; a logout waits for a keep-alive tick in flight.
#[test]
fn clients_sharing_one_store_serialize_and_logout_waits_for_a_request_in_flight() {
    let case = Case::new("shared-store", false);
    let (active, maximum) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let hold = Arc::new(AtomicBool::new(false));
    let gate = Gate::default();
    let mut clients: Vec<_> = (0..3)
        .map(|_| {
            let (provider, counted, highest, holding, held) = (
                case.provider.clone(),
                active.clone(),
                maximum.clone(),
                hold.clone(),
                gate.clone(),
            );
            case.client_with(move |request| {
                let now = counted.fetch_add(1, Ordering::SeqCst) + 1;
                highest.fetch_max(now, Ordering::SeqCst);

                match holding.load(Ordering::SeqCst) {
                    true => held.hold(),
                    false => std::thread::sleep(Duration::from_millis(2)),
                }
                let reply = provider.answer(request);
                counted.fetch_sub(1, Ordering::SeqCst);
                reply
            })
        })
        .collect();
    let before = case.home.generation().unwrap();

    for client in &mut clients {
        client.mcp.begin(
            "tools/call",
            json!({"name": "inna_get_overview", "arguments": {}}),
        );
        client.mcp.signal("USR1");
        client.mcp.begin(
            "tools/call",
            json!({"name": "inna_list_students", "arguments": {}}),
        );
    }
    for client in &mut clients {
        for _ in 0..2 {
            let answer = client.mcp.answer("a shared-store call").value();
            assert!(answer["result"]["isError"] != true, "{answer}");
        }
        assert!(client.mcp.until_stderr("keep-alive: kept", 1, PATIENCE));
    }
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    // Each of the nine operations wrote its jar back once, in its own hold.
    assert_eq!(case.home.generation().unwrap(), before + 9);

    hold.store(true, Ordering::SeqCst);
    clients[0].mcp.signal("USR1");
    gate.reached();
    let mut logout = case
        .home
        .command(&case.upstream.origin, &case.env(), &["auth", "logout"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The logout waits for the session lock the held tick has (review of PR #27, finding 2): its
    // lock temporary appears, and it is still running, before the tick is released.
    let session = case.home.path("session.json");
    let deadline = std::time::Instant::now() + PATIENCE;

    while !support::waits_for_lock(&session, logout.id()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the logout never waited for the store's lock"
        );
        assert!(logout.try_wait().unwrap().is_none(), "the logout ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(logout.try_wait().unwrap().is_none(), "the logout ended");
    assert!(support::waits_for_lock(&session, logout.id()));
    gate.release();
    assert!(clients[0].mcp.until_stderr("keep-alive: kept", 2, PATIENCE));
    let status = support::wait(&mut logout, PATIENCE).expect("the logout finished");
    assert!(status.success());
    assert_eq!(case.home.record().as_deref(), Some("null"));
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
}

/// Not a case: the fake browser, run by `fake-chrome` (support/browser.rs).
#[test]
#[ignore = "run as the fake browser by support/browser.rs's fake-chrome"]
fn fake_browser() {
    support::browser::fake_browser();
}

/// integration.test.ts:1190
#[test]
fn google_sign_in_saves_a_version_2_session_and_refuses_a_changed_binding() {
    let case = Case::empty("google", false);
    // The fake browser and its profile's TMPDIR; the store, session and Inna are the case's.
    let login = Login::new("google-binding");
    let mut env = login.environment(&[]);
    env.extend(case.env());
    let run = |extra: &[&str]| {
        collect(login.spawn_at(
            &case.home,
            &login.browser,
            &env,
            20,
            Some(&case.upstream.origin),
            extra,
        ))
    };
    let signed_in = || {
        (
            Some(0),
            "Signed in. Saved in an encrypted file.\n".to_owned(),
        )
    };

    let (exit, stdout, stderr) = run(&[]);
    assert_eq!((exit, stdout), signed_in());
    assert_eq!(stderr, START_MESSAGE);
    assert_eq!(case.provider.paths(0), [USER_PATH]);
    let saved = case.saved();
    assert_eq!(saved["version"], 2);
    assert_eq!(
        saved["account"],
        json!({"userId": 1, "studentId": "2", "schoolId": "3"})
    );
    let students: Vec<&String> = saved["students"].as_object().unwrap().keys().collect();
    assert_eq!(students, ["1"]);
    let jar = saved["jar"].as_str().unwrap();
    assert!(jar.contains("synthetic-rotated"), "{jar}");
    assert!(jar.contains("synthetic-xsrf"), "{jar}");
    assert!(!jar.contains("decoy"), "{jar}");
    assert_eq!(mode(&case.store()), 0o600);

    let status = case
        .client()
        .call("inna_session_status", json!({}))
        .unwrap();
    assert_eq!(status["authenticated"], true, "{status}");
    assert_eq!(status["context"]["studentId"], "2", "{status}");

    let before = case.home.record();
    case.provider.state().user["studentId"] = json!("99");
    let (exit, stdout, stderr) = run(&[]);
    assert_eq!((exit, stdout.as_str()), (Some(1), ""));
    assert_eq!(
        stderr,
        format!(
            "{START_MESSAGE}This export changes the account, student, or school. Use --allow-account-change deliberately.\n"
        )
    );
    assert_eq!(case.home.record(), before);

    let (exit, stdout, _) = run(&["--allow-account-change"]);
    assert_eq!((exit, stdout), signed_in());
    assert_eq!(case.saved()["account"]["studentId"], "99");
    expect_clean(&case.home.record().unwrap(), "the saved session");
}
