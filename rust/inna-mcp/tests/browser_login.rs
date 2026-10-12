//! Google sign-in through a private debugging pipe, executed: tests/ts/browser-login.test.ts
//! against `inna-mcp auth login --google`, with fake-browser.ts ported to this test binary
//! (support/browser.rs). The upstream is browser-login.test.ts's `createUpstream`. Needs no Bun;
//! needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs;

use support::browser::{
    Login, START_MESSAGE, TIMEOUT_MESSAGE, USER_PATH, collect, pid_is_running, signal, upstream,
};

const PREVIOUS: &str = r#"{"synthetic":"previous session file"}"#;

/// Not a case: the fake browser, run by `fake-chrome` (support/browser.rs).
#[test]
#[ignore = "run as the fake browser by support/browser.rs's fake-chrome"]
fn fake_browser() {
    support::browser::fake_browser();
}

/// `savePreviousSession`.
fn save_previous(login: &Login) {
    login.home.write("session.json", PREVIOUS, 0o600);
}

fn previous_kept(login: &Login) {
    assert_eq!(fs::read_to_string(&login.session).unwrap(), PREVIOUS);
}

/// The encrypted record (`storeAt(storeHome(directory)).path`).
fn record(login: &Login) -> std::path::PathBuf {
    login.home.path(".config/inna-mcp/session.enc")
}

fn profile_removed(state: &support::browser::State) {
    assert!(!state.profile.exists(), "{}", state.profile.display());
}

/// browser-login.test.ts:138
#[test]
fn google_login_captures_over_a_private_pipe_closes_the_browser_then_verifies_and_saves() {
    let login = Login::new("google-private");
    let (upstream, seen) = upstream(&login, true);
    let env = login.environment(&[("INNA_FAKE_EMPTY_POLLS", "1")]);
    let child = login.spawn(&login.browser, &env, 20, Some(&upstream.origin), &[]);
    let state = login.state();
    let (exit, stdout, stderr) = collect(child);

    assert_eq!(stderr, START_MESSAGE);
    assert_eq!(stdout, "Signed in. Saved in an encrypted file.\n");
    assert_eq!(exit, Some(0));
    assert!(!(stdout.clone() + &stderr).contains("synthetic-"));
    assert!(!(stdout + &stderr).contains("decoy-"));

    let debugging: Vec<&String> = state
        .args
        .iter()
        .filter(|argument| argument.contains("remote-debugging"))
        .collect();
    assert_eq!(debugging, ["--remote-debugging-pipe"]);
    assert_eq!(
        state.args.last().map(String::as_str),
        Some("https://r.inna.is/auth/google")
    );
    assert!(
        state
            .args
            .contains(&format!("--user-data-dir={}", state.profile.display()))
    );
    assert!(state.profile.starts_with(&login.temporary));
    assert!(
        state
            .profile
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("inna-login-")
    );
    assert_eq!(state.profile_mode, 0o700);
    assert_eq!(state.transport, "pipe");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].path, USER_PATH);
    assert_eq!(seen[0].xsrf.as_deref(), Some("synthetic-xsrf"));
    assert!(
        seen[0].browser_closed && seen[0].profile_removed,
        "{seen:?}"
    );
    let cookie = seen[0].cookie.clone().unwrap_or_default();
    assert!(cookie.contains("SESSION=synthetic-session"), "{cookie}");
    assert!(!cookie.contains("decoy"), "{cookie}");

    let saved = login.home.record().expect("a saved session");
    assert!(!saved.contains("decoy"));
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["version"], 2);
    assert_eq!(
        saved["account"],
        serde_json::json!({"userId": 1, "studentId": "2", "schoolId": "3"})
    );
    let jar: serde_json::Value = serde_json::from_str(saved["jar"].as_str().unwrap()).unwrap();
    let cookies = jar["cookies"].as_array().unwrap();
    let mut pairs: Vec<String> = cookies
        .iter()
        .map(|cookie| {
            format!(
                "{}={}",
                cookie["key"].as_str().unwrap(),
                cookie["value"].as_str().unwrap()
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        [
            "JSESSIONID=synthetic-jsession",
            "SESSION=synthetic-session",
            "XSRF-TOKEN=synthetic-xsrf"
        ]
    );

    for cookie in cookies {
        assert_eq!(
            (&cookie["domain"], &cookie["path"], &cookie["secure"]),
            (
                &serde_json::json!("nam.inna.is"),
                &serde_json::json!("/"),
                &serde_json::json!(true)
            ),
            "{cookie}"
        );
    }
    let store = record(&login);
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&store).unwrap().permissions())
            & 0o777,
        0o600
    );
    // The session exists only encrypted: no plaintext file, and no cookie value on disk.
    assert!(!login.session.exists());
    assert_eq!(
        login.files_containing(&["synthetic-session", "synthetic-jsession", "synthetic-xsrf"]),
        Vec::<std::path::PathBuf>::new()
    );
    profile_removed(&state);
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
    assert!(login.closed());
    // It closed at Browser.close: it was never signalled.
    assert!(!login.signal_file.exists());
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:539
#[test]
fn google_login_removes_the_profile_when_browser_readiness_fails() {
    let login = Login::new("google-readiness");
    let env = login.environment(&[("INNA_FAKE_BAD_READINESS", "1")]);
    let child = login.spawn(&login.browser, &env, 20, None, &[]);
    let state = login.state();
    let (exit, _, stderr) = collect(child);

    assert_eq!(exit, Some(1));
    assert!(
        stderr.contains("Could not establish a private Chrome debugging pipe."),
        "{stderr}"
    );
    profile_removed(&state);
    assert!(!pid_is_running(state.pid));
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
}

