//! The 30-minute renewal and the 401 retry, executed: tests/ts/renewal.test.ts against the
//! binary's `serve`, with the scaled bound (INNA_TEST_RENEWAL_MS) and fake-inna.ts
//! (support/fake.rs). Renewal ages are monotonic, so the bound is real time; the waits are on the
//! fake's state or the server's `keep-alive:` lines, and the two observation windows the
//! TypeScript test slept through are the time the bound itself must pass. Needs no Bun; needs the
//! `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::case::EXPORT;
use support::fake::{Fake, Planted, State};
use support::{Home, Mcp, NOW, PATIENCE, Run, Upstream, cli};

const USER: &str = "/api/UserData/GetLoggedInUser";
const SCHOOL: &str = "/api/Announcements/GetStudentAnnouncements";
const POST: &str = "/api/RegisterAbsence/AddNewLeave";
const SWITCH: &str = "/auth/system";
const WARNING: &str = "Inna renewal failed. School requests require successful renewal.";

fn ms(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

struct Fixture {
    home: Home,
    fake: Fake,
    upstream: Upstream,
    env: Vec<(String, String)>,
    mcp: Option<Mcp>,
    started: Instant,
    /// Every tool answer, for the leak check at close.
    answers: String,
}

impl Fixture {
    /// renewal.test.ts's `fixture(bound, args)`: an import, then `serve`, ready once its
    /// startup tick has saved its rotation.
    fn new(bound: u64, args: &[&str]) -> Self {
        let fake = Fake::default();
        let upstream = fake.upstream();
        let home = Home::new("renewal");
        let env = vec![
            (
                "INNA_SESSION_FILE".to_owned(),
                home.path("legacy.json").display().to_string(),
            ),
            ("INNA_TEST_RENEWAL_MS".to_owned(), bound.to_string()),
            ("INNA_TEST_KEEP_ALIVE".to_owned(), "1".to_owned()),
        ];
        home.write("cookies.json", EXPORT, 0o600);
        let mut fixture = Self {
            home,
            fake,
            upstream,
            env,
            mcp: None,
            started: Instant::now(),
            answers: String::new(),
        };
        fixture.import();
        fixture.started = Instant::now();
        let (mcp, _) = Mcp::start(&fixture.home, &fixture.upstream.origin, &fixture.env, args);
        let ready = mcp.until_stderr("keep-alive: kept", 1, ms(bound / 30 + 2_000));
        assert!(ready, "no startup renewal: {}", mcp.stderr());
        fixture.mcp = Some(mcp);
        fixture
    }

    /// `cli(['auth', 'import', source])`: exit 0, and no session value in its output.
    fn import(&self) {
        let source = self.home.path("cookies.json").display().to_string();
        let run: Run = cli(
            &self.home,
            &self.upstream.origin,
            &self.env,
            &["auth", "import", &source],
            None,
        );
        assert_eq!(run.code, Some(0), "{run:?}");
        assert!(!format!("{}{}", run.stdout, run.stderr).contains("synthetic-"));
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.fake.state()
    }

    fn seen(&self) -> usize {
        self.state().seen.len()
    }

    fn paths(&self, from: usize) -> Vec<String> {
        self.state().paths(from)
    }

    /// One tool call: the whole result (isError, content, structuredContent).
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let answer = self.mcp.as_mut().unwrap().call(name, arguments).value();
        self.answers.push_str(&answer.to_string());
        answer["result"].clone()
    }

    fn overview(&mut self) -> Value {
        self.call("inna_get_overview", json!({}))
    }

    /// renewal.test.ts's `until`: the fake's state gets there within `patience`.
    #[track_caller]
    fn until(&self, patience: Duration, ready: impl FnMut(&State) -> bool) {
        assert!(
            self.fake.until(patience, ready),
            "The fake request did not arrive in time."
        );
    }

    /// Lets `length` pass while the server runs: what the bound itself must outlast.
    fn window(&self, length: Duration) {
        self.fake.until(length, |_| false);
    }

    /// `close`: end of input, the exit, and no session value or decoy in anything written.
    fn close(mut self) -> String {
        let mcp = self.mcp.take().unwrap();
        let (_, stderr) = mcp.close();
        let text = format!("{}{stderr}", self.answers);
        // Every value, including rotations and the second process's import, uses this prefix.
        assert!(!text.contains("synthetic-"), "{text}");
        assert!(!text.contains("DO-NOT-RETURN"), "{text}");
        stderr
    }
}

fn is_error(result: &Value) -> bool {
    result["isError"] == true
}

/// The idle test's scale, coupled as renewal.test.ts:126 couples them (TTL = 2B, window = 6.2B),
/// at B = 6 s instead of 1.5 s: the slower cadence (`--no-keep-alive`) waits 2B/3 after each
/// renewal, which leaves B/3 for the request, the store and the scheduler. At 1.5 s that was
/// 0.5 s, and Linux containers took up to 0.88 s (review of PR #27, timing ruling (b)).
const IDLE_BOUND_MS: u64 = 6_000;
const IDLE_TTL_MS: u64 = 2 * IDLE_BOUND_MS;
const IDLE_WINDOW_MS: u64 = IDLE_BOUND_MS * 62 / 10;

