//! Abler's API, with packages/abler-mcp/src/api.ts's requests, rotation, refresh, bounds,
//! upstream shapes and messages. A client operation holds the session lock throughout, so it runs
//! on a blocking thread and waits for each request with `Handle::block_on`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use family_store::{Cancel, KeyProvider};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HeaderValue, SET_COOKIE};
use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use tokio::sync::{RwLock, watch};

use crate::auth::{Session, Slot, with_session};
use crate::error::{Fail, Result};
use crate::input::{ChildSchedules, Event, Messages, Page, Schedule};
use crate::jar::{AUTH_COOKIES, parse_set_cookie};
use crate::js;

pub const ORIGIN: &str = "https://www.abler.io";

const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(20);
/// Response headers Bun's fetch accepts (measured on 1.4.2); hyper's default is 100.
const MAX_HEADERS: usize = 256;

const REQUEST_FAILED: &str =
    "Abler request failed or timed out. Check the connection and try again.";

const NOT_ADVANCED: &str =
    "Abler pagination did not advance. Retry later; do not report this schedule as complete.";

const EVENT_FIELDS: &str = "
  eventId name type description from to status arrivalTime
  locationDetails locationAddress locationLink
  ageGroup { id name }
  groups { id name }
  currentPlayerAttendance { status coachStatus player { id displayName } }
";

const MESSAGE_FIELDS: &str = "
  id messageBody createdAt
  creator { id displayName }
  attachments { id fileName description contentType }
  recipient { isRead }
";

const PAGE_FIELDS: &str = "pageInfo { hasNextPage endCursor }";

