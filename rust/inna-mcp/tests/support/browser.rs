//! packages/inna-mcp/test/fake-browser.ts and tests/ts/browser-harness.ts without Bun. The fake
//! browser is this test binary again: `fake-chrome`, a `/bin/sh` wrapper, puts the browser's
//! arguments in INNA_FAKE_ARGS and re-runs the binary's ignored `fake_browser` test with the
//! debugging pipe moved from descriptors 3 and 4 to 0 and 2 (safe Rust reads only those; libtest
//! writes its own lines to 1, which goes nowhere). It speaks the pipe's NUL-terminated JSON as
//! fake-browser.ts did, controlled by the same INNA_FAKE_* variables.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Home, PATIENCE, Reply, Request, Upstream};

pub const START_MESSAGE: &str = "A browser window is opening. Sign in to Inna with your Google account there; this window closes by itself when you are done.\n";

pub const TIMEOUT_MESSAGE: &str = "Inna sign-in was not finished in time, and nothing was saved. Run the command again, or use electronic ID (`inna-mcp auth login`) or `inna-mcp auth import`.\n";

/// The arguments libtest needs to run only the fake browser.
const ENTRY: [&str; 4] = ["--ignored", "--exact", "fake_browser", "--test-threads=1"];

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

fn number(name: &str) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// fake-browser.ts, run by `fake-chrome` as each test binary's ignored `fake_browser`. Without
/// the wrapper's arguments it does nothing. It never returns: it ends as the browser would.
pub fn fake_browser() {
    let Ok(arguments) = std::env::var("INNA_FAKE_ARGS") else {
        return;
    };
    let args: Vec<String> = arguments.lines().map(str::to_owned).collect();
    let profile = args
        .iter()
        .find_map(|argument| argument.strip_prefix("--user-data-dir="))
        .expect("a profile")
        .to_owned();
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
    let (state_file, exit_file, signal_file) = (
        var("INNA_FAKE_BROWSER_STATE"),
        var("INNA_FAKE_BROWSER_EXIT"),
        var("INNA_FAKE_BROWSER_SIGNAL"),
    );
    let write_state = || {
        let mode = fs::metadata(&profile).unwrap().permissions().mode() & 0o777;
        let state = json!({
            "pid": std::process::id(),
            "profile": profile,
            "profileMode": mode,
            "transport": "pipe",
            "args": args,
        });
        fs::write(&state_file, state.to_string()).unwrap();
    };

    if flag("INNA_FAKE_LAUNCHER") {
        // Bun.spawn's stdio ['ignore', 'ignore', 'ignore', 3, 4]: here the pipe is on 0 and 2.
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(ENTRY)
            .env("INNA_FAKE_LAUNCHER", "0")
            .env("INNA_FAKE_LAUNCHER_CHILD", "1")
            .stdout(Stdio::null());

        // `detached`: outside the launcher's process group, which the binary signals.
        if flag("INNA_FAKE_IGNORE_BROWSER_CLOSE") {
            command.process_group(0);
        }
        command.spawn().unwrap();
        std::process::exit(0);
    }

    if flag("INNA_FAKE_EXIT_BEFORE_READY") {
        write_state();
        std::process::exit(23);
    }
    assert!(
        args.iter()
            .any(|argument| argument == "--remote-debugging-pipe"),
        "The fake browser requires a private debugging pipe."
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).unwrap();
        let (close, mut closing) = tokio::sync::mpsc::unbounded_channel::<()>();
        let commands = close.clone();
        std::thread::spawn(move || serve(&commands));
        // The state file tells the test the browser is ready, so it may be signalled from then on.
        write_state();

        loop {
            tokio::select! {
                _ = terminate.recv() => {
                    fs::write(&signal_file, "SIGTERM").unwrap();

                    if flag("INNA_FAKE_IGNORE_SIGTERM") {
                        continue;
                    }

                    if flag("INNA_FAKE_DELAY_SIGTERM") {
                        let later = close.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            let _ = later.send(());
                        });
                        continue;
                    }
                    break;
                }
                Some(()) = closing.recv() => break,
            }
        }
        fs::write(&exit_file, "closed").unwrap();
        std::process::exit(0);
    });
}