/// browser-login.test.ts:568
#[test]
fn google_login_removes_the_profile_when_the_browser_exits_before_readiness() {
    let login = Login::new("google-early-exit");
    let env = login.environment(&[("INNA_FAKE_EXIT_BEFORE_READY", "1")]);
    let child = login.spawn(&login.browser, &env, 20, None, &[]);
    let state = login.state();
    let (exit, _, stderr) = collect(child);

    assert_eq!(exit, Some(1));
    assert!(
        stderr.contains("Could not establish a private Chrome debugging pipe."),
        "{stderr}"
    );
    profile_removed(&state);
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
}

/// browser-login.test.ts:596
#[test]
fn google_login_reports_pipe_setup_failure_and_removes_its_profile() {
    let login = Login::new("google-spawn");
    let invalid = login.home.path("invalid-chrome");
    login
        .home
        .write("invalid-chrome", "not an executable browser", 0o700);
    let env = login.environment(&[]);
    let (exit, _, stderr) = collect(login.spawn(&invalid, &env, 20, None, &[]));

    assert_eq!(exit, Some(1));
    assert!(
        stderr.contains(
            "Could not establish a private Chrome debugging pipe. Select Google Chrome or Chromium with `--browser`, or use electronic ID (`inna-mcp auth login`) or `inna-mcp auth import`."
        ),
        "{stderr}"
    );
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
}

/// browser-login.test.ts:223
#[test]
fn google_login_waits_through_the_google_pages_until_the_student_application() {
    let login = Login::new("google-wait");
    let (upstream, seen) = upstream(&login, true);
    // The school cookies exist from the first poll; the first page poll still shows Google.
    let env = login.environment(&[("INNA_FAKE_SIGNIN_POLLS", "1")]);
    let (exit, _, stderr) =
        collect(login.spawn(&login.browser, &env, 20, Some(&upstream.origin), &[]));

    assert_eq!(stderr, START_MESSAGE);
    assert_eq!(exit, Some(0));
    let closed: Vec<bool> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|seen| seen.browser_closed)
        .collect();
    assert_eq!(closed, [true]);
    let saved: serde_json::Value = serde_json::from_str(&login.home.record().unwrap()).unwrap();
    let jar: serde_json::Value = serde_json::from_str(saved["jar"].as_str().unwrap()).unwrap();
    let mut keys: Vec<&str> = jar["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cookie| cookie["key"].as_str().unwrap())
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["JSESSIONID", "SESSION", "XSRF-TOKEN"]);
}