/// The upstream shapes, as zod objects parse them: unknown keys dropped, known keys in shape order.
#[derive(Clone, Copy)]
enum S {
    Str,
    /// A string of 1 to 256 code points.
    Id,
    Num,
    Bool,
    StrOrNum,
    Opt(&'static S),
    Null(&'static S),
    Nullish(&'static S),
    List(&'static S),
    Obj(&'static [(&'static str, S)]),
}

const PERSON: S = S::Obj(&[("id", S::Id), ("displayName", S::Str)]);

const NAMED: S = S::Obj(&[("id", S::Id), ("name", S::Str)]);

const ATTENDANCE: S = S::List(&S::Obj(&[
    ("status", S::Null(&S::Str)),
    ("coachStatus", S::Null(&S::Str)),
    ("player", PERSON),
]));

const EVENT: S = S::Obj(&[
    ("eventId", S::Id),
    ("name", S::Str),
    ("type", S::Opt(&S::Str)),
    ("description", S::Nullish(&S::Str)),
    ("from", S::Str),
    ("to", S::Null(&S::Str)),
    ("status", S::Opt(&S::Str)),
    // Abler sends this as a number (minutes before `from`); keep strings for older shapes.
    ("arrivalTime", S::Nullish(&S::StrOrNum)),
    ("locationDetails", S::Nullish(&S::Str)),
    ("locationAddress", S::Nullish(&S::Str)),
    ("locationLink", S::Nullish(&S::Str)),
    ("ageGroup", NAMED),
    ("groups", S::Opt(&S::List(&NAMED))),
    ("currentPlayerAttendance", ATTENDANCE),
]);

const PROFILE: S = S::Obj(&[
    ("id", S::Id),
    ("displayName", S::Str),
    ("children", S::List(&PERSON)),
]);

const GROUPS: S = S::Obj(&[(
    "userAgeGroups",
    S::List(&S::Obj(&[
        ("id", S::Id),
        ("name", S::Str),
        ("isActive", S::Opt(&S::Bool)),
        (
            "groups",
            S::Opt(&S::List(&S::Obj(&[
                ("id", S::Id),
                ("name", S::Str),
                ("label", S::Nullish(&S::Str)),
            ]))),
        ),
        ("sport", S::Nullish(&NAMED)),
    ])),
)]);

// Upstream shapes accept null or missing fields more widely than observed.
const MESSAGE: S = S::Obj(&[
    ("id", S::Id),
    ("messageBody", S::Nullish(&S::Str)),
    ("createdAt", S::Str),
    ("creator", S::Nullish(&PERSON)),
    (
        "attachments",
        S::Nullish(&S::List(&S::Obj(&[
            ("id", S::Id),
            ("fileName", S::Str),
            ("description", S::Nullish(&S::Str)),
            ("contentType", S::Str),
        ]))),
    ),
    (
        "recipient",
        S::Nullish(&S::Obj(&[("isRead", S::Nullish(&S::Bool))])),
    ),
]);

const CONVERSATION: S = S::Obj(&[
    ("id", S::Id),
    ("name", S::Nullish(&S::Str)),
    ("conversationType", S::Str),
    ("membersCount", S::Nullish(&S::Num)),
    ("unreadCount", S::Num),
    ("messageGroup", S::Nullish(&NAMED)),
    ("user1", S::Nullish(&PERSON)),
    ("user2", S::Nullish(&PERSON)),
    (
        "messages",
        S::Nullish(&S::Obj(&[(
            "edges",
            S::List(&S::Obj(&[("node", MESSAGE)])),
        )])),
    ),
]);

/// `schema.parse(value)`; `None` input is `undefined`, and stays absent.
fn shape(schema: &S, value: Option<&Value>) -> Result<Option<Value>> {
    let parsed = match (schema, value) {
        (S::Opt(_) | S::Nullish(_), None) => return Ok(None),
        (S::Null(_) | S::Nullish(_), Some(Value::Null)) => Value::Null,
        (S::Opt(inner) | S::Null(inner) | S::Nullish(inner), value) => return shape(inner, value),
        (S::Str | S::StrOrNum, Some(Value::String(text))) => Value::String(text.clone()),
        (S::Id, Some(Value::String(text))) if (1..=256).contains(&js::length(text)) => {
            Value::String(text.clone())
        }
        (S::Num | S::StrOrNum, Some(number @ Value::Number(_))) => number.clone(),
        (S::Bool, Some(flag @ Value::Bool(_))) => flag.clone(),
        (S::List(item), Some(Value::Array(items))) => Value::Array(
            items
                .iter()
                .map(|value| shape(item, Some(value))?.ok_or(Fail::Invalid))
                .collect::<Result<_>>()?,
        ),
        (S::Obj(fields), Some(Value::Object(object))) => {
            let mut parsed = Map::new();

            for (key, field) in *fields {
                if let Some(value) = shape(field, object.get(*key))? {
                    parsed.insert((*key).to_owned(), value);
                }
            }
            Value::Object(parsed)
        }
        _ => return Err(Fail::Invalid),
    };
    Ok(Some(parsed))
}

fn parse(schema: &S, value: Option<&Value>) -> Result<Value> {
    shape(schema, value)?.ok_or(Fail::Invalid)
}

/// `pageOf(node)`: edges of nodes, and a cursor whenever another page follows.
struct UpstreamPage {
    nodes: Vec<Value>,
    page_info: Value,
    end_cursor: Option<String>,
}

const PAGE_INFO: S = S::Obj(&[("hasNextPage", S::Bool), ("endCursor", S::Null(&S::Str))]);

fn page_of(node: &S, value: Option<&Value>) -> Result<UpstreamPage> {
    let Some(Value::Object(page)) = value else {
        return Err(Fail::Invalid);
    };
    let Some(Value::Array(edges)) = page.get("edges") else {
        return Err(Fail::Invalid);
    };
    let nodes = edges
        .iter()
        .map(|edge| match edge {
            Value::Object(edge) => parse(node, edge.get("node")),
            _ => Err(Fail::Invalid),
        })
        .collect::<Result<Vec<_>>>()?;
    let page_info = parse(&PAGE_INFO, page.get("pageInfo"))?;
    let end_cursor = page_info["endCursor"].as_str().map(str::to_owned);
    let more = page_info["hasNextPage"] == true;

    if more && (nodes.is_empty() || end_cursor.as_deref().is_none_or(str::is_empty)) {
        // 'Abler returned an incomplete pagination cursor.' is a ZodError.
        return Err(Fail::Invalid);
    }
    Ok(UpstreamPage {
        nodes,
        page_info,
        end_cursor,
    })
}

impl UpstreamPage {
    fn advanced(&self, after: Option<&str>, message: &'static str) -> Result<()> {
        if self.page_info["hasNextPage"] == true
            && self.end_cursor.is_some()
            && self.end_cursor.as_deref() == after
        {
            return Err(Fail::Safe(message));
        }
        Ok(())
    }
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `graphqlResponseSchema`: its data record, its error count, and whether one is UNAUTHENTICATED.
struct Graphql {
    data: Option<Map<String, Value>>,
    errors: usize,
    unauthenticated: bool,
    messages: Vec<String>,
    redactions: Vec<String>,
}

fn graphql(value: &Value) -> Option<Graphql> {
    let object = value.as_object()?;
    let data = match object.get("data") {
        None | Some(Value::Null) => None,
        Some(Value::Object(data)) => {
            let mut data = data.clone();
            // z.record never copies `__proto__`.
            data.shift_remove("__proto__");
            Some(data)
        }
        Some(_) => return None,
    };
    let mut codes = Vec::new();
    let mut messages = Vec::new();
    let errors = match object.get("errors") {
        None | Some(Value::Null) => 0,
        Some(Value::Array(errors)) => {
            for error in errors {
                if let Some(message) = error.get("message").and_then(Value::as_str) { messages.push(message.to_owned()); }
                match error {
                    Value::Null => {}
                    Value::Object(error) => match error.get("extensions") {
                        None | Some(Value::Null) => {}
                        Some(Value::Object(extensions)) => match extensions.get("code") {
                            None => {}
                            Some(Value::String(code)) => codes.push(code.as_str()),
                            Some(_) => return None,
                        },
                        Some(_) => return None,
                    },
                    _ => return None,
                }
            }
            errors.len()
        }
        Some(_) => return None,
    };
    let unauthenticated = codes.contains(&"UNAUTHENTICATED");
    Some(Graphql {
        data,
        errors,
        unauthenticated,
        messages,
        redactions: Vec::new(),
    })
}

/// The upstream origin. Only a `test-origin` build reads ABLER_TEST_ORIGIN; cookies always
/// belong to Abler's HTTPS host.
fn origin() -> String {
    #[cfg(feature = "test-origin")]
    if let Some(origin) = std::env::var("ABLER_TEST_ORIGIN")
        .ok()
        .filter(|o| !o.is_empty())
    {
        return origin;
    }
    ORIGIN.to_owned()
}

/// Whether `auth_status` and `auth status` force a refresh, as `AblerClient.status(true)` does;
/// only verification forces one otherwise. A `test-origin` seam for the TypeScript suite.
pub fn status_forces_refresh() -> bool {
    #[cfg(feature = "test-origin")]
    return std::env::var_os("ABLER_TEST_FORCE_REFRESH").is_some();
    #[cfg(not(feature = "test-origin"))]
    false
}

/// hyper could not read the response head (over its buffer of about 408 KiB, or more headers than
/// `MAX_HEADERS`), so its cookies, perhaps a rotation, are unknown.
fn head_too_large(error: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(error);

    while let Some(error) = source {
        // `is_parse_too_large` needs hyper's server feature; this is its kind's fixed text.
        if error.downcast_ref::<hyper::Error>().is_some_and(|error| {
            error.is_parse() && error.to_string() == "message head is too large"
        }) {
            return true;
        }
        source = error.source();
    }
    false
}

/// A `Cookie` header value, as fetch sends a string: Latin-1 bytes, or nothing beyond them.
fn cookie_header(text: &str) -> Option<HeaderValue> {
    let bytes: Option<Vec<u8>> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect();
    HeaderValue::from_bytes(&bytes?).ok()
}

pub struct Client {
    path: PathBuf,
    /// `Candidate` verifies a new session before it is promoted; its rotations stay there.
    slot: Slot,
    keys: Option<Arc<dyn KeyProvider>>,
    http: reqwest::Client,
    origin: String,
    handle: Handle,
    cancel: Cancel,
    stop: watch::Sender<bool>,
    active: Arc<RwLock<()>>,
}

impl Client {
    pub fn new(path: PathBuf, slot: Slot, keys: Option<Arc<dyn KeyProvider>>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .http1_max_headers(MAX_HEADERS)
            .build()
            .map_err(|_| Fail::Unknown)?;

        Ok(Self {
            path,
            slot,
            keys,
            http,
            origin: origin(),
            handle: Handle::current(),
            cancel: Cancel::default(),
            stop: watch::channel(false).0,
            active: Arc::new(RwLock::new(())),
        })
    }

    /// Cancel requests and lock waits in flight, and every later one.
    pub fn abort(&self) {
        self.cancel.cancel();
        self.stop.send_replace(true);
    }

    /// Abort, then wait for every operation to end.
    pub async fn close(&self) {
        self.abort();
        let _ = self.active.write().await;
    }

    /// Run a client operation on a blocking thread; `close` waits for it.
    pub async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Client) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let active = self.active.clone().read_owned().await;
        let client = self.clone();

        let outcome = tokio::task::spawn_blocking(move || {
            let _active = active;
            operation(&client)
        })
        .await;
        outcome.unwrap_or(Err(Fail::Unknown))
    }

    /// Run `work` with the session held. Blocks.
    fn session<T>(&self, work: impl FnOnce(&Client, &mut Session) -> Result<T>) -> Result<T> {
        with_session(
            &self.path,
            self.slot,
            &self.cancel,
            self.keys.clone(),
            |session| work(self, session),
        )
    }

    /// Wait for `future` from the blocking thread; `None` once the client closes.
    fn wait<F: Future>(&self, future: F) -> Option<F::Output> {
        let mut stop = self.stop.subscribe();

        self.handle.block_on(async {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => None,
                output = future => Some(output),
            }
        })
    }