/// Answer the pipe's commands until it ends. The browser stays up after that, as fake-browser.ts
/// did (its keep-alive interval), until it is closed or signalled.
fn serve(close: &tokio::sync::mpsc::UnboundedSender<()>) {
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut polls = 0;
    let mut target_polls = 0;
    let mut current_target = "fake-page";
    let detach = flag("INNA_FAKE_DETACH_ON_EMPTY_POLL");
    let students = "https://nam.inna.is/Components/Students/Students.html#!/home";
    let session = |name: &str, value: &str, domain: &str| {
        json!({
            "name": name, "value": value, "domain": domain, "path": "/", "expires": -1,
            "session": true, "secure": true, "httpOnly": true,
        })
    };
    // Chrome answers Network.getCookies with far more than the school session; none may be saved.
    let mut path_decoy = session("SESSION", "decoy-path", "nam.inna.is");
    path_decoy["path"] = json!("/api");
    let decoys = vec![
        session("SESSION", "decoy-portal", "r.inna.is"),
        session("SESSION", "decoy-parent", ".inna.is"),
        path_decoy,
        session("JSESSIONID", "decoy-google", "accounts.google.com"),
        session("SID", "decoy-sid", ".google.com"),
        session("tracking", "decoy-other", "nam.inna.is"),
        session("empty", "", "nam.inna.is"),
    ];
    let mut cookies = decoys.clone();
    cookies.push(session("SESSION", "synthetic-session", "nam.inna.is"));
    cookies.push(session("JSESSIONID", "synthetic-jsession", "nam.inna.is"));

    if std::env::var("INNA_FAKE_XSRF").as_deref() != Ok("0") {
        let mut xsrf = session("XSRF-TOKEN", "synthetic-xsrf", "nam.inna.is");
        xsrf["secure"] = json!(false);
        xsrf["httpOnly"] = json!(false);
        cookies.push(xsrf);
    }

    loop {
        let mut frame = Vec::new();

        if input.read_until(0, &mut frame).unwrap_or(0) == 0 || frame.pop() != Some(0) {
            return;
        }
        let command: Value = serde_json::from_slice(&frame).unwrap();
        let method = command["method"].as_str().unwrap();
        let result = match method {
            "Browser.getVersion" => match flag("INNA_FAKE_BAD_READINESS") {
                true => json!({}),
                false => json!({"product": "Chrome/1.0"}),
            },
            "Target.getTargets" => {
                target_polls += 1;
                let url = match target_polls <= number("INNA_FAKE_SIGNIN_POLLS") {
                    true => "https://accounts.google.com/v3/signin/identifier".to_owned(),
                    false => std::env::var("INNA_FAKE_PAGE_URL").unwrap_or(students.to_owned()),
                };
                json!({"targetInfos": [
                    {"targetId": "fake-worker", "type": "service_worker", "url": students},
                    {"targetId": current_target, "type": "page", "url": url},
                ]})
            }
            "Target.attachToTarget" => {
                json!({"sessionId": command["params"]["targetId"].as_str().unwrap_or(current_target)})
            }
            "Network.getCookies" if detach && command["sessionId"] != current_target => {
                write(&json!({
                    "id": command["id"],
                    "error": {"code": -32001, "message": "Session with given id not found."},
                }));
                continue;
            }
            "Network.getCookies" => {
                polls += 1;
                match polls <= number("INNA_FAKE_EMPTY_POLLS") {
                    true => json!({"cookies": decoys}),
                    false => json!({"cookies": cookies}),
                }
            }
            _ => json!({}),
        };
        write(&json!({"id": command["id"], "result": result}));

        if method == "Network.getCookies" && detach && polls == 1 {
            current_target = "replacement-page";
            write(&json!({
                "method": "Target.detachedFromTarget",
                "params": {"sessionId": "fake-page"},
            }));
        }

        if method == "Browser.close" && !flag("INNA_FAKE_IGNORE_BROWSER_CLOSE") {
            let close = close.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                let _ = close.send(());
            });
        }
    }
}

/// One frame on the pipe.
fn write(frame: &Value) {
    let mut output = std::io::stderr().lock();
    output
        .write_all(format!("{frame}\0").as_bytes())
        .and_then(|()| output.flush())
        .unwrap();
}

/// browser-harness.ts's `browserStateSchema`.
#[derive(Debug, Clone)]
pub struct State {
    pub pid: u32,
    pub profile: PathBuf,
    pub profile_mode: u32,
    pub transport: String,
    pub args: Vec<String>,
}