/// browser-login.test.ts:266
#[test]
fn google_login_keeps_waiting_without_both_page_and_cookies_then_times_out() {
    let waiting: [(&str, (&str, &str)); 8] = [
        (
            "cookies while the page is still at Google",
            ("INNA_FAKE_SIGNIN_POLLS", "999"),
        ),
        (
            "the student path on another host",
            (
                "INNA_FAKE_PAGE_URL",
                "https://r.inna.is/Components/Students/Students.html",
            ),
        ),
        (
            "the student path on a look-alike host",
            (
                "INNA_FAKE_PAGE_URL",
                "https://nam.inna.is.example/Components/Students/Students.html",
            ),
        ),
        (
            "the student path over plain HTTP",
            (
                "INNA_FAKE_PAGE_URL",
                "http://nam.inna.is/Components/Students/Students.html",
            ),
        ),
        (
            "another path on the school host",
            (
                "INNA_FAKE_PAGE_URL",
                "https://nam.inna.is/Components/Login/Login.html",
            ),
        ),
        (
            "the student page named only in a query",
            (
                "INNA_FAKE_PAGE_URL",
                "https://nam.inna.is/login?next=/Components/Students/Students.html",
            ),
        ),
        (
            "the student page without the XSRF cookie",
            ("INNA_FAKE_XSRF", "0"),
        ),
        (
            "the student page before the school cookies exist",
            ("INNA_FAKE_EMPTY_POLLS", "999"),
        ),
    ];
    let runs: Vec<_> = waiting
        .into_iter()
        .map(|(name, overrides)| {
            std::thread::spawn(move || {
                let login = Login::new("google-waiting");
                save_previous(&login);
                let (upstream, seen) = upstream(&login, true);
                let env = login.environment(&[overrides]);
                let child = login.spawn(&login.browser, &env, 1, Some(&upstream.origin), &[]);
                let state = login.state();
                let (exit, stdout, stderr) = collect(child);

                assert_eq!(
                    (name, stderr.as_str()),
                    (name, format!("{START_MESSAGE}{TIMEOUT_MESSAGE}").as_str())
                );
                assert_eq!(stdout, "", "{name}");
                assert_eq!(exit, Some(1), "{name}");
                assert!(seen.lock().unwrap().is_empty(), "{name}");
                previous_kept(&login);
                profile_removed(&state);
                assert!(login.closed(), "{name}");
                assert!(!pid_is_running(state.pid), "{name}");
            })
        })
        .collect();

    for run in runs {
        run.join().unwrap();
    }
}