    fn post(
        &self,
        session: &mut Session,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let cookie = cookie_header(&session.jar.header(path)).ok_or(Fail::Safe(REQUEST_FAILED))?;
        let body = body.map_or_else(String::new, Value::to_string);
        let request = self
            .http
            .post(format!("{}{path}", self.origin))
            .timeout(TIMEOUT)
            .header(COOKIE, cookie)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            // fetch sends `Content-Length: 0` for a POST without a body too.
            .header(CONTENT_LENGTH, body.len())
            .body(body);
        let sent = self.wait(request.send());

        // Bun's fetch reads a head of 256 headers and more than 1 MiB. reqwest exposes only the
        // header count, so hyper still refuses a head over about 408 KiB. Any head it refuses,
        // rotation or not, ends the session here, where fetch would have read on.
        if let Some(Err(error)) = &sent
            && head_too_large(error)
            && let Some(fail) = session.lose()
        {
            return Err(fail);
        }
        let response = sent
            .and_then(std::result::Result::ok)
            // fetch's `redirect: 'error'` fails on any redirect status.
            .filter(|response| ![301, 302, 303, 307, 308].contains(&response.status().as_u16()))
            .ok_or(Fail::Safe(REQUEST_FAILED))?;
        let mut changed = false;

        for header in response.headers().get_all(SET_COOKIE) {
            let text: String = header
                .as_bytes()
                .iter()
                .map(|&byte| char::from(byte))
                .collect();
            let Some(cookie) = parse_set_cookie(&text) else {
                continue;
            };

            if !AUTH_COOKIES.contains(&cookie.key.as_str()) {
                continue;
            }
            // Cookie-library errors can include untrusted response headers.
            session
                .jar
                .set(cookie, path)
                .map_err(|_| Fail::Safe("Abler returned an invalid authentication cookie."))?;
            changed = true;
        }

        // Persist rotation before another request, including when Abler returns an error.
        if changed {
            session.save()?;
        }
        Ok(response)
    }

