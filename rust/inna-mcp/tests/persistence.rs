//! Where the session is saved, executed: the drop-in cases of tests/ts/integration.test.ts on the
//! plaintext session file, its migration into the encrypted store, and what decides between
//! them, against the binary's `serve` and `auth` commands. A store moved under another folder is
//! the drop-ins' `storeAt(folder)`: the binary's XDG folders there. The drop-ins'
//! `defaultUserId()` ran the TypeScript store, so it is not ported. Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use support::case::{Case, Client};
use support::provider::{SIBLING, request, sibling, student};
use support::{Home, Upstream};

const PLAINTEXT: &str = "Saved in a plaintext file. Run inna-mcp auth migrate.";
const ENCRYPTED: &str = "Saved in an encrypted file.";
const MIGRATED: &str =
    "Inna session moved to the encrypted store; the plaintext file was removed.\n";
const ALREADY: &str = "Already migrated.\n";
const ALREADY_REMOVED: &str = "Already migrated. Removed the leftover plaintext session file.\n";
const UNCERTAIN: &str = "The last write to the Inna session store";

fn overview(client: &mut Client, key: Option<&str>) -> Result<Value, String> {
    let arguments = match key {
        Some(key) => json!({"studentKey": key}),
        None => json!({}),
    };
    client.call("inna_get_overview", arguments)
}

fn student_id(client: &mut Client, key: Option<&str>) -> Value {
    overview(client, key).unwrap()["context"]["studentId"].clone()
}

/// The stdout of a command that succeeded, or its stderr line.
fn outcome(run: support::Run) -> Result<String, String> {
    match run.code {
        Some(0) => Ok(run.stdout),
        _ => Err(run.stderr.trim_end().to_owned()),
    }
}

#[track_caller]
fn starts<T: std::fmt::Debug>(outcome: Result<T, String>, text: &str) {
    match outcome {
        Err(error) if error.starts_with(text) => {}
        other => panic!("expected {text:?}, got {other:?}"),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The record and the key of a store under `root`.
fn store_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join(".config/inna-mcp/session.enc"),
        root.join(".config/inna-mcp/session.enc.marker"),
        root.join(".local/share/family-mcp/keys/inna-mcp.default.key"),
    )
}

fn absence(case: &Case) -> Option<Vec<u8>> {
    fs::read(case.home.path("session.json.absence.json")).ok()
}

/// integration.test.ts:1427
#[test]
fn a_version_1_session_file_migrates_and_learns_students_on_use() {
    let case = Case::new("version-1", false);
    let saved = case.saved();
    let mut version1 = serde_json::Map::new();
    version1.insert("version".into(), json!(1));
    version1.insert("jar".into(), saved["jar"].clone());
    version1.insert("account".into(), saved["account"].clone());
    version1.insert("pauseUntil".into(), saved["pauseUntil"].clone());
    let version1 = Value::Object(version1).to_string();
    // Before any store exists the plaintext file is authoritative and is written back in place.
    let root = case.home.path("unmigrated");
    let mut client = case.client_in(case.env_store(&root), None);
    case.home.write("session.json", &version1, 0o600);
    assert_eq!(student_id(&mut client, None), "2");
    let migrated: Value =
        serde_json::from_str(&fs::read_to_string(case.session()).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
    let keys: Vec<&String> = migrated["students"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["1"]);
    assert_eq!(student_id(&mut client, Some(SIBLING)), "6");
    assert_eq!(student_id(&mut client, None), "2");
    assert_eq!(mode(&case.session()), 0o600);
    let status = client.call("inna_session_status", json!({})).unwrap();
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["storage"], PLAINTEXT);
    let (record, marker, _) = store_paths(&root);
    assert!(!record.exists() && !marker.exists());

    // A version 1 file whose browser session moved to the sibling switches back to its default.
    {
        let mut state = case.provider.state();
        state.selected = SIBLING.into();
        state.user = sibling();
    }
    case.home.write("session.json", &version1, 0o600);
    let switches = case.provider.state().switches;
    assert_eq!(student_id(&mut client, None), "2");
    assert_eq!(case.provider.state().switches, switches + 1);

    // The same version 1 value in the store record is read as version 2 too.
    {
        let mut state = case.provider.state();
        state.selected = "1".into();
        state.user = student();
    }
    case.home.update_record(|_| version1.clone());
    assert_eq!(student_id(&mut case.client(), None), "2");
    assert_eq!(case.saved()["version"], 2);
}

