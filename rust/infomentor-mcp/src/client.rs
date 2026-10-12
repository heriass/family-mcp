//! packages/infomentor-mcp/src/client.ts's `InfoMentorClient`: one queue of account operations per
//! MCP connection. Each read holds the store lock, proves the session on the parent page, and
//! writes changed cookies back before the next read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use family_store::{Cancel, KeyProvider};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::collection::{Source, collect_updates};
use crate::error::{Code, Fail, Result};
use crate::http::{Http, Parent};
use crate::input::{AssessmentItemRequest, AssessmentsRequest, CollectRequest, LoginRequest, MessagesRequest, NotificationsRequest};
use crate::js;
use crate::login::{
    self, create_authenticated_http, has_configured_credentials, resolve_credentials,
    session_from_http,
};
use crate::session::{SavedSession, session_path};
use crate::shapes::{self, Feed, MessagesPage};
use crate::signal::{Controller, Signal};
use crate::store::{self, Held, with_session};
use crate::upstream::Net;

const COLLECTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

const RENEWAL_TIMEOUT: Duration = Duration::from_secs(60);

const OVERVIEW_MAX_UNITS: usize = 40_000;

/// `SessionOptions`.
#[derive(Default, Clone)]
pub struct Options {
    /// `--session`, resolved.
    pub session_file: Option<PathBuf>,
    /// `--credentials`, resolved.
    pub credentials_file: Option<PathBuf>,
    /// The store's keys; `None` uses the default provider.
    pub keys: Option<Arc<dyn KeyProvider>>,
}

/// What a read sees besides the HTTP session.
pub struct Context<'a> {
    pub signal: &'a Signal,
    /// `signal`, as the store's cancel flag.
    pub cancel: &'a Cancel,
    /// How the session is stored, for the status tool.
    pub storage: &'static str,
}

/// The HTTP session built from the saved one, and that saved session's JSON.
struct Active {
    http: Http,
    serialized: String,
}

/// `SetupStatus`, its fields in the order the TypeScript client sets them.
#[derive(Clone)]
struct SetupStatus {
    operation: Option<&'static str>,
    state: &'static str,
    message: String,
}

impl SetupStatus {
    fn to_json(&self) -> Value {
        match self.operation {
            Some(operation) => {
                json!({"operation": operation, "state": self.state, "message": self.message})
            }
            None => json!({"state": self.state, "message": self.message}),
        }
    }
}

/// A running login or import: what cancels it, and what reports its end.
struct Setup {
    controller: Controller,
    done: watch::Receiver<bool>,
}

pub struct Client {
    options: Options,
    net: Net,
    /// The queue: one operation at a time, in call order, holding the cached session.
    active: Arc<Mutex<Option<Active>>>,
    lifetime: Controller,
    closed: AtomicBool,
    /// The running login or import. It runs outside the queue; reads refuse while it runs.
    setup: std::sync::Mutex<Option<Setup>>,
    status: std::sync::Mutex<SetupStatus>,
    logging_out: AtomicBool,
}

/// `saveActive`: persist the session when a request changed it, never for another account.
fn save_active(
    active: &mut Active,
    previous: &SavedSession,
    held: &mut Held,
) -> Result<SavedSession> {
    let mut current = session_from_http(&active.http)?;

    if let (Some(previous), Some(current)) = (&previous.account_id, &current.account_id)
        && previous != current
    {
        return Err(Fail::new(
            Code::InvalidSession,
            "Refusing to replace the verified account during a school-data request. Sign in explicitly to change accounts.",
        ));
    }

    if current.account_id.is_none() {
        current.account_id.clone_from(&previous.account_id);
    }

    if current.selected_child_id.is_none() {
        current
            .selected_child_id
            .clone_from(&previous.selected_child_id);
    }

    if current.comparable() == previous.comparable() {
        active.serialized = previous.to_json().to_string();
        return Ok(previous.clone());
    }
    held.save(&current)?;
    active.serialized = current.to_json().to_string();
    Ok(current)
}