    /// The parsed body, or null when it is unreadable, too large or not JSON.
    fn read_json(&self, response: reqwest::Response) -> Value {
        self.wait(mcp_runtime::read_capped(response, MAX_RESPONSE_BODY_BYTES))
            .and_then(std::result::Result::ok)
            .and_then(|body| js::parse(&body))
            .unwrap_or(Value::Null)
    }

    fn refresh(&self, session: &mut Session) -> Result<()> {
        let response = self.post(session, "/oauth/token", None)?;
        let status = response.status();

        if [401, 403].contains(&status.as_u16()) {
            return Err(Fail::Expired(
                "Abler session expired or was revoked. Sign in again and capture/import it.",
            ));
        }

        if !status.is_success() {
            return Err(Fail::Safe(
                "Abler session refresh failed. Check your session and try again.",
            ));
        }
        let result = self.read_json(response);
        let valid = result.as_object().is_some_and(|result| {
            result
                .get("access_token")
                .and_then(Value::as_str)
                .is_some_and(|token| js::length(token) >= 1)
                && !result.get("error").is_some_and(truthy)
        });

        if !valid {
            return Err(Fail::Safe(
                "Abler returned an invalid session refresh response.",
            ));
        }

        if !session.jar.has("/graphql", "id_token") {
            return Err(Fail::Safe(
                "Abler did not issue an access cookie. Capture a fresh session.",
            ));
        }
        Ok(())
    }