/// integration.test.ts:1994
#[test]
fn auth_migrate_moves_a_plaintext_session_once_and_later_plaintext_is_never_read() {
    let case = Case::new("migrate-once", true);
    let preview = case.client().prepare(request()).unwrap();
    let absence_before = absence(&case);
    let legacy = case.home.record().unwrap();
    let planted = legacy.replace("synthetic-rotated", "synthetic-planted");
    let root = case.home.path("unmigrated");
    let env = case.env_store(&root);
    let cookies = Arc::new(Mutex::new(Vec::new()));
    let (provider, seen) = (case.provider.clone(), cookies.clone());
    let upstream = Upstream::new(move |request| {
        let cookie = request.headers.get("cookie").and_then(Value::as_str);
        seen.lock().unwrap().push(cookie.unwrap_or("").to_owned());
        provider.answer(request)
    });
    let mut client = case.client_in(env.clone(), Some(upstream));
    let migrate = |case: &Case| outcome(case.cli_in(&env, &["auth", "migrate"]));

    assert_eq!(
        client.call("inna_session_status", json!({})).unwrap(),
        json!({"authenticated": false})
    );
    starts(migrate(&case), "No Inna session.");
    assert!(!store_paths(&root).1.exists());

    case.home.write("session.json", &legacy, 0o600);
    let status = client.call("inna_session_status", json!({})).unwrap();
    assert_eq!(status["storage"], PLAINTEXT);
    assert_eq!(
        client.status()["operation"]["operationId"],
        preview["operationId"]
    );
    assert_eq!(migrate(&case).unwrap(), MIGRATED);
    assert!(!case.session().exists());
    let moved: Value = serde_json::from_str(&case.home.record_at(&root).unwrap()).unwrap();
    let original: Value = serde_json::from_str(&legacy).unwrap();
    assert_eq!(moved["account"], original["account"]);
    let status = client.call("inna_session_status", json!({})).unwrap();
    assert_eq!(status["storage"], ENCRYPTED);
    assert_eq!(migrate(&case).unwrap(), ALREADY);
    assert!(case.plaintext_cookies().is_empty());

    // A plaintext file planted after the marker exists is removed unread.
    case.home.write("session.json", &planted, 0o600);
    assert_eq!(client.keep_alive(), "kept");
    assert_eq!(student_id(&mut client, None), "2");
    assert_eq!(
        client.status()["operation"]["operationId"],
        preview["operationId"]
    );
    assert_eq!(fs::read_to_string(case.session()).unwrap(), planted);
    assert_eq!(migrate(&case).unwrap(), ALREADY_REMOVED);
    assert!(!case.session().exists());

    // Logout keeps the store deciding, so a planted file stays unread and login is still needed.
    assert_eq!(case.cli_in(&env, &["auth", "logout"]).code, Some(0));
    case.home.write("session.json", &planted, 0o600);
    let calls = case.provider.state().calls.len();
    assert_eq!(
        client.call("inna_session_status", json!({})).unwrap(),
        json!({"authenticated": false})
    );
    assert_eq!(client.keep_alive(), "skipped");
    starts(overview(&mut client, None), "No Inna session.");
    assert_eq!(case.provider.state().calls.len(), calls);
    assert_eq!(migrate(&case).unwrap(), ALREADY_REMOVED);
    assert_eq!(case.home.record_at(&root).as_deref(), Some("null"));
    let cookies = cookies.lock().unwrap();
    assert!(!cookies.join("\n").contains("synthetic-planted"));
    assert!(!cookies.is_empty());
    assert_eq!(absence(&case), absence_before);
}

/// integration.test.ts:2054: the CLI alone, with no upstream to reach.
#[test]
fn the_cli_migrates_reports_where_it_is_saved_and_repeats_safely() {
    let case = Case::new("cli-migrate", false);
    let directory = case.home.path("cli");
    let path = directory.join("session.json");
    case.home
        .write("cli/session.json", &case.home.record().unwrap(), 0o600);
    case.home.write(
        "cli/session.json.absence.json",
        "synthetic absence record",
        0o600,
    );
    let mut env = vec![("INNA_SESSION_FILE".to_owned(), path.display().to_string())];
    env.extend(Home::store_env(&directory.join("store")));
    let run = |args: &[&str]| {
        let mut all = vec!["auth"];
        all.extend_from_slice(args);
        let run = support::cli(&case.home, "http://127.0.0.1:9", &env, &all, None);
        (run.code, run.stdout, run.stderr)
    };

    assert_eq!(run(&["migrate"]), (Some(0), MIGRATED.into(), String::new()));
    assert!(!path.exists());
    let leaked: Vec<PathBuf> = case
        .plaintext_cookies()
        .into_iter()
        .filter(|found| found.starts_with(&directory))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    assert_eq!(
        case.home.record_at(&directory.join("store")),
        case.home.record()
    );
    assert_eq!(run(&["migrate"]).1, ALREADY);
    case.home.write("cli/session.json", "not a session", 0o600);
    assert_eq!(run(&["migrate"]).1, ALREADY_REMOVED);
    let logout = run(&["logout"]);
    assert_eq!((logout.0, logout.2), (Some(0), String::new()));
    assert_eq!(
        run(&["status"]),
        (Some(0), "No saved Inna session.\n".into(), String::new())
    );
    assert_eq!(
        fs::read_to_string(directory.join("session.json.absence.json")).unwrap(),
        "synthetic absence record"
    );
}