/// browser-login.test.ts:337
#[test]
fn google_login_closes_a_launcher_spawned_browser_child_before_removing_its_profile() {
    let login = Login::new("google-launcher");
    let (upstream, seen) = upstream(&login, true);
    let env = login.environment(&[("INNA_FAKE_LAUNCHER", "1")]);
    let child = login.spawn(&login.browser, &env, 20, Some(&upstream.origin), &[]);
    let state = login.state();
    let (exit, _, stderr) = collect(child);

    assert_eq!(stderr, START_MESSAGE);
    assert_eq!(exit, Some(0));
    assert_eq!(state.profile_mode, 0o700);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].browser_closed && seen[0].profile_removed,
        "{seen:?}"
    );
    profile_removed(&state);
    assert!(login.closed());
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:375
#[test]
fn google_login_timeout_closes_a_launcher_spawned_browser_child() {
    let login = Login::new("google-launcher-timeout");
    let env = login.environment(&[
        ("INNA_FAKE_SIGNIN_POLLS", "999"),
        ("INNA_FAKE_LAUNCHER", "1"),
    ]);
    let child = login.spawn(&login.browser, &env, 1, None, &[]);
    let state = login.state();
    let (exit, _, stderr) = collect(child);

    assert_eq!(exit, Some(1));
    assert_eq!(stderr, format!("{START_MESSAGE}{TIMEOUT_MESSAGE}"));
    assert!(!login.session.exists());
    assert!(!record(&login).exists());
    profile_removed(&state);
    assert!(login.closed());
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:407
#[test]
fn google_login_cancelled_while_waiting_closes_the_browser_and_saves_nothing() {
    let login = Login::new("google-cancel");
    save_previous(&login);
    let env = login.environment(&[("INNA_FAKE_SIGNIN_POLLS", "999")]);
    let child = login.spawn(&login.browser, &env, 60, None, &[]);
    let state = login.state();
    signal(child.id(), "INT");
    let (exit, stdout, stderr) = collect(child);

    assert_eq!(stderr, format!("{START_MESSAGE}Inna login cancelled.\n"));
    assert_eq!(stdout, "");
    assert_eq!(exit, Some(1));
    previous_kept(&login);
    profile_removed(&state);
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
    assert!(login.closed());
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:442
#[test]
fn google_login_keeps_signal_handlers_through_delayed_sigterm_and_sigkill_cleanup() {
    let login = Login::new("google-signals");
    save_previous(&login);
    let (upstream, seen) = upstream(&login, true);
    let env = login.environment(&[
        ("INNA_FAKE_IGNORE_BROWSER_CLOSE", "1"),
        ("INNA_FAKE_DELAY_SIGTERM", "1"),
    ]);
    let mut child = login.spawn(&login.browser, &env, 20, Some(&upstream.origin), &[]);
    let state = login.state();
    let pid = child.id();
    signal(pid, "INT");
    let deadline = std::time::Instant::now() + support::PATIENCE;

    while !login.signal_file.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the browser was never sent SIGTERM"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    signal(pid, "INT");
    std::thread::sleep(std::time::Duration::from_millis(100));

    if child.try_wait().unwrap().is_none() {
        signal(pid, "INT");
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    // Neither repeated SIGINT ended it: it is still cleaning up.
    assert!(child.try_wait().unwrap().is_none());

    let (exit, stdout, stderr) = collect(child);
    assert_eq!(exit, Some(1));
    assert!(stderr.contains("Inna login cancelled."), "{stderr}");
    assert_eq!(stdout, "");
    assert!(seen.lock().unwrap().is_empty());
    previous_kept(&login);
    assert_eq!(fs::read_to_string(&login.signal_file).unwrap(), "SIGTERM");
    // SIGKILL ended it before its own delayed close: it never wrote its exit file.
    assert!(!login.closed());
    profile_removed(&state);
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:498
#[test]
fn google_login_keeps_the_previous_session_when_verification_refuses() {
    let login = Login::new("google-verify");
    let (upstream, seen) = upstream(&login, false);
    let env = login.environment(&[]);
    let child = login.spawn(&login.browser, &env, 20, Some(&upstream.origin), &[]);
    let state = login.state();
    let (exit, stdout, stderr) = collect(child);

    assert_eq!(exit, Some(1));
    assert!(stderr.contains("sign-in is required"), "{stderr}");
    assert_eq!(stdout, "");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].browser_closed && seen[0].profile_removed,
        "{seen:?}"
    );
    assert!(!login.session.exists());
    assert!(!record(&login).exists());
    profile_removed(&state);
    assert!(login.closed());
    assert!(!pid_is_running(state.pid));
}

/// browser-login.test.ts:653
#[test]
fn google_login_errors_when_an_ignored_launcher_child_may_still_be_running() {
    let login = Login::new("google-orphan");
    let env = login.environment(&[
        ("INNA_FAKE_LAUNCHER", "1"),
        ("INNA_FAKE_IGNORE_BROWSER_CLOSE", "1"),
    ]);
    let child = login.spawn(&login.browser, &env, 20, None, &[]);
    let state = login.state();
    let (exit, stdout, stderr) = collect(child);

    assert_eq!(exit, Some(1));
    assert!(
        stderr
            .contains("A browser process may still be running; its temporary profile was removed."),
        "{stderr}"
    );
    assert_eq!(stdout, "");
    assert!(!login.session.exists());
    assert!(!record(&login).exists());
    profile_removed(&state);
    // Login's drop kills it.
    assert!(pid_is_running(state.pid));
}

/// browser-login.test.ts:616. The sweep runs before the browser is looked for, and only on this
/// server's own profiles in its TMPDIR.
#[test]
fn google_login_sweeps_abandoned_profiles_but_preserves_recent_and_live_profiles() {
    let login = Login::new("google-sweep");
    let profile = |name: &str| {
        let path = login.temporary.join(name);
        fs::create_dir(&path).unwrap();
        path
    };
    let (abandoned, recent, active, foreign) = (
        profile("inna-login-abandonedX"),
        profile("inna-login-recentX"),
        profile("inna-login-activeX"),
        profile("abler-login-abandonedX"),
    );
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    let age = |path: &std::path::Path| fs::File::open(path).unwrap().set_modified(old).unwrap();
    std::os::unix::fs::symlink(
        format!("test-host-{}", std::process::id()),
        active.join("SingletonLock"),
    )
    .unwrap();

    for path in [&abandoned, &foreign, &active] {
        age(path);
    }
    let missing = login.home.path("missing-browser");
    let env = login.environment(&[]);
    let (exit, stdout, stderr) = collect(login.spawn(&missing, &env, 20, None, &[]));

    assert_eq!(exit, Some(1));
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "Google sign-in requires Google Chrome or Chromium, and none was found. Install one, or give its executable with --browser or INNA_BROWSER. Electronic ID (`inna-mcp auth login`) needs no browser.\n"
    );
    assert!(!abandoned.exists());
    assert!(recent.is_dir() && active.is_dir() && foreign.is_dir());
}

/// browser-login.test.ts:687
#[test]
fn google_login_rediscovers_the_student_tab_after_its_pipe_session_detaches() {
    let login = Login::new("google-detached");
    let (upstream, seen) = upstream(&login, true);
    let env = login.environment(&[
        ("INNA_FAKE_EMPTY_POLLS", "1"),
        ("INNA_FAKE_DETACH_ON_EMPTY_POLL", "1"),
    ]);
    let child = login.spawn(&login.browser, &env, 10, Some(&upstream.origin), &[]);
    let state = login.state();
    let (exit, _, stderr) = collect(child);

    assert_eq!(stderr, START_MESSAGE);
    assert_eq!(exit, Some(0));
    let saved: serde_json::Value = serde_json::from_str(&login.home.record().unwrap()).unwrap();
    let jar: serde_json::Value = serde_json::from_str(saved["jar"].as_str().unwrap()).unwrap();
    let mut keys: Vec<&str> = jar["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cookie| cookie["key"].as_str().unwrap())
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["JSESSIONID", "SESSION", "XSRF-TOKEN"]);
    let paths: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|seen| seen.path.clone())
        .collect();
    assert_eq!(paths, [USER_PATH]);
    profile_removed(&state);
    assert!(!pid_is_running(state.pid));
}

/// packages/inna-mcp/test/browser-login.test.ts:176, its `loginInBrowser` half through the
/// binary: on Linux without a display the sign-in fails before anything is launched. Its
/// `requireDisplay` table is src/google.rs::a_missing_display_on_linux_fails_first.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_display_on_linux_fails_before_anything_is_launched() {
    let login = Login::new("google-display");
    let marker = login.home.path("launched");
    login.home.write(
        "marker-browser",
        &format!("#!/bin/sh\n: > '{}'\n", marker.display()),
        0o700,
    );
    let env = login.environment(&[("DISPLAY", ""), ("WAYLAND_DISPLAY", "")]);
    let (exit, stdout, stderr) =
        collect(login.spawn(&login.home.path("marker-browser"), &env, 5, None, &[]));

    assert_eq!(exit, Some(1));
    assert_eq!(stdout, "");
    assert!(stderr.contains("needs a desktop session"), "{stderr}");
    assert!(!marker.exists());
    assert_eq!(login.temporary_entries(), Vec::<String>::new());
}