    fn query(
        &self,
        session: &mut Session,
        operation: &str,
        query: &str,
        variables: Value,
        force_refresh: bool,
    ) -> Result<Map<String, Value>> {
        let result = self.query_response(session, operation, query, variables, force_refresh, false)?;
        if result.errors > 0 {
            return Err(Fail::Safe("Abler rejected the request. The session may lack permission, or the API may have changed."));
        }
        result.data.ok_or(Fail::Safe("Abler returned no data."))
    }

    /// The same transport and one-refresh rule, retaining GraphQL validation errors only for
    /// discovery. Normal tools still use query() and its original reviewed failure boundary.
    fn query_response(
        &self, session: &mut Session, operation: &str, query: &str,
        variables: Value, force_refresh: bool, discovery: bool,
    ) -> Result<Graphql> {
        let mut redactions = crate::describe::cookie_values(&session.jar.serialize());
        let access = session
            .jar
            .get("/graphql")
            .into_iter()
            .find(|cookie| cookie.key == "id_token");

        if force_refresh || access.is_none_or(|access| access.ttl(js::now_ms()) < 60_000.0) {
            self.refresh(session)?;
            redactions.extend(crate::describe::cookie_values(&session.jar.serialize()));
        }
        let body = json!({ "operationName": operation, "query": query, "variables": variables });
        let mut response = self.post(session, "/graphql", Some(&body))?;
        redactions.extend(crate::describe::cookie_values(&session.jar.serialize()));
        let mut status = response.status();
        let mut result = graphql(&self.read_json(response));

        if status.as_u16() == 401 || result.as_ref().is_some_and(|result| result.unauthenticated) {
            self.refresh(session)?;
            redactions.extend(crate::describe::cookie_values(&session.jar.serialize()));
            response = self.post(session, "/graphql", Some(&body))?;
            redactions.extend(crate::describe::cookie_values(&session.jar.serialize()));
            status = response.status();
            result = graphql(&self.read_json(response));
        }

        if !status.is_success() && !(discovery && [400, 403].contains(&status.as_u16()) && result.as_ref().is_some_and(|result| result.errors > 0 && !result.unauthenticated)) {
            return Err(Fail::Safe(
                "Abler returned an error for the requested operation.",
            ));
        }
        let mut result = result.ok_or(Fail::Safe("Abler returned an invalid API response."))?;

        if result.unauthenticated {
            // Server messages may contain private values. Never echo raw response bodies.
            return Err(Fail::Safe(
                "Abler rejected the request. The session may lack permission, or the API may have changed.",
            ));
        }
        result.redactions = redactions;
        Ok(result)
    }

    pub fn describe_api(&self, input: crate::describe::Request) -> Result<Value> {
        self.session(|client, session| {
            let query = crate::describe::introspection_query();
            let result = client.query_response(session, "DescribeApi", &query, json!({}), false, true)?;
            let mut secrets = result.redactions;
            if result.errors == 0 {
                let data = result.data.ok_or(Fail::Safe("Abler returned no schema discovery data."))?;
                return crate::describe::schema(&Value::Object(data), &input, &secrets);
            }
            let errors = result.messages.iter().map(|message| crate::describe::safe_error(message, &secrets)).collect::<Vec<_>>();
            let typename = client.query_response(session, "DescribeApiMutationType", "mutation DescribeApiMutationType { __typename }", json!({}), false, true)?;
            secrets.extend(typename.redactions);
            let mutation_type = typename.data.as_ref().and_then(|data| data.get("__typename")).and_then(Value::as_str)
                .filter(|name| crate::describe::name(name)).map(|name| crate::describe::safe_text(name, &secrets));
            let mut probes = Vec::new();
            probes.push(json!({"field": "__typename", "errors": typename.messages.iter().map(|message| crate::describe::safe_error(message, &secrets)).collect::<Vec<_>>()}));
            let mut suggestions = Vec::new();
            for (index, field) in crate::describe::PROBES.iter().enumerate() {
                let operation = format!("DescribeApiProbe{index}");
                let query = crate::describe::probe_query(&operation, field);
                let response = client.query_response(session, &operation, &query, json!({}), false, true)?;
                secrets.extend(response.redactions);
                for message in &response.messages {
                    suggestions.extend(crate::describe::suggestions(message).into_iter().map(|name| crate::describe::safe_text(&name, &secrets)));
                }
                probes.push(json!({"field": field, "errors": response.messages.iter().map(|message| crate::describe::safe_error(message, &secrets)).collect::<Vec<_>>()}));
            }
            // Re-sanitize earlier diagnostics after any cookie rotation during later probes.
            crate::describe::fallback(errors, mutation_type, probes, suggestions, &input, &secrets)
        })
    }