/// renewal.test.ts:126: with the session expiring 2B after each renewal, 6.2B of idle serve
/// renew it more than six times, never more than the bound apart, with nothing but renewals.
#[test]
fn idle_serve_survives_several_ttls_within_the_bound_including_the_flag() {
    for args in [&[][..], &["--no-keep-alive"][..]] {
        let mut f = Fixture::new(IDLE_BOUND_MS, args);
        {
            let mut state = f.state();
            state.ttl = Some(ms(IDLE_TTL_MS));
            state.renewal_gaps = Some(Vec::new());
        }
        f.window(ms(IDLE_WINDOW_MS));
        {
            let state = f.state();
            assert!(
                state.rotations > 6,
                "{args:?}: {} rotations",
                state.rotations
            );
            let gaps = state.renewal_gaps.as_ref().unwrap();
            assert!(
                gaps.iter().max().unwrap() < &ms(IDLE_BOUND_MS),
                "{args:?}: {gaps:?}"
            );
        }
        assert!(f.paths(0).iter().all(|path| path == USER));
        assert!(!is_error(&f.overview()), "{args:?}");
        f.close();
    }
}

/// renewal.test.ts:143
#[test]
fn renewal_401_and_5xx_fail_closed_past_the_bound_without_a_school_request() {
    for status in [401, 500] {
        let mut f = Fixture::new(300, &[]);
        assert!(!is_error(&f.overview()));
        f.state()
            .planted
            .insert(USER.into(), Planted::status(status));
        f.window(ms(450));
        let from = f.seen();
        let result = f.overview();
        assert!(is_error(&result), "{status}: {result}");
        let expected = match status {
            401 => "sign-in is required",
            _ => "unavailable or unexpected",
        };
        assert!(result.to_string().contains(expected), "{result}");
        assert!(f.paths(from).iter().all(|path| path == USER));
        f.state().planted.remove(USER);
        assert!(!is_error(&f.overview()));
        f.close();
    }
}

/// renewal.test.ts:165
#[test]
fn read_401_refreshes_once_and_retries_once_and_a_second_401_sends_no_third() {
    for (failures, student_key) in [(1, None), (2, None), (1, Some("5"))] {
        let mut f = Fixture::new(60_000, &[]);
        f.state().once401.insert(SCHOOL.into(), failures);
        let from = f.seen();
        let arguments = match student_key {
            Some(key) => json!({"studentKey": key}),
            None => json!({}),
        };
        let result = f.call("inna_get_overview", arguments);
        assert_eq!(is_error(&result), failures == 2, "{result}");
        let paths = f.paths(from);
        assert_eq!(paths.iter().filter(|path| *path == SCHOOL).count(), 2);
        let first = paths.iter().position(|path| path == SCHOOL).unwrap();
        assert_eq!(paths[first..first + 3], [SCHOOL, USER, SCHOOL]);

        if failures == 2 {
            assert!(
                result.to_string().contains("sign-in is required"),
                "{result}"
            );
        }
        f.close();
    }
}

/// renewal.test.ts:188
#[test]
fn a_separate_cli_import_is_used_by_the_next_call() {
    let mut f = Fixture::new(60_000, &[]);
    f.overview();
    f.import();
    let rotated = format!("synthetic-rotated-{}", f.state().rotations);
    f.state().once401.insert(USER.into(), 1);
    let from = f.seen();
    assert!(!is_error(&f.overview()));
    let cookie = f.state().seen[from].request.headers["cookie"].clone();
    assert!(cookie.as_str().unwrap().contains(&rotated), "{cookie}");
    assert_eq!(f.paths(from)[..3], [USER, USER, USER]);
    f.close();
}

/// renewal.test.ts:204: the first school request outlasts the bound, so the call renews again
/// before its next one, and that renewal fails.
#[test]
fn renewal_age_is_checked_again_between_school_requests() {
    let mut f = Fixture::new(300, &[]);
    {
        let mut state = f.state();
        state.delays.insert(SCHOOL.into(), ms(350));
        state.after_user = Some(Box::new(|state: &mut State| {
            state.planted.insert(USER.into(), Planted::status(500));
        }));
    }
    let from = f.seen();
    let result = f.overview();
    assert!(is_error(&result));
    assert!(
        result.to_string().contains("unavailable or unexpected"),
        "{result}"
    );
    let paths = f.paths(from);
    assert_eq!(paths[..3], [USER, SCHOOL, USER], "{paths:?}");
    let schools: Vec<&String> = paths.iter().filter(|path| *path != USER).collect();
    assert_eq!(schools, [SCHOOL]);
    f.close();
}

