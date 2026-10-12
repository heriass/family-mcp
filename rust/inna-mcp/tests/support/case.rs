//! What tests/ts/integration.test.ts's `fixture` and the rust-inna.ts drop-ins gave each case: a
//! scratch home holding an imported session (`INNA_SESSION_FILE` beside the store), the case's
//! provider, and clients. As the drop-in `InnaClient`, each client is one `inna-mcp serve` with its
//! own loopback upstream, which answers as the shared provider or as a case's wrapper around it, and
//! the keep-alive ticks only on SIGUSR1 (INNA_TEST_KEEP_ALIVE).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Value, json};

use super::provider::Provider;
use super::{Home, Mcp, PATIENCE, Reply, Request, Run, Upstream, cli};

/// integration.test.ts's `fixture` cookie export.
pub const EXPORT: &str = r#"[{"name":"SESSION","value":"synthetic-session","domain":"nam.inna.is","path":"/","secure":true},{"name":"XSRF-TOKEN","value":"synthetic-xsrf","domain":"nam.inna.is","path":"/","secure":true}]"#;

pub struct Case {
    pub home: Home,
    pub provider: Provider,
    pub upstream: Upstream,
    writes: bool,
}

impl Case {
    /// `fixture(allowAbsenceWrites)`: the provider, and its session imported.
    pub fn new(name: &str, writes: bool) -> Self {
        let case = Self::empty(name, writes);
        case.home.write("cookies.json", EXPORT, 0o600);
        let source = case.home.path("cookies.json").display().to_string();
        let run = case.cli(&["auth", "import", &source]);
        assert_eq!(run.code, Some(0), "import: {run:?}");
        case
    }

    /// The provider and the home, with nothing imported.
    pub fn empty(name: &str, writes: bool) -> Self {
        let provider = Provider::default();
        let upstream = provider.upstream();
        Self {
            home: Home::new(name),
            provider,
            upstream,
            writes,
        }
    }

    /// The drop-in harness's environment besides the home's own.
    pub fn env(&self) -> Vec<(String, String)> {
        self.env_at(&self.home.path("session.json"))
    }

    /// The environment with another session path (`InnaClient({sessionFile})`).
    pub fn env_at(&self, session: &Path) -> Vec<(String, String)> {
        vec![
            ("INNA_SESSION_FILE".into(), session.display().to_string()),
            ("INNA_TEST_KEEP_ALIVE".into(), "1".into()),
        ]
    }

    /// One `inna-mcp` command line against the case's upstream.
    pub fn cli(&self, args: &[&str]) -> Run {
        cli(&self.home, &self.upstream.origin, &self.env(), args, None)
    }

    /// One command line with `env` in place of the case's own.
    pub fn cli_in(&self, env: &[(String, String)], args: &[&str]) -> Run {
        cli(&self.home, &self.upstream.origin, env, args, None)
    }

    /// The case's environment with its store moved under `root` (`storeAt(root)`).
    pub fn env_store(&self, root: &Path) -> Vec<(String, String)> {
        let mut env = self.env();
        env.extend(Home::store_env(root));
        env
    }

    /// One command line with another session path.
    pub fn cli_at(&self, session: &Path, args: &[&str]) -> Run {
        cli(
            &self.home,
            &self.upstream.origin,
            &self.env_at(session),
            args,
            None,
        )
    }

    /// `importSession(source, allowAccountChange)`: the import's stdout, or its stderr line.
    pub fn import(&self, allow_account_change: bool) -> Result<String, String> {
        let source = self.home.path("cookies.json").display().to_string();
        let mut args = vec!["auth", "import", source.as_str()];

        if allow_account_change {
            args.push("--allow-account-change");
        }
        let run = self.cli(&args);
        match run.code {
            Some(0) => Ok(run.stdout),
            _ => Err(run.stderr.trim_end().to_owned()),
        }
    }

    /// A client with another session path.
    pub fn client_at(&self, session: &Path) -> Client {
        let (mcp, _) = Mcp::start(
            &self.home,
            &self.upstream.origin,
            &self.env_at(session),
            &[],
        );
        Client {
            mcp,
            upstream: None,
        }
    }

    /// The legacy session path (`f.path`).
    pub fn session(&self) -> PathBuf {
        self.home.path("session.json")
    }

    /// The encrypted record (`f.store.path`).
    pub fn store(&self) -> PathBuf {
        self.home.path(".config/inna-mcp/session.enc")
    }

    /// The store's key file (`f.store.key`).
    pub fn key(&self) -> PathBuf {
        self.home
            .path(".local/share/family-mcp/keys/inna-mcp.default.key")
    }

    /// integration.test.ts's `storeFiles`: the bytes of everything a store operation could
    /// change: the record, its marker, the key, the legacy file and the absence record.
    pub fn store_files(&self) -> Vec<Option<Vec<u8>>> {
        let marker = PathBuf::from(format!("{}.marker", self.store().display()));
        let absence = PathBuf::from(format!("{}.absence.json", self.session().display()));
        [self.store(), marker, self.key(), self.session(), absence]
            .iter()
            .map(|path| std::fs::read(path).ok())
            .collect()
    }