fn parse_state(text: &str) -> Option<State> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(State {
        pid: u32::try_from(value["pid"].as_u64()?).ok()?,
        profile: PathBuf::from(value["profile"].as_str()?),
        profile_mode: u32::try_from(value["profileMode"].as_u64()?).ok()?,
        transport: value["transport"].as_str()?.to_owned(),
        args: value["args"]
            .as_array()?
            .iter()
            .map(|argument| argument.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
    })
}

/// One run's scratch: browser-harness.ts's `makeTestDirectory`, `makeFakeBrowser` and
/// `browserEnvironment`, in a home whose store is the binary's own default under it.
pub struct Login {
    pub home: Home,
    /// The run's TMPDIR, where the binary makes its profile.
    pub temporary: PathBuf,
    pub browser: PathBuf,
    /// INNA_SESSION_FILE.
    pub session: PathBuf,
    pub state_file: PathBuf,
    pub exit_file: PathBuf,
    pub signal_file: PathBuf,
    /// Pids to kill when the run is over (`stopChild(child, state?.pid)`).
    pub kill: Mutex<Vec<u32>>,
}

impl Login {
    pub fn new(name: &str) -> Self {
        let home = Home::new(name);
        let temporary = home.path("tmp");
        fs::create_dir(&temporary).unwrap();
        let browser = home.path("fake-chrome");
        let quote =
            |path: &Path| format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
        fs::write(
            &browser,
            format!(
                "#!/bin/sh\nINNA_FAKE_ARGS=$(printf '%s\\n' \"$@\")\nexport INNA_FAKE_ARGS\nexec {} {} 0<&3 2>&4 >/dev/null 3<&- 4>&-\n",
                quote(&std::env::current_exe().unwrap()),
                ENTRY.join(" "),
            ),
        )
        .unwrap();
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            session: home.path("session.json"),
            state_file: home.path("browser.json"),
            exit_file: home.path("browser.closed"),
            signal_file: home.path("browser.signal"),
            temporary,
            browser,
            home,
            kill: Mutex::new(Vec::new()),
        }
    }

    /// `browserEnvironment` with `overrides`.
    pub fn environment(&self, overrides: &[(&str, &str)]) -> Vec<(String, String)> {
        let path = |path: &Path| path.display().to_string();
        let mut env = vec![
            ("INNA_SESSION_FILE".to_owned(), path(&self.session)),
            // A test that forgets --browser must fail instead of opening the machine's browser.
            (
                "INNA_BROWSER".to_owned(),
                path(&self.home.path("no-browser-selected")),
            ),
            ("INNA_FAKE_BROWSER_STATE".to_owned(), path(&self.state_file)),
            ("INNA_FAKE_BROWSER_EXIT".to_owned(), path(&self.exit_file)),
            (
                "INNA_FAKE_BROWSER_SIGNAL".to_owned(),
                path(&self.signal_file),
            ),
            ("INNA_FAKE_LAUNCHER".to_owned(), "0".to_owned()),
            // The Linux display check must not depend on the machine running the tests.
            ("DISPLAY".to_owned(), ":0".to_owned()),
            ("TMPDIR".to_owned(), path(&self.temporary)),
        ];
        env.extend(
            overrides
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        env
    }

    /// `spawnLogin`: `auth login --google --timeout <seconds> --browser <browser>`, with `origin`
    /// or, without one, a closed loopback port, so no bug can reach Inna.
    pub fn spawn(
        &self,
        browser: &Path,
        env: &[(String, String)],
        seconds: u32,
        origin: Option<&str>,
        extra: &[&str],
    ) -> Child {
        self.spawn_at(&self.home, browser, env, seconds, origin, extra)
    }

    /// `spawn` in another home: its store and session.
    pub fn spawn_at(
        &self,
        home: &Home,
        browser: &Path,
        env: &[(String, String)],
        seconds: u32,
        origin: Option<&str>,
        extra: &[&str],
    ) -> Child {
        let seconds = seconds.to_string();
        let browser = browser.display().to_string();
        let mut args = vec![
            "auth",
            "login",
            "--google",
            "--timeout",
            &seconds,
            "--browser",
            &browser,
        ];
        args.extend(extra);
        home.command(origin.unwrap_or("http://127.0.0.1:9"), env, &args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// `waitForBrowserState`.
    pub fn state(&self) -> State {
        let deadline = Instant::now() + PATIENCE;

        loop {
            if let Some(state) = fs::read_to_string(&self.state_file)
                .ok()
                .as_deref()
                .and_then(parse_state)
            {
                self.kill.lock().unwrap().push(state.pid);
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "The fake browser did not start in time."
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The browser's exit file: `closed` once it closed itself.
    pub fn closed(&self) -> bool {
        fs::read_to_string(&self.exit_file).is_ok_and(|text| text == "closed")
    }

    /// Every file under the home whose bytes contain one of `values` (`filesContaining`).
    pub fn files_containing(&self, values: &[&str]) -> Vec<PathBuf> {
        fn walk(directory: &Path, values: &[&str], found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();

                if path.is_dir() {
                    walk(&path, values, found);
                } else if let Ok(bytes) = fs::read(&path) {
                    let text = String::from_utf8_lossy(&bytes);

                    if values.iter().any(|value| text.contains(value)) {
                        found.push(path);
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.home.0, values, &mut found);
        found
    }

    /// The temporary directory's entries (`readdir(temporaryDirectory)`).
    pub fn temporary_entries(&self) -> Vec<String> {
        fs::read_dir(&self.temporary)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        for pid in self.kill.lock().unwrap().iter() {
            if pid_is_running(*pid) {
                signal(*pid, "KILL");
            }
        }
    }
}

/// `collectProcess`: the exit code, stdout and stderr.
pub fn collect(mut child: Child) -> (Option<i32>, String, String) {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).unwrap();
        text
    });
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();

    let Some(status) = super::wait(&mut child, PATIENCE) else {
        let _ = child.kill();
        panic!("the sign-in did not finish: {errors}");
    };
    (status.code(), reader.join().unwrap(), errors)
}

/// `process.kill(pid, 0)`.
pub fn pid_is_running(pid: u32) -> bool {
    Command::new("/bin/sh")
        .args(["-c", r#"kill -0 "$0" 2>/dev/null"#, &pid.to_string()])
        .status()
        .unwrap()
        .success()
}

pub fn signal(pid: u32, signal: &str) {
    let status = Command::new("/bin/sh")
        .args(["-c", r#"kill -"$0" "$1""#, signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} {pid}");
}

/// What browser-login.test.ts's `createUpstream` recorded of each request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub cookie: Option<String>,
    pub xsrf: Option<String>,
    pub browser_closed: bool,
    pub profile_removed: bool,
}

pub const USER_PATH: &str = "/api/UserData/GetLoggedInUser";

/// browser-login.test.ts's `user`.
pub fn user() -> Value {
    json!({
        "userId": 1,
        "studentId": "2",
        "schoolId": "3",
        "studentName": "Synthetic student",
        "name": "Synthetic guardian",
        "schoolLong": "Synthetic school",
        "defaultTermId": "4",
        "isGuardian": true,
        "logInType": "2",
        "olderThan18": false,
        "registerAbsenceGuardian": "1",
        "registerAbsenceUnder18": "0",
        "registerAbsenceOver18": "1",
        "registerAbsence": "1",
        "student18RegisterAbsence": "1",
        "registerLeave": "1",
        "student18RegisterLeave": "1",
        "registerIllnessTomorrow": "1",
    })
}

/// `createUpstream({valid})`: records each request with whether the browser had closed and its
/// profile was gone by then, and answers GetLoggedInUser (a 401 when not `valid`).
pub fn upstream(login: &Login, valid: bool) -> (Upstream, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (requests, state_file, exit_file) = (
        seen.clone(),
        login.state_file.clone(),
        login.exit_file.clone(),
    );
    let upstream = Upstream::new(move |request: &Request| {
        let path = url::Url::parse(&request.url).unwrap().path().to_owned();
        let state = parse_state(&fs::read_to_string(&state_file).unwrap()).unwrap();
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        requests.lock().unwrap().push(Seen {
            path: path.clone(),
            cookie: header("cookie"),
            xsrf: header("x-xsrf-token"),
            browser_closed: fs::read_to_string(&exit_file).is_ok_and(|text| text == "closed"),
            profile_removed: !state.profile.exists(),
        });

        if path != USER_PATH {
            return Reply::empty(404);
        }

        match valid {
            true => Reply::json(200, &user()),
            false => Reply::empty(401),
        }
    });
    (upstream, seen)
}