    /// Use an authenticated read so 'authenticated' never means only 'file exists'.
    pub fn status(&self, force_refresh: bool) -> Result<Value> {
        self.session(|client, session| {
            let data = client.query(
                session,
                "SessionStatus",
                "query SessionStatus { me { id displayName } }",
                json!({}),
                force_refresh,
            )?;
            Ok(json!({ "authenticated": true, "account": parse(&PERSON, data.get("me"))? }))
        })
    }

    pub fn profile(&self) -> Result<Value> {
        self.session(|client, session| client.profile_with_session(session))
    }

    fn profile_with_session(&self, session: &mut Session) -> Result<Value> {
        let data = self.query(
            session,
            "Profile",
            "query Profile { me { id displayName children { id displayName } } }",
            json!({}),
            false,
        )?;

        if !data.get("me").is_some_and(truthy) {
            return Err(Fail::Safe("Abler returned no signed-in user."));
        }
        let mut profile = parse(&PROFILE, data.get("me"))?;
        let mut names = Map::new();

        for child in profile["children"].as_array().into_iter().flatten() {
            names.insert(
                child["id"].as_str().unwrap_or_default().to_owned(),
                child["displayName"].clone(),
            );
        }
        // z.record never copies `__proto__`, and JavaScript orders integer-like keys first.
        names.shift_remove("__proto__");
        profile["childNamesById"] = Value::Object(js::order(names));
        Ok(profile)
    }