/// One read's signal and store cancel flag.
#[derive(Clone, Copy)]
struct Step<'a> {
    signal: &'a Signal,
    cancel: &'a Cancel,
}

impl Step<'_> {
    /// Prove the session on the parent page, which the read then reuses, save it, and read.
    fn verified<T>(
        self,
        active: &mut Active,
        saved: &mut SavedSession,
        held: &mut Held,
        read: &mut impl FnMut(&mut Http, &Context) -> Result<T>,
    ) -> Result<T> {
        let parent = active.http.read_parent(self.signal, None)?;

        if saved
            .account_id
            .as_ref()
            .is_some_and(|account| *account != parent.account_id)
        {
            return Err(Fail::new(
                Code::InvalidSession,
                "The saved session no longer matches its verified account. Sign in explicitly before continuing.",
            ));
        }
        *saved = save_active(active, saved, held)?;
        self.run(active, held, read)
    }

    fn run<T>(
        self,
        active: &mut Active,
        held: &Held,
        read: &mut impl FnMut(&mut Http, &Context) -> Result<T>,
    ) -> Result<T> {
        let context = Context {
            signal: self.signal,
            cancel: self.cancel,
            storage: held.storage,
        };
        let output = read(&mut active.http, &context)?;
        self.signal.check()?;
        Ok(output)
    }
}

fn timetable(http: &mut Http, signal: &Signal) -> Result<Feed> {
    http.read_app_data(
        "timetable/timetable/appData",
        &[],
        signal,
        shapes::timetable_response,
    )
}

fn messages_page(
    http: &mut Http,
    signal: &Signal,
    folder: &str,
    page: i64,
    page_size: i64,
    search: &str,
) -> Result<MessagesPage> {
    http.read_app_data(
        "Message/message/GetMessages",
        &[
            ("page", page.to_string()),
            ("pageSize", page_size.to_string()),
            ("messageText", search.to_owned()),
            ("inbox", (folder == "inbox").to_string()),
            ("sentItems", (folder == "sent").to_string()),
        ],
        signal,
        shapes::messages_response,
    )
}

fn message(http: &mut Http, signal: &Signal, id: i64) -> Result<Value> {
    http.read_app_data(
        "Message/message/GetMessage",
        &[("id", id.to_string())],
        signal,
        shapes::message_detail,
    )
}

fn notifications(http: &mut Http, signal: &Signal) -> Result<Feed> {
    http.read_app_data(
        "NotificationApp/NotificationApp/appData",
        &[],
        signal,
        shapes::notifications_response,
    )
}

fn text<'v>(value: &'v Value, key: &str) -> &'v str {
    value[key].as_str().unwrap_or_default()
}

/// The overview of the selected child, after selecting `child_id` when given.
fn overview(http: &mut Http, signal: &Signal, child_id: Option<&str>) -> Result<Value> {
    let mut read = || -> Result<Value> {
        let parent = match (child_id, &http.parent) {
            (None, Some(parent)) => parent.clone(),
            _ => http.read_parent(signal, child_id)?,
        };
        let (timetable, skipped) = match parent.has_timetable() {
            true => {
                let feed = timetable(http, signal)?;
                (Some(feed.items), feed.skipped)
            }
            false => (None, 0),
        };
        let mut lines: Vec<String> = parent
            .pupils
            .iter()
            .map(|pupil| {
                format!(
                    "{}{}",
                    pupil.name,
                    if pupil.selected { " (selected)" } else { "" }
                )
            })
            .collect();

        for item in timetable.iter().flatten() {
            lines.push(format!(
                "{}: {} ({}–{})",
                text(item, "start"),
                text(item, "title"),
                text(item, "startTime"),
                text(item, "endTime")
            ));
        }
        let text = lines.join("\n");
        Ok(json!({
            "title": "InfoMentor parent overview",
            "text": js::slice_units(&text, OVERVIEW_MAX_UNITS),
            "truncated": js::units(&text) > OVERVIEW_MAX_UNITS,
            "children": parent
                .pupils
                .iter()
                .map(|pupil| json!({"id": pupil.id, "name": pupil.name, "selected": pupil.selected}))
                .collect::<Vec<_>>(),
            "timetable": timetable,
            "skipped": skipped,
            "retrievedAt": js::iso_string(js::now_ms()),
        }))
    };

    match (read(), child_id) {
        (Err(fail), Some(_)) => Err(fail.rewrap(
            "InfoMentor could not load the selected child. Selection may have changed; refresh infomentor_get_overview before continuing.",
        )),
        (outcome, _) => outcome,
    }
}