/// renewal.test.ts:223. The clock is a day before the fake's leave history, which is in 2040,
/// as the TypeScript test's real today was.
#[test]
fn a_401_on_absence_post_sends_once_and_retains_the_uncertain_outcome() {
    let mut f = Fixture::new(60_000, &["--allow-absence-writes"]);
    let today = NOW - 30 * 86_400_000;
    f.home.set_clock(today);
    let day = "2039-12-03";
    {
        let mut state = f.state();
        let empty = Planted {
            status: 200,
            body: "[]".into(),
        };
        state.planted.insert(
            "/api/RegisterAbsence/GetStudentRegisteredAbsences".into(),
            empty.clone(),
        );
        state
            .planted
            .insert("/api/RegisterAbsence/GetStudentLeaves".into(), empty);
    }
    let preview = f.call(
        "inna_prepare_absence",
        json!({"kind": "leave", "dateFrom": day, "dateTo": day, "reason": "Test"}),
    );
    assert!(!is_error(&preview), "{preview}");
    f.state().planted.insert(POST.into(), Planted::status(401));
    let result = f.call(
        "inna_submit_absence",
        json!({"operationId": preview["structuredContent"]["operationId"], "confirm": true}),
    );
    assert!(is_error(&result));
    assert!(
        result.to_string().contains("outcome is uncertain"),
        "{result}"
    );
    assert_eq!(f.paths(0).iter().filter(|path| *path == POST).count(), 1);
    let status = f.call("inna_absence_status", json!({}));
    assert_eq!(status["structuredContent"]["operation"]["state"], "unknown");
    f.close();
}

/// renewal.test.ts:254: the first renewal comes within a retry interval (bound / 30) of start.
#[test]
fn serve_start_renews_within_the_bound_in_both_modes() {
    let (bound, slack) = (6_000, 400);

    for args in [&[][..], &["--no-keep-alive"][..]] {
        let f = Fixture::new(bound, args);
        f.until(ms(bound / 30 + slack), |state| state.rotations >= 2);
        assert!(f.started.elapsed() < ms(bound / 30 + slack), "{args:?}");
        assert!(f.paths(0).iter().all(|path| path == USER));
        f.close();
    }
}

/// renewal.test.ts:269
#[test]
fn a_switch_401_recovers_once_and_a_persistent_401_refuses_after_two_switches() {
    for persistent in [false, true] {
        let mut f = Fixture::new(60_000, &[]);
        match persistent {
            true => {
                f.state()
                    .planted
                    .insert(SWITCH.into(), Planted::status(401));
            }
            false => {
                f.state().once401.insert(SWITCH.into(), 1);
            }
        }
        let from = f.seen();
        let result = f.call("inna_get_overview", json!({"studentKey": "5"}));
        assert_eq!(is_error(&result), persistent, "{result}");
        let paths = f.paths(from);
        assert_eq!(paths.iter().filter(|path| *path == SWITCH).count(), 2);
        let first = paths.iter().position(|path| path == SWITCH).unwrap();
        assert_eq!(paths[first..first + 3], [SWITCH, USER, SWITCH]);

        if persistent {
            assert!(
                result
                    .to_string()
                    .contains("Inna refused the student switch and asked for sign-in."),
                "{result}"
            );
        }
        f.close();
    }
}

/// renewal.test.ts:292
#[test]
fn a_500_renewal_after_a_checked_read_401_keeps_the_transient_text_and_sends_no_retry() {
    let mut f = Fixture::new(60_000, &[]);
    {
        let mut state = f.state();
        state.after_user = Some(Box::new(|state: &mut State| {
            state.planted.insert(USER.into(), Planted::status(500));
        }));
        state.once401.insert(SCHOOL.into(), 1);
    }
    let from = f.seen();
    let result = f.overview();
    assert!(is_error(&result));
    assert!(
        result.to_string().contains("unavailable or unexpected"),
        "{result}"
    );
    assert_eq!(f.paths(from), [USER, SCHOOL, USER]);
    f.close();
}

/// renewal.test.ts:309: failed ticks retry every bound / 30, well before the cadence, and warn
/// once the last renewal is older than the bound; the next success renews.
#[test]
fn failed_ticks_retry_before_cadence_and_warn_once_past_the_age_bound() {
    let bound = 1_500;
    let retry = bound / 30;
    let cadence = bound * 2 / 3;
    let f = Fixture::new(bound, &["--no-keep-alive"]);
    f.until(PATIENCE, |state| state.rotations >= 2);
    let kept_at = f.state().renewed_at.unwrap();
    f.state().planted.insert(USER.into(), Planted::status(500));
    let from = f.seen();
    f.until(PATIENCE, |state| state.seen.len() > from);
    let failed_at = Instant::now();
    let failed = f.seen();
    f.until(ms(cadence / 2), |state| state.seen.len() >= failed + 2);
    assert!(failed_at.elapsed() < ms(cadence / 2));
    // Added here: the failures so far are inside the bound, so nothing has warned yet.
    assert!(kept_at.elapsed() < ms(bound));
    let early = f.mcp.as_ref().unwrap().stderr();
    assert!(!early.contains(WARNING), "{early}");
    f.window(
        ms(bound + retry * 3)
            .checked_sub(kept_at.elapsed())
            .unwrap_or_default(),
    );
    f.state().planted.remove(USER);
    f.until(ms(cadence / 2), |state| state.rotations >= 3);
    assert!(f.paths(0).iter().all(|path| path == USER));
    let stderr = f.close();
    assert_eq!(stderr.matches(WARNING).count(), 1, "{stderr}");
}