    pub fn groups(&self) -> Result<Value> {
        self.session(|client, session| {
            let data = client.query(
                session,
                "Groups",
                "query Groups { me { userAgeGroups {
        id name isActive groups { id name label } sport { id name }
      } } }",
                json!({}),
                false,
            )?;
            Ok(json!({ "groups": parse(&GROUPS, data.get("me"))?["userAgeGroups"] }))
        })
    }

    pub fn schedule(&self, filters: Schedule) -> Result<Value> {
        self.session(|client, session| client.schedule_with_session(session, &filters))
    }

    fn schedule_with_session(&self, session: &mut Session, input: &Schedule) -> Result<Value> {
        let mut filter = Map::new();

        if let Some(from) = &input.from {
            filter.insert("dateFrom".to_owned(), json!(from));
        }

        if let Some(to) = &input.to {
            filter.insert("dateTo".to_owned(), json!(to));
        }

        if let Some(types) = &input.types {
            filter.insert("label".to_owned(), json!(types));
        }

        if let Some(groups) = &input.group_ids {
            filter.insert("group".to_owned(), json!(groups));
        }

        if let Some(participants) = &input.participant_ids {
            filter.insert("participant".to_owned(), json!(participants));
        }
        let mut variables = json!({ "first": input.first, "cursor": input.after });

        if !filter.is_empty() {
            variables["filter"] = Value::Object(filter);
        }
        let data = self.query(
            session,
            "Schedule",
            &format!(
                "query Schedule($first: Int, $cursor: String, $filter: eventFilter) {{
      schedule(first: $first, after: $cursor, filter: $filter) {{ edges {{ node {{ {EVENT_FIELDS} }} }} {PAGE_FIELDS} }}
    }}"
            ),
            variables,
            false,
        )?;
        let page = page_of(&EVENT, data.get("schedule"))?;
        page.advanced(input.after.as_deref(), NOT_ADVANCED)?;

        Ok(json!({ "events": page.nodes, "pageInfo": page.page_info }))
    }

    pub fn child_schedules(&self, input: ChildSchedules) -> Result<Value> {
        self.session(|client, session| {
            let profile = client.profile_with_session(session)?;
            let names = &profile["childNamesById"];

            if input
                .child_ids
                .iter()
                .flatten()
                .any(|child| names.get(child).is_none())
            {
                return Err(Fail::Safe(
                    "Unknown child ID. Use get_profile to choose linked children.",
                ));
            }
            let selected: Vec<&Value> = profile["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|child| {
                    input
                        .child_ids
                        .as_ref()
                        .is_none_or(|ids| ids.iter().any(|id| child["id"] == id.as_str()))
                })
                .collect();

            if input
                .after_by_child
                .iter()
                .any(|(child, _)| !selected.iter().any(|selected| selected["id"] == child.as_str()))
            {
                return Err(Fail::Safe(
                    "A cursor was supplied for an unselected child. Match afterByChild keys to childIds.",
                ));
            }
            let mut children = Vec::new();

            for child in selected {
                let id = child["id"].as_str().unwrap_or_default();
                let after = input
                    .after_by_child
                    .iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, cursor)| cursor.clone());

                // `afterByChild[id]` finds an Object.prototype member, which is not a cursor.
                if after.is_none() && js::inherited(id) {
                    return Err(Fail::Invalid);
                }
                // Each child gets an upstream-filtered page, so another child's busy schedule
                // cannot hide theirs.
                let filters = Schedule {
                    participant_ids: Some(vec![id.to_owned()]),
                    after,
                    ..input.filters.clone()
                };
                let page = client.schedule_with_session(session, &filters)?;
                let events: Vec<Value> = page["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|event| {
                        let mut event = event.as_object().cloned().unwrap_or_default();
                        let attendance = event.shift_remove("currentPlayerAttendance");
                        let rows: Vec<Value> = attendance
                            .as_ref()
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter(|row| row["player"]["id"] == id)
                            .map(|row| json!({ "status": row["status"], "coachStatus": row["coachStatus"] }))
                            .collect();
                        event.insert("attendance".to_owned(), Value::Array(rows));
                        Value::Object(event)
                    })
                    .collect();
                children.push(json!({ "child": child, "events": events, "pageInfo": page["pageInfo"] }));
            }
            Ok(json!({ "children": children }))
        })
    }

    pub fn event(&self, input: Event) -> Result<Value> {
        self.session(|client, session| {
            let data = client.query(
                session,
                "Event",
                &format!(
                    "query Event($id: String!, $ageGroupId: String!) {{
        event(id: $id, ageGroupId: $ageGroupId, first: 1) {{ edges {{ node {{ {EVENT_FIELDS} }} }} {PAGE_FIELDS} }}
      }}"
                ),
                json!({ "id": input.event_id, "ageGroupId": input.age_group_id }),
                false,
            )?;
            let page = page_of(&EVENT, data.get("event"))?;
            let event = page.nodes.into_iter().next().ok_or(Fail::Safe(
                "Event not found or not accessible with this session.",
            ))?;

            if event["eventId"] != input.event_id.as_str()
                || event["ageGroup"]["id"] != input.age_group_id.as_str()
            {
                return Err(Fail::Safe(
                    "Abler returned a different event than requested.",
                ));
            }
            Ok(event)
        })
    }

    pub fn conversations(&self, input: Page) -> Result<Value> {
        self.session(|client, session| {
            let data = client.query(
                session,
                "Conversations",
                &format!(
                    "query Conversations($first: Int, $cursor: String) {{
        getMessageUnreadCount
        message(first: $first, after: $cursor) {{
          edges {{ node {{
            id name conversationType membersCount unreadCount
            messageGroup {{ id name }}
            user1 {{ id displayName }}
            user2 {{ id displayName }}
            messages(first: 1) {{ edges {{ node {{ {MESSAGE_FIELDS} }} }} }}
          }} }}
          {PAGE_FIELDS}
        }}
      }}"
                ),
                json!({ "first": input.first, "cursor": input.after }),
                false,
            )?;
            let page = page_of(&CONVERSATION, data.get("message"))?;
            page.advanced(
                input.after.as_deref(),
                "Abler pagination did not advance. Retry later; do not report these conversations as complete.",
            )?;
            let unread = parse(&S::Num, data.get("getMessageUnreadCount"))?;
            let conversations: Vec<Value> = page.nodes.iter().map(to_conversation).collect();

            Ok(json!({ "unreadCount": unread, "conversations": conversations, "pageInfo": page.page_info }))
        })
    }

    pub fn messages(&self, input: Messages) -> Result<Value> {
        self.session(|client, session| {
            let data = client.query(
                session,
                "ConversationMessages",
                &format!(
                    "query ConversationMessages($pagination: PaginationType!, $conversationIds: [ID!]) {{
        conversationMessages(pagination: $pagination, conversationIds: $conversationIds) {{
          edges {{ node {{ {MESSAGE_FIELDS} }} }}
          {PAGE_FIELDS}
        }}
      }}"
                ),
                json!({
                    "pagination": { "first": input.page.first, "after": input.page.after },
                    "conversationIds": [input.conversation_id],
                }),
                false,
            )?;
            let page = page_of(&MESSAGE, data.get("conversationMessages"))?;
            page.advanced(
                input.page.after.as_deref(),
                "Abler pagination did not advance. Retry later; do not report these messages as complete.",
            )?;
            let messages: Vec<Value> = page.nodes.iter().map(to_message).collect();

            Ok(json!({ "messages": messages, "pageInfo": page.page_info }))
        })
    }
}