    /// integration.test.ts's `plaintextCookies`: every file under the home holding a cookie
    /// value, besides the owner's own export.
    pub fn plaintext_cookies(&self) -> Vec<PathBuf> {
        const VALUES: [&str; 5] = [
            "synthetic-session",
            "synthetic-xsrf",
            "synthetic-rotated",
            "synthetic-kept",
            "synthetic-planted",
        ];
        fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
                let path = entry.path();

                if path.is_dir() {
                    walk(&path, found);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    let text = String::from_utf8_lossy(&bytes);

                    if VALUES.iter().any(|value| text.contains(value)) {
                        found.push(path);
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.home.0, &mut found);
        found.retain(|path| *path != self.home.path("cookies.json"));
        found
    }

    /// A client on the case's own upstream.
    pub fn client(&self) -> Client {
        self.client_on(None)
    }

    /// A client whose upstream answers with `handler` (a case's fetch around the provider).
    pub fn client_with(
        &self,
        handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> Client {
        self.client_on(Some(Upstream::new(handler)))
    }

    /// A client without `--allow-absence-writes`, whatever the case allows.
    pub fn read_only_client(&self) -> Client {
        let (mcp, _) = Mcp::start(&self.home, &self.upstream.origin, &self.env(), &[]);
        Client {
            mcp,
            upstream: None,
        }
    }

    fn client_on(&self, upstream: Option<Upstream>) -> Client {
        self.client_in(self.env(), upstream)
    }

    /// A client with `env` in place of the case's own, on `upstream` or the case's.
    pub fn client_in(&self, env: Vec<(String, String)>, upstream: Option<Upstream>) -> Client {
        let origin = upstream.as_ref().unwrap_or(&self.upstream).origin.clone();
        let args: &[&str] = match self.writes {
            true => &["--allow-absence-writes"],
            false => &[],
        };
        let (mcp, _) = Mcp::start(&self.home, &origin, &env, args);
        Client { mcp, upstream }
    }

    /// integration.test.ts's `savedFile`: the stored session record.
    pub fn saved(&self) -> Value {
        serde_json::from_str(&self.home.record().expect("a stored record")).unwrap()
    }

    /// The text of the absence record beside the session file, if any.
    pub fn journal(&self) -> Option<String> {
        std::fs::read_to_string(self.home.path("session.json.absence.json")).ok()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.provider.finish("the case's provider");
        }
    }
}

/// One `serve`, as a drop-in `InnaClient`.
pub struct Client {
    pub mcp: Mcp,
    upstream: Option<Upstream>,
}

impl Client {
    /// A tool's structured content, or its error text (a tool error or a protocol error).
    pub fn call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let answer = self.mcp.call(name, arguments);
        let value = answer.value();

        if let Some(error) = value.get("error") {
            return Err(error["message"].as_str().unwrap_or_default().to_owned());
        }
        answer.outcome()
    }

    /// The client's own upstream (`Case::client_with`).
    pub fn upstream(&self) -> &Upstream {
        self.upstream
            .as_ref()
            .expect("a client with its own upstream")
    }

    /// `prepareAbsence(input)`.
    pub fn prepare(&mut self, input: Value) -> Result<Value, String> {
        self.call("inna_prepare_absence", input)
    }

    /// `submitAbsence(operationId, true)`.
    pub fn submit(&mut self, operation_id: &str) -> Result<Value, String> {
        self.call(
            "inna_submit_absence",
            json!({"operationId": operation_id, "confirm": true}),
        )
    }

    /// `keepAlive()`: one tick of the server's own keep-alive (SIGUSR1), and the status it
    /// reports next.
    pub fn keep_alive(&mut self) -> String {
        let before = self.mcp.statuses().len();
        self.mcp.signal("USR1");
        assert!(
            self.mcp.until_stderr("keep-alive: ", before + 1, PATIENCE),
            "no keep-alive status: {}",
            self.mcp.stderr()
        );
        self.mcp.statuses()[before].clone()
    }

    /// `absenceStatus()`.
    pub fn status(&mut self) -> Value {
        self.call("inna_absence_status", json!({})).unwrap()
    }
}

/// Fails unless `outcome` is an error whose text contains `part`.
#[track_caller]
pub fn refused(outcome: Result<Value, String>, part: &str) -> String {
    match outcome {
        Err(text) if text.contains(part) => text,
        other => panic!("expected a refusal with {part:?}, got {other:?}"),
    }
}

/// A request held at the upstream until released: `hold` runs in the upstream's handler.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<(bool, bool)>, Condvar)>);

impl Gate {
    /// In the handler: say the request has come, and wait for `release`.
    pub fn hold(&self) {
        let (lock, changed) = &*self.0;
        let mut state = lock.lock().unwrap();
        state.0 = true;
        changed.notify_all();

        while !state.1 {
            state = changed.wait(state).unwrap();
        }
    }

    /// Say it has come, without waiting: what `reached` waits for.
    pub fn arrive(&self) {
        let (lock, changed) = &*self.0;
        lock.lock().unwrap().0 = true;
        changed.notify_all();
    }

    /// Wait until the held request has come.
    pub fn reached(&self) {
        let (lock, changed) = &*self.0;
        let state = lock.lock().unwrap();
        let (state, timeout) = changed
            .wait_timeout_while(state, PATIENCE, |state| !state.0)
            .unwrap();
        drop(state);
        assert!(!timeout.timed_out(), "the held request never came");
    }

    pub fn release(&self) {
        let (lock, changed) = &*self.0;
        lock.lock().unwrap().1 = true;
        changed.notify_all();
    }
}