/// A collection's reads through the session; the parent page the read already verified is used
/// once, as the first parent read.
struct Reads<'h> {
    http: &'h mut Http,
    cached: Option<Parent>,
}

impl Source for Reads<'_> {
    fn get_parent(&mut self, signal: &Signal) -> Result<Parent> {
        match self.cached.take() {
            Some(parent) => Ok(parent),
            None => self.http.read_parent(signal, None),
        }
    }

    fn select_child(&mut self, child_id: &str, signal: &Signal) -> Result<Parent> {
        self.http.read_parent(signal, Some(child_id))
    }

    fn read_timetable(&mut self, parent: &Parent, signal: &Signal) -> Result<Option<Feed>> {
        match parent.has_timetable() {
            true => timetable(self.http, signal).map(Some),
            false => Ok(None),
        }
    }

    fn get_messages(&mut self, folder: &str, page: i64, signal: &Signal) -> Result<MessagesPage> {
        messages_page(self.http, signal, folder, page, 100, "")
    }

    fn get_message(&mut self, id: i64, signal: &Signal) -> Result<Value> {
        message(self.http, signal, id)
    }

    fn get_notifications(&mut self, signal: &Signal) -> Result<Feed> {
        notifications(self.http, signal)
    }
}

impl Client {
    /// `None` outside a Tokio runtime or when no HTTP client can be built.
    pub fn new(options: Options) -> Option<Self> {
        Some(Self {
            options,
            net: Net::new()?,
            active: Arc::default(),
            lifetime: Controller::default(),
            closed: AtomicBool::new(false),
            setup: std::sync::Mutex::default(),
            status: std::sync::Mutex::new(SetupStatus {
                operation: None,
                state: "idle",
                message: "No setup operation has started.".to_owned(),
            }),
            logging_out: AtomicBool::new(false),
        })
    }

    /// Queue `read` behind earlier operations and run it on a blocking thread with the session
    /// held: verified on the parent page first, and saved again afterwards when it changed.
    async fn read<T: Send + 'static>(
        self: &Arc<Self>,
        signal: Signal,
        read: impl FnMut(&mut Http, &Context) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let signal = signal.any(&self.lifetime.signal());
        // A local queue preserves call order; the store lock also excludes other MCP processes.
        let mut active = self.active.clone().lock_owned().await;
        let client = self.clone();