/// `value ?? null`.
fn or_null(value: Option<&Value>) -> Value {
    value.cloned().unwrap_or(Value::Null)
}

fn to_message(message: &Value) -> Value {
    let attachments: Vec<Value> = message["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|attachment| {
            json!({
                "id": attachment["id"],
                "fileName": attachment["fileName"],
                "description": or_null(attachment.get("description")),
                "contentType": attachment["contentType"],
            })
        })
        .collect();

    json!({
        "id": message["id"],
        "body": or_null(message.get("messageBody")),
        "createdAt": message["createdAt"],
        "sender": or_null(message.get("creator")),
        "read": or_null(message.get("recipient").and_then(|recipient| recipient.get("isRead"))),
        "attachments": attachments,
    })
}

fn to_conversation(conversation: &Value) -> Value {
    let latest = conversation
        .get("messages")
        .and_then(|messages| messages["edges"].get(0))
        .map(|edge| to_message(&edge["node"]));
    let participants: Vec<&Value> = ["user1", "user2"]
        .iter()
        .filter_map(|user| conversation.get(*user).filter(|user| !user.is_null()))
        .collect();

    json!({
        "id": conversation["id"],
        "name": or_null(conversation.get("name")),
        "type": conversation["conversationType"],
        "membersCount": or_null(conversation.get("membersCount")),
        "unreadCount": conversation["unreadCount"],
        "group": or_null(conversation.get("messageGroup")),
        "participants": participants,
        "latestMessage": latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_shapes_strip_and_order_like_zod() {
        let value = json!({ "displayName": "A", "x": 1, "id": "p1" });
        assert_eq!(
            parse(&PERSON, Some(&value)).unwrap().to_string(),
            r#"{"id":"p1","displayName":"A"}"#
        );
        assert!(parse(&PERSON, Some(&json!({ "id": "", "displayName": "A" }))).is_err());
        assert!(parse(&PERSON, Some(&json!([]))).is_err());
        let subgroup = json!({ "userAgeGroups": [{ "id": "g", "name": "G", "sport": null, "groups": [{ "label": null, "name": "S", "id": "s" }] }] });
        assert_eq!(
            parse(&GROUPS, Some(&subgroup)).unwrap().to_string(),
            r#"{"userAgeGroups":[{"id":"g","name":"G","groups":[{"id":"s","name":"S","label":null}],"sport":null}]}"#
        );
        let incomplete =
            json!({ "edges": [], "pageInfo": { "hasNextPage": true, "endCursor": "c" } });
        assert!(matches!(
            page_of(&PERSON, Some(&incomplete)),
            Err(Fail::Invalid)
        ));
        let graphql =
            graphql(&json!({ "errors": [null, { "extensions": { "code": "UNAUTHENTICATED" } }] }))
                .unwrap();
        assert!(graphql.unauthenticated && graphql.errors == 2 && graphql.data.is_none());
        assert!(super::graphql(&json!({ "errors": [{ "extensions": { "code": 1 } }] })).is_none());
        assert_eq!(cookie_header("a=\u{e9}").unwrap().as_bytes(), b"a=\xe9");
        assert!(cookie_header("a=\u{100}").is_none());
    }
}