/// integration.test.ts:2101
#[test]
fn a_login_or_import_before_migration_moves_the_plaintext_session() {
    let case = Case::new("import-migrates", true);
    let mut client = case.client();
    client.prepare(request()).unwrap();
    overview(&mut client, Some(SIBLING)).unwrap();
    overview(&mut client, None).unwrap();
    drop(client);
    let absence_before = absence(&case);
    let legacy = case.home.record().unwrap();
    let root = case.home.path("unmigrated");
    let env = case.env_store(&root);
    let source = case.home.path("cookies.json").display().to_string();
    let import = || outcome(case.cli_in(&env, &["auth", "import", &source]));
    case.home.write("session.json", &legacy, 0o600);

    // The plaintext session still decides what an import may replace.
    case.provider.state().user["studentId"] = json!("99");
    let error = import().unwrap_err();
    assert!(error.contains("changes the account"), "{error}");
    assert_eq!(fs::read_to_string(case.session()).unwrap(), legacy);
    let (_, marker, key) = store_paths(&root);
    assert!(!marker.exists() && !key.exists());

    case.provider.state().user["studentId"] = json!("2");
    let stdout = import().unwrap();
    assert_eq!(stdout, format!("Signed in. {ENCRYPTED}\n"));
    assert!(!case.session().exists());
    // The students learned before the import are kept, as in a replacement import.
    let saved: Value = serde_json::from_str(&case.home.record_at(&root).unwrap()).unwrap();
    let mut keys: Vec<&String> = saved["students"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["1", SIBLING]);
    assert!(case.plaintext_cookies().is_empty());
    assert_eq!(absence(&case), absence_before);
}

/// integration.test.ts:2242: a sign-in that replaced a lost key's store and died before writing
/// the new record leaves a marker without a record.
#[test]
fn a_marker_without_a_record_never_reads_plaintext_and_migrate_resumes() {
    let case = Case::new("marker-only", false);
    let legacy = case.home.record().unwrap();
    fs::remove_file(case.key()).unwrap();
    case.home.reset_store();
    case.home.write("session.json", &legacy, 0o600);
    let calls = case.provider.state().calls.len();
    let mut client = case.client();
    assert_eq!(
        client.call("inna_session_status", json!({})).unwrap(),
        json!({"authenticated": false})
    );
    assert_eq!(client.keep_alive(), "skipped");
    starts(overview(&mut client, None), "No Inna session.");
    assert_eq!(case.provider.state().calls.len(), calls);
    assert_eq!(fs::read_to_string(case.session()).unwrap(), legacy);
    assert_eq!(outcome(case.cli(&["auth", "migrate"])).unwrap(), MIGRATED);
    assert!(!case.session().exists());
    assert_eq!(student_id(&mut client, None), "2");
}

/// integration.test.ts:2260
#[test]
fn any_read_sweeps_a_stale_absence_temporary_and_keeps_a_fresh_one() {
    let case = Case::new("sweep", false);
    let stale = "session.json.absence.json.5a0c1f0e-0000-4000-8000-000000000001.tmp";
    let fresh = "session.json.absence.json.5a0c1f0e-0000-4000-8000-000000000002.tmp";
    case.home.write(stale, "synthetic", 0o600);
    case.home.write(fresh, "synthetic", 0o600);
    let epoch = FileTimes::new()
        .set_accessed(UNIX_EPOCH)
        .set_modified(UNIX_EPOCH);
    File::options()
        .write(true)
        .open(case.home.path(stale))
        .unwrap()
        .set_times(epoch)
        .unwrap();
    case.client()
        .call("inna_session_status", json!({}))
        .unwrap();
    assert!(!case.home.path(stale).exists());
    assert!(case.home.path(fresh).exists());
}