        tokio::task::spawn_blocking(move || client.held_read(&mut active, &signal, read))
            .await
            .unwrap_or(Err(Fail::Unknown))
    }

    fn held_read<T>(
        &self,
        active: &mut Option<Active>,
        signal: &Signal,
        mut read: impl FnMut(&mut Http, &Context) -> Result<T>,
    ) -> Result<T> {
        signal.check()?;

        if self.closed.load(Ordering::SeqCst) {
            return Err(Fail::new(
                Code::Cancelled,
                "This InfoMentor client has been closed.",
            ));
        }

        if self.setting_up() || self.logging_out.load(Ordering::SeqCst) {
            return Err(Fail::new(
                Code::OperationInProgress,
                "Account setup is in progress. Check infomentor_setup_status before reading school data.",
            ));
        }
        let file = session_path(self.options.session_file.as_deref())?;
        let bridge = signal.store_cancel(&self.net.handle);

        with_session(&file, &bridge.cancel, self.options.keys.clone(), |held| {
            let mut saved = held.session.clone();
            let serialized = saved.to_json().to_string();

            if active
                .as_ref()
                .is_none_or(|active| active.serialized != serialized)
            {
                *active = None;
            }
            let current = active.get_or_insert_with(|| Active {
                http: Http::new(saved.jar(), saved.cooldown(), self.net.clone()),
                serialized,
            });
            let step = Step {
                signal,
                cancel: &bridge.cancel,
            };

            match step.verified(current, &mut saved, held, &mut read) {
                Err(fail) if fail.is(Code::LoginRequired) => {
                    self.renew(active, saved, held, step, &mut read, fail)
                }
                outcome => {
                    if !signal.aborted() {
                        save_active(current, &saved, held)?;
                    }
                    outcome
                }
            }
        })
    }

    /// The renewal after a confirmed authentication expiry: one submission of the stored sign-in,
    /// or else the configured one, for the same account, then the read replayed once. Until the
    /// renewed session is saved, nothing replaces the expired one, so a failure keeps it.
    fn renew<T>(
        &self,
        active: &mut Option<Active>,
        mut saved: SavedSession,
        held: &mut Held,
        step: Step,
        read: &mut impl FnMut(&mut Http, &Context) -> Result<T>,
        expired: Fail,
    ) -> Result<T> {
        if held.credentials.is_none() && !has_configured_credentials(&self.options) {
            return Err(expired);
        }
        let verified = active
            .as_ref()
            .and_then(|active| active.http.parent.as_ref())
            .map(|parent| parent.account_id.clone());
        let Some(account_id) = saved.account_id.clone().or(verified) else {
            return Err(Fail::new(
                Code::LoginRequired,
                "This older session expired before its account could be verified. Call infomentor_login once to enable automatic authentication refresh.",
            ));
        };
        // The stored sign-in comes first; one submission, never a second source after a rejection.
        let (credentials, _) =
            resolve_credentials(&self.options, step.signal, held.credentials.as_ref())?;
        let mut candidate = create_authenticated_http(
            &self.net,
            step.signal,
            credentials,
            &Signal::timeout(RENEWAL_TIMEOUT),
        )?;

        if candidate.parent.as_ref().map(|parent| &parent.account_id) != Some(&account_id) {
            return Err(Fail::new(
                Code::LoginRequired,
                "The stored or configured credentials belong to a different InfoMentor account. The previous session was kept. Correct the private credentials or explicitly sign in to change accounts.",
            ));
        }

        if let Some(child) = &saved.selected_child_id {
            candidate.read_parent(step.signal, Some(child))?;
        }
        let mut renewed = Active {
            http: candidate,
            serialized: String::new(),
        };
        saved = save_active(&mut renewed, &saved, held)?;
        let current = active.insert(renewed);
        // Only confirmed authentication expiry replays a read, once. Other failures propagate.
        let outcome = step.run(current, held, read);

        if !step.signal.aborted() {
            save_active(current, &saved, held)?;
        }
        outcome
    }

    pub async fn overview(self: &Arc<Self>, signal: Signal) -> Result<Value> {
        self.read(signal, |http, context| overview(http, context.signal, None))
            .await
    }

    pub async fn assessments(self: &Arc<Self>, input: AssessmentsRequest, signal: Signal) -> Result<Value> {
        self.read(signal, move |http, context| crate::assessment::overview(http, context.signal, &input)).await
    }

    pub async fn assessment_item(self: &Arc<Self>, input: AssessmentItemRequest, signal: Signal) -> Result<Value> {
        self.read(signal, move |http, context| crate::assessment::item(http, context.signal, &input)).await
    }

    pub async fn select_child(self: &Arc<Self>, child_id: String, signal: Signal) -> Result<Value> {
        self.read(signal, move |http, context| {
            overview(http, context.signal, Some(&child_id))
        })
        .await
    }

    pub async fn messages(
        self: &Arc<Self>,
        input: MessagesRequest,
        signal: Signal,
    ) -> Result<Value> {
        self.read(signal, move |http, context| {
            let page = messages_page(
                http,
                context.signal,
                input.folder,
                input.page,
                input.page_size,
                &input.search,
            )?;
            // The live endpoint reports page: 0 even when it correctly applies a requested page.
            Ok(json!({
                "items": page.items,
                "skipped": page.skipped,
                "more": page.more,
                "page": input.page,
                "pageSize": input.page_size,
                "folder": input.folder,
                "retrievedAt": js::iso_string(js::now_ms()),
            }))
        })
        .await
    }

    pub async fn message(self: &Arc<Self>, id: i64, signal: Signal) -> Result<Value> {
        self.read(signal, move |http, context| {
            Ok(json!({
                "message": message(http, context.signal, id)?,
                "retrievedAt": js::iso_string(js::now_ms()),
            }))
        })
        .await
    }

    pub async fn notifications(
        self: &Arc<Self>,
        input: NotificationsRequest,
        signal: Signal,
    ) -> Result<Value> {
        self.read(signal, move |http, context| {
            let feed = notifications(http, context.signal)?;
            let shown: Vec<Value> = feed
                .items
                .into_iter()
                .filter(|item| {
                    (input.include_cleared || item["state"] != "Cleared")
                        && (!input.selected_child_only
                            || item["currentlySelectedPupil"] == Value::Bool(true))
                })
                .collect();
            Ok(json!({
                "notifications": shown,
                "skipped": feed.skipped,
                "selectedChildOnly": input.selected_child_only,
                "includeCleared": input.include_cleared,
                "retrievedAt": js::iso_string(js::now_ms()),
            }))
        })
        .await
    }

    pub async fn collect(self: &Arc<Self>, input: CollectRequest, signal: Signal) -> Result<Value> {
        let signal = signal.any(&Signal::timeout(COLLECTION_TIMEOUT));
        let session_file = self.options.session_file.clone();

        self.read(signal, move |http, context| {
            let file = session_path(session_file.as_deref())?;
            let cached = http.parent.clone();
            let mut reads = Reads { http, cached };
            collect_updates(&input, &file, context.signal, context.cancel, &mut reads)
        })
        .await
    }

    /// `getSessionStatus`: an expired or missing session is a status, not a failure.
    pub async fn session_status(self: &Arc<Self>, signal: Signal) -> Result<Value> {
        let status = self
            .read(signal, |_, context| {
                Ok(json!({"authenticated": true, "storage": context.storage}))
            })
            .await;

        match status {
            Err(Fail::Im {
                code: Code::LoginRequired,
                message,
                ..
            }) => Ok(json!({"authenticated": false, "nextStep": message})),
            other => other,
        }
    }

    fn setting_up(&self) -> bool {
        self.setup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn set_status(&self, status: SetupStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// `getSetupStatus`.
    pub fn setup_status(&self) -> Value {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .to_json()
    }

    /// `startLogin`: start a login or import and return at once; its result becomes the setup
    /// status.
    pub fn start_login(self: &Arc<Self>, request: LoginRequest) -> Result<Value> {
        if request.import_file.is_some() && request.credentials_file.is_some() {
            return Err(Fail::config("Choose session import or login, not both."));
        }

        if self.closed.load(Ordering::SeqCst) {
            return Err(Fail::new(
                Code::Cancelled,
                "This InfoMentor client has been closed.",
            ));
        }
        let mut setup = self.setup.lock().unwrap_or_else(PoisonError::into_inner);

        if setup.is_some() || self.logging_out.load(Ordering::SeqCst) {
            return Err(Fail::new(
                Code::OperationInProgress,
                "Another setup operation is active. Check its status or cancel it first.",
            ));
        }
        let operation = match request.import_file {
            Some(_) => "import",
            None => "login",
        };
        self.set_status(SetupStatus {
            operation: Some(operation),
            state: "running",
            message: "Setup started. Check infomentor_setup_status for progress.".to_owned(),
        });
        let controller = Controller::default();
        let signal = controller.signal();
        let (finished, done) = watch::channel(false);
        let client = self.clone();

        tokio::spawn(async move {
            let outcome = client.run_setup(request, &signal).await;
            let (state, message) = match outcome {
                Ok(advice) => (
                    "succeeded",
                    std::iter::once(
                        "Session saved in the encrypted store. Call infomentor_session_status to verify access."
                            .to_owned(),
                    )
                    .chain(advice.map(|file| store::delete_credentials_advice(&file)))
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
                Err(_) if signal.aborted() => (
                    "cancelled",
                    "Setup cancelled. The previously saved session was kept.".to_owned(),
                ),
                Err(Fail::Im { message, .. }) => ("failed", message.to_owned()),
                Err(_) => (
                    "failed",
                    "Setup failed. Check the network and session-file permissions.".to_owned(),
                ),
            };
            client.set_status(SetupStatus {
                operation: Some(operation),
                state,
                message,
            });
            *client.setup.lock().unwrap_or_else(PoisonError::into_inner) = None;
            finished.send_replace(true);
        });
        *setup = Some(Setup { controller, done });
        drop(setup);
        Ok(self.setup_status())
    }

    /// The setup itself, after the operations queued before it. Returns the credentials file to
    /// advise deleting: only one this call named, never the server's configured path.
    async fn run_setup(
        self: &Arc<Self>,
        request: LoginRequest,
        signal: &Signal,
    ) -> Result<Option<String>> {
        {
            let mut active = self.active.lock().await;
            signal.check()?;
            *active = None;
        }
        let client = self.clone();
        let signal = signal.clone();

        tokio::task::spawn_blocking(move || {
            let allow = request.allow_account_change.unwrap_or(false);

            if let Some(file) = &request.import_file {
                login::import_session(
                    &client.net,
                    &client.options,
                    Path::new(file),
                    &signal,
                    allow,
                )?;
                return Ok(None);
            }
            let named = request.credentials_file.is_some();
            let options = Options {
                credentials_file: request
                    .credentials_file
                    .map(PathBuf::from)
                    .or_else(|| client.options.credentials_file.clone()),
                ..client.options.clone()
            };
            let file = login::login(
                &client.net,
                &options,
                &signal,
                allow,
                request.timeout_seconds as f64 * 1000.0,
            )?;
            Ok(file.filter(|_| named))
        })
        .await
        .unwrap_or(Err(Fail::Unknown))
    }

    /// `cancelSetup`: abort the running setup, wait for it to end, and report the status.
    pub async fn cancel_setup(&self) -> Value {
        let done = self
            .setup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|setup| {
                setup.controller.abort();
                setup.done.clone()
            });

        if let Some(mut done) = done {
            // A dropped sender means the task ended without reporting; it has ended either way.
            let _ = done.wait_for(|finished| *finished).await;
        }
        self.setup_status()
    }

    /// `logout`: cancel any setup, wait for queued operations, then remove the local session, the
    /// stored sign-in and the collection cursors.
    pub async fn logout(&self) -> Result<()> {
        if self.logging_out.swap(true, Ordering::SeqCst) {
            return Err(Fail::new(
                Code::OperationInProgress,
                "Logout is already in progress.",
            ));
        }
        let outcome = async {
            self.cancel_setup().await;
            *self.active.lock().await = None;
            let file = session_path(self.options.session_file.as_deref())?;
            let keys = self.options.keys.clone();
            tokio::task::spawn_blocking(move || store::logout(&file, keys))
                .await
                .unwrap_or(Err(Fail::Unknown))?;
            self.set_status(SetupStatus {
                operation: None,
                state: "idle",
                message: "Local session and stored sign-in removed. Call infomentor_login to sign in again."
                    .to_owned(),
            });
            Ok(())
        }
        .await;
        self.logging_out.store(false, Ordering::SeqCst);
        outcome
    }

    /// Stop every operation: queued and running reads, and a running setup, end with a
    /// cancellation.
    pub fn abort(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.lifetime.abort();

        if let Some(setup) = self
            .setup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            setup.controller.abort();
        }
    }

    /// `close`: abort, then wait for the setup and the running operation to end.
    pub async fn close(&self) {
        self.abort();
        self.cancel_setup().await;
        *self.active.lock().await = None;
    }
}