/// integration.test.ts:2272
#[test]
fn a_sign_in_onto_a_deciding_store_removes_a_leftover_plaintext_file() {
    let case = Case::new("leftover", false);
    case.home
        .write("session.json", "planted plaintext session", 0o600);
    case.import(false).unwrap();
    assert!(!case.session().exists());

    // Also on a marker without a record, as a reset that died before its write leaves it.
    fs::remove_file(case.key()).unwrap();
    case.home.reset_store();
    case.home
        .write("session.json", "planted plaintext session", 0o600);
    case.import(false).unwrap();
    assert!(!case.session().exists());
    assert_eq!(student_id(&mut case.client(), None), "2");
}

/// integration.test.ts:2287
#[test]
fn a_record_without_its_marker_is_uncertain_not_a_reason_to_read_plaintext() {
    let case = Case::new("record-only", false);
    let legacy = case.home.record().unwrap();
    fs::remove_file(case.home.path(".config/inna-mcp/session.enc.marker")).unwrap();
    case.home.write("session.json", &legacy, 0o600);
    let files = case.store_files();
    let calls = case.provider.state().calls.len();
    let mut client = case.client();

    starts(overview(&mut client, None), UNCERTAIN);
    starts(client.call("inna_session_status", json!({})), UNCERTAIN);
    starts(client.call("inna_absence_status", json!({})), UNCERTAIN);
    starts(outcome(case.cli(&["auth", "migrate"])), UNCERTAIN);
    starts(outcome(case.cli(&["auth", "logout"])), UNCERTAIN);
    starts(case.import(false), UNCERTAIN);
    assert_eq!(client.keep_alive(), "failed");
    assert_eq!(case.provider.state().calls.len(), calls);
    assert_eq!(case.store_files(), files);
}

/// integration.test.ts:2367's `worst`: the largest record the saved-session schema accepts, as
/// `JSON.stringify` wrote it (each control character is 6 bytes; names are cut to 256 units).
fn worst_record() -> String {
    let control = "\\u0001";
    let digits = |index: usize| format!("{index:9>32}");
    let students: Vec<String> = (0..64)
        .map(|index| {
            format!(
                r#""{key}":{{"userId":9007199254740991,"studentId":"{key}","schoolId":"{key}","studentName":"{name}"}}"#,
                key = digits(index),
                name = control.repeat(256),
            )
        })
        .collect();
    format!(
        r#"{{"version":2,"jar":"{jar}","account":{{"userId":9007199254740991,"studentId":"{zero}","schoolId":"{zero}"}},"students":{{{students}}},"pauseUntil":-1.7976931348623157e+308}}"#,
        jar = control.repeat(262_144),
        zero = digits(0),
        students = students.join(","),
    )
}

/// integration.test.ts:2367. The store's bound is exactly the worst record: the binary reads
/// it (and fails only at its jar of control characters, which is no cookie jar) and refuses one
/// byte more as an unusable store. That the schema refuses a longer jar is
/// src/session.rs::saved_names_are_cut_but_held_names_are_not; through the binary that refusal
/// and the jar's own failure have one generic text.
#[test]
fn the_record_limit_is_exactly_the_largest_session_the_schema_accepts() {
    let worst = worst_record();
    let parsed: Value = serde_json::from_str(&worst).unwrap();
    assert_eq!(parsed["students"].as_object().unwrap().len(), 64);
    // packages/inna-mcp/src/client.ts RECORD_MAX_BYTES, evaluated by Bun.
    assert_eq!(worst.len(), 1_682_444);
    let refused = "Cannot use the Inna session store.";

    let case = Case::new("record-limit", false);
    case.home.update_record(|_| worst.clone());
    assert_eq!(case.home.record().as_deref(), Some(worst.as_str()));
    let exact = case
        .client()
        .call("inna_session_status", json!({}))
        .unwrap_err();
    assert!(!exact.contains(refused), "{exact}");
    assert_eq!(case.home.record().as_deref(), Some(worst.as_str()));

    // One byte more: a record past the bound, written by a store that allows it.
    let mut options = case.home.store_options();
    options.max_bytes += 1;
    let over = worst.replacen(r#"","account""#, r#"x","account""#, 1);
    assert_eq!(over.len(), worst.len() + 1);
    family_store::with_secret_store(&options, |store| store.write(&over)).unwrap();
    let past = case
        .client()
        .call("inna_session_status", json!({}))
        .unwrap_err();
    assert!(past.starts_with(refused), "{past}");
}
