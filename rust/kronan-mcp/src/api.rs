//! Krónan's API, with packages/kronan-mcp/src/api.ts's requests, bounds, upstream shapes and
//! messages. A client operation may hold a file lock across requests, so it runs on a blocking
//! thread and waits for each request with `Handle::block_on`.

use std::cell::OnceCell;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use family_store::Cancel;
use reqwest::Method;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::{RwLock, watch};
use tokio::time::Instant;

use crate::attempts::{self, Checkout, Claim, FileLock};
use crate::auth;
use crate::error::{Fail, Result};
use crate::input::{Approval, ProductKey, Query, Window};
use crate::js;
use crate::shapes::{self, S};

#[cfg(not(feature = "test-origin"))]
const ORIGIN: &str = "https://api.kronan.is";

const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(20);

const TOKEN_REJECTED: Fail = Fail::Safe(
    "Krónan rejected the access token. Create a new token in Krónan settings and run kronan-mcp auth set.",
);

const ACCESS_DENIED: Fail = Fail::Safe(
    "Krónan denied this request for the saved token. Use a token for the right user or customer group, or one with the needed permission.",
);

const RESPONSE_TOO_LARGE: Fail =
    Fail::Safe("Krónan response exceeded the 4 MiB limit. Request a smaller page and retry.");

const CANCELLED: Fail =
    Fail::Safe("The Krónan request was cancelled while the server was shutting down.");

const INVALID_RESPONSE: Fail = Fail::Safe("Krónan returned an invalid API response.");

const REQUEST_FAILED: Fail =
    Fail::Safe("Krónan request failed or timed out. Check the connection and try again.");

const RATE_LIMITED: Fail = Fail::Safe(
    "Krónan rate limit reached (200 requests per 200 seconds per account). Wait and retry.",
);

const UNEXPECTED_DATA: Fail = Fail::Safe(
    "Krónan returned data outside the documented schema. The API may have changed; report this with the kronan-mcp version.",
);

const UPSTREAM_ERROR: Fail = Fail::Safe("Krónan returned an error for the requested operation.");

/// A sent mutation without a readable confirmation. It is never retried; the caller must re-read.
const WRITE_UNCONFIRMED: Fail = Fail::Safe(
    "Krónan did not confirm this change (connection failure, timeout, server error, or unreadable response); it may have been applied. Read the current state before trying again.",
);

const ORDER_NOT_FOUND: Fail =
    Fail::Safe("Order not found at Krónan. Use a token from list_orders.");

/// Any failure after an order-change request was sent; a 4xx does not prove nothing changed.
const ORDER_CHANGE_UNCONFIRMED: Fail = Fail::Safe(
    "Krónan did not confirm this order change; it may have been applied. Read get_order before anything else, and do not repeat the change until the order shows what happened.",
);

const WRITE_REFUSED: Fail = Fail::Safe(
    "Krónan refused the request; nothing was changed. Check the input against the current state.",
);

// Money-gate refusals. Each is sent before any charge-bearing request, so each says so.

const CHECKOUT_EMPTY: Fail = Fail::Safe(
    "The checkout is empty. Nothing was sent to Krónan: no slot was reserved and no order was placed or changed.",
);

const CHECKOUT_REPLACED: Fail = Fail::Safe(
    "The checkout token differs from the approved checkout; review it with get_checkout. Nothing was sent to Krónan: no slot was reserved and no order was placed or changed.",
);

const CHECKOUT_TOTAL_CHANGED: Fail = Fail::Safe(
    "The checkout total differs from the approved total; review it with get_checkout and ask the user again. Nothing was sent to Krónan: no slot was reserved and no order was placed or changed.",
);

const NO_ACTIVE_ORDER: Fail = Fail::Safe(
    "Krónan reports no active order. Nothing was sent to Krónan: no order was placed or changed.",
);

const ORDER_REPLACED: Fail = Fail::Safe(
    "The active order differs from the approved order; review it with get_active_order. Nothing was sent to Krónan: no order was placed or changed.",
);

const GATE_UNVERIFIED: Fail = Fail::Safe(
    "Could not read the checkout or active order to verify the approval; call get_checkout or get_active_order for the reason. Nothing was sent to Krónan: no slot was reserved and no order was placed or changed.",
);

const GATE_REFUSALS: [Fail; 5] = [
    CHECKOUT_EMPTY,
    CHECKOUT_REPLACED,
    CHECKOUT_TOTAL_CHANGED,
    NO_ACTIVE_ORDER,
    ORDER_REPLACED,
];

const OUTCOME_UNKNOWN: &str = "Outcome unknown: the request reached or may have reached Krónan, and no confirmation was read (an error status does not prove it was refused). Check get_active_order and list_orders and ask the user; do not retry or place another order. Order calls for this checkout stay blocked.";

macro_rules! weight_note {
    () => {
        "authorizedAmount is the amount authorized on the saved card; tell the user, because fees and the selected slot can make it higher than the approved checkout total. Separately, weight-charged products mean the final captured amount can differ."
    };
}

const ORDER_PLACED: &str = concat!(
    "Krónan accepted the order. ",
    weight_note!(),
    " Check get_active_order for its state."
);

const SLOT_RESERVED: &str = concat!(
    "Krónan reserved the slot and returned an order token. ",
    weight_note!(),
    " Check get_active_order before any other order call."
);

const LINES_ADDED: &str = concat!(
    "Krónan added the checkout lines to the active order. ",
    weight_note!(),
    " Check get_active_order for its state."
);

/// A response together with the deadline that bounds its request, for the body read.
struct Exchange {
    response: reqwest::Response,
    deadline: Instant,
}

#[cfg(not(feature = "test-origin"))]
fn origin() -> String {
    ORIGIN.to_owned()
}

/// A `test-origin` build never talks to Krónan: it sends to the loopback port in
/// KRONAN_TEST_ORIGIN, and without one to a closed local port.
#[cfg(feature = "test-origin")]
fn origin() -> String {
    let origin = std::env::var("KRONAN_TEST_ORIGIN").unwrap_or_default();
    let port = origin.strip_prefix("http://127.0.0.1:").unwrap_or_default();

    match !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
        true => origin,
        false => "http://127.0.0.1:9".to_owned(),
    }
}

/// Maps status codes to fixed messages; a 404 is only meaningful where the caller names it.
fn failure(status: u16, not_found: Option<Fail>) -> Option<Fail> {
    match status {
        401 => Some(TOKEN_REJECTED),
        403 => Some(ACCESS_DENIED),
        429 => Some(RATE_LIMITED),
        404 if not_found.is_some() => not_found,
        200..=299 => None,
        _ => Some(UPSTREAM_ERROR),
    }
}

/// `URLSearchParams` in insertion order.
fn query_string(query: &Query) -> String {
    let pairs: Vec<String> = query
        .iter()
        .map(|(name, value)| format!("{}={}", js::encode_query(name), js::encode_query(value)))
        .collect();
    match pairs.is_empty() {
        true => String::new(),
        false => format!("?{}", pairs.join("&")),
    }
}

/// Limit/offset pages arrive with absolute URLs; the result reports the window as flags. The next
/// offset counts the items actually returned, so a clamped or short upstream page cannot skip
/// items.
fn offset_page(page: Value, window: Window) -> Value {
    let has_next = !matches!(page.get("next"), None | Some(Value::Null));
    let returned = page["results"].as_array().map_or(0, Vec::len) as i64;
    json!({
        "count": page["count"],
        "limit": window.limit,
        "offset": window.offset,
        "hasNextPage": has_next,
        "nextOffset": if has_next { json!(window.offset + returned) } else { Value::Null },
        "results": page["results"],
    })
}

/// The order token of an accepted charge-bearing response, for the attempt record.
fn order_token_of(value: &Value) -> String {
    value["orderToken"].as_str().unwrap_or_default().to_owned()
}

/// Where the client's token comes from: the saved one, read for every request so `auth set`
/// applies without a restart, or one given to verify or show it.
pub enum Token {
    Saved,
    Given(String),
}

pub struct Client {
    token: Token,
    http: reqwest::Client,
    origin: String,
    handle: Handle,
    stop: watch::Sender<bool>,
    /// The same stop for the attempts lock wait.
    cancel: Cancel,
    active: Arc<RwLock<()>>,
}

impl Client {
    pub fn new(token: Token) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| Fail::Unknown)?;

        Ok(Self {
            token,
            http,
            origin: origin(),
            handle: Handle::current(),
            stop: watch::channel(false).0,
            cancel: Cancel::default(),
            active: Arc::new(RwLock::new(())),
        })
    }

    /// Cancel requests in flight, and every later one.
    pub fn abort(&self) {
        self.stop.send_replace(true);
        self.cancel.cancel();
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

    /// The token for a request: the saved one, read now, or the one given.
    fn token(&self) -> Result<String> {
        match &self.token {
            Token::Saved => auth::load_token(),
            Token::Given(token) => Ok(token.clone()),
        }
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&Value>,
    ) -> Result<Exchange> {
        self.send_as(&self.token()?, method, path, query, body)
    }

    fn send_as(
        &self,
        token: &str,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&Value>,
    ) -> Result<Exchange> {
        // Identifiers are validated and encoded, so URL normalization must not move the request.
        if path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        {
            return Err(Fail::Safe("Invalid identifier in the request."));
        }
        let url = format!("{}/api/v1{path}{}", self.origin, query_string(query));
        let authorization =
            HeaderValue::from_str(&format!("AccessToken {token}")).map_err(|_| Fail::Unknown)?;
        let announces_body = method != Method::GET;
        let mut request = self
            .http
            .request(method, url)
            .header(AUTHORIZATION, authorization)
            .header(ACCEPT, "application/json");

        match body {
            Some(body) => {
                request = request
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.to_string());
            }
            // fetch announces an empty body on every method that may carry one.
            None if announces_body => request = request.header(CONTENT_LENGTH, "0"),
            None => {}
        }
        let deadline = Instant::now() + TIMEOUT;
        let sent = self.wait(tokio::time::timeout_at(deadline, request.send()));
        let response = sent
            .ok_or(CANCELLED)?
            .ok()
            .and_then(std::result::Result::ok)
            // fetch's `redirect: 'error'` fails on any redirect status.
            .filter(|response| ![301, 302, 303, 307, 308].contains(&response.status().as_u16()))
            .ok_or(REQUEST_FAILED)?;
        Ok(Exchange { response, deadline })
    }

    fn json(&self, exchange: Exchange, not_found: Option<Fail>) -> Result<Value> {
        let Exchange { response, deadline } = exchange;

        // Error bodies may echo request details; they are discarded unread.
        if let Some(fail) = failure(response.status().as_u16(), not_found) {
            return Err(fail);
        }
        let read = self.wait(tokio::time::timeout_at(
            deadline,
            mcp_runtime::read_capped(response, MAX_RESPONSE_BODY_BYTES),
        ));
        let body = match read {
            None => return Err(CANCELLED),
            // The request timer also bounds the body; a stalled stream is a connection problem.
            Some(Err(_)) => return Err(REQUEST_FAILED),
            Some(Ok(Err(mcp_runtime::BodyError::TooLarge))) => return Err(RESPONSE_TOO_LARGE),
            Some(Ok(Err(_))) => return Err(INVALID_RESPONSE),
            Some(Ok(Ok(body))) => body,
        };
        js::parse(&body).ok_or(INVALID_RESPONSE)
    }

    fn get(&self, path: &str, query: &Query, schema: &S, not_found: Option<Fail>) -> Result<Value> {
        self.get_as(&self.token()?, path, query, schema, not_found)
    }

    fn get_as(
        &self,
        token: &str,
        path: &str,
        query: &Query,
        schema: &S,
        not_found: Option<Fail>,
    ) -> Result<Value> {
        let data = self.json(
            self.send_as(token, Method::GET, path, query, None)?,
            not_found,
        )?;
        shapes::parse(schema, &data).ok_or(UNEXPECTED_DATA)
    }

    fn post(&self, path: &str, body: &Value, schema: &S) -> Result<Value> {
        let data = self.json(
            self.send(Method::POST, path, &Query::new(), Some(body))?,
            None,
        )?;
        shapes::parse(schema, &data).ok_or(UNEXPECTED_DATA)
    }

    /// Fetches one limit/offset page and reports its window as flags instead of upstream URLs.
    fn offset_paged(&self, path: &str, window: Window, extra: Query, page: &S) -> Result<Value> {
        let mut query = vec![
            ("limit", window.limit.to_string()),
            ("offset", window.offset.to_string()),
        ];
        query.extend(extra);
        Ok(offset_page(self.get(path, &query, page, None)?, window))
    }

    /// Sends one mutation and never repeats it. A 4xx is a refusal before acceptance; every other
    /// failure after the request may have left is `WRITE_UNCONFIRMED`, or `unconfirmed` when
    /// given, which also replaces every error status.
    fn write(
        &self,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&Value>,
        unconfirmed: Option<Fail>,
    ) -> Result<Exchange> {
        self.write_as(&self.token()?, method, path, query, body, unconfirmed)
    }

    fn write_as(
        &self,
        token: &str,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&Value>,
        unconfirmed: Option<Fail>,
    ) -> Result<Exchange> {
        let exchange = match self.send_as(token, method, path, query, body) {
            // Token and identifier checks fail before sending; a failed or cancelled request may
            // have been sent.
            Err(cause) if cause == REQUEST_FAILED || cause == CANCELLED => {
                return Err(unconfirmed.unwrap_or(WRITE_UNCONFIRMED));
            }
            sent => sent?,
        };
        let status = exchange.response.status().as_u16();

        if (200..=299).contains(&status) {
            return Ok(exchange);
        }

        if let Some(unconfirmed) = unconfirmed {
            return Err(unconfirmed);
        }
        Err(match status {
            401 => TOKEN_REJECTED,
            403 => ACCESS_DENIED,
            429 => RATE_LIMITED,
            400..=499 => WRITE_REFUSED,
            _ => WRITE_UNCONFIRMED,
        })
    }

    fn write_json(
        &self,
        method: Method,
        path: &str,
        query: &Query,
        body: Option<&Value>,
        schema: &S,
        unconfirmed: Option<Fail>,
    ) -> Result<Value> {
        let exchange = self.write(method, path, query, body, unconfirmed)?;
        self.written(exchange, schema, unconfirmed)
    }

    /// A money tool's one charge-bearing POST, sent with the token its gate read.
    fn charge(&self, token: &str, path: &str, body: Option<&Value>, schema: &S) -> Result<Value> {
        let unconfirmed = Some(WRITE_UNCONFIRMED);
        let exchange =
            self.write_as(token, Method::POST, path, &Query::new(), body, unconfirmed)?;
        self.written(exchange, schema, unconfirmed)
    }

    /// The validated response of a sent write.
    fn written(&self, exchange: Exchange, schema: &S, unconfirmed: Option<Fail>) -> Result<Value> {
        let unconfirmed = unconfirmed.unwrap_or(WRITE_UNCONFIRMED);
        let data = self.json(exchange, None).map_err(|_| unconfirmed)?;
        shapes::parse(schema, &data).ok_or(unconfirmed)
    }

    /// Refuses unless the live checkout is the non-empty one, at the total, the user approved. The
    /// total is a consistency check, not a cap on the amount Krónan authorizes.
    fn verify_checkout(&self, token: &str, approval: &Approval) -> Result<Checkout> {
        let current = self.get_as(token, "/checkout/", &Query::new(), &shapes::CHECKOUT, None)?;

        if current["lines"].as_array().is_none_or(Vec::is_empty) {
            return Err(CHECKOUT_EMPTY);
        }

        if current["token"] != approval.checkout_token.as_str() {
            return Err(CHECKOUT_REPLACED);
        }

        if current["total"].as_f64() != Some(approval.total as f64) {
            return Err(CHECKOUT_TOTAL_CHANGED);
        }
        Ok(Checkout {
            token: approval.checkout_token.clone(),
            total: approval.total,
            print: attempts::fingerprint(&current),
        })
    }

    /// Claims the one attempt this approval allows, runs the gate, and sends one charge-bearing
    /// request. Gate and record refusals say nothing was sent; once the request may have left,
    /// every failure, 4xx included, is an unknown outcome (`None`).
    ///
    /// Rust: an intended difference from api.ts, which reads the saved token inside the request.
    /// The gate reads it first, and the gate's reads and the request use that one token, so a
    /// token store failure, such as a concurrent `auth set` holding the store, refuses with the
    /// store's own message before any record is written, instead of recording an unknown outcome
    /// for a request that never left.
    fn place(
        &self,
        tool: &'static str,
        approval: &Approval,
        gate: impl FnOnce(&str) -> Result<Checkout>,
        request: impl FnOnce(&str) -> Result<Value>,
    ) -> Result<Option<Value>> {
        let token = OnceCell::new();

        attempts::claim_attempt(
            &attempts::attempts_path()?,
            Claim {
                tool,
                expected_checkout_token: &approval.checkout_token,
                cancel: &self.cancel,
                gate: Box::new(|| {
                    let loaded = self.token()?;
                    let checkout =
                        gate(&loaded).map_err(|cause| match GATE_REFUSALS.contains(&cause) {
                            true => cause,
                            false => GATE_UNVERIFIED,
                        })?;
                    let _ = token.set(loaded);
                    Ok(checkout)
                }),
                send: Box::new(|| request(token.get().ok_or(Fail::Unknown)?)),
                order_token: order_token_of,
            },
            &FileLock,
        )
    }

    fn read_active_order(&self) -> Result<Value> {
        self.read_active_order_as(&self.token()?)
    }

    fn read_active_order_as(&self, token: &str) -> Result<Value> {
        let exchange = self.send_as(
            token,
            Method::GET,
            "/orders/currently-active/",
            &Query::new(),
            None,
        )?;

        // Krónan documents 404 with no body as 'no active order'; every other failure is an error.
        if exchange.response.status() == 404 {
            return Ok(json!({ "active": false, "order": null }));
        }
        let data = self.json(exchange, None)?;
        let order = shapes::parse(&shapes::ACTIVE_ORDER, &data).ok_or(UNEXPECTED_DATA)?;
        Ok(json!({ "active": true, "order": order }))
    }

    /// An authenticated read, so 'authenticated' never means only 'file exists'.
    pub fn status(&self) -> Result<Value> {
        let account = self.get("/me/", &Query::new(), &shapes::ME, None)?;
        Ok(json!({ "authenticated": true, "account": account }))
    }

    pub fn search_products(&self, body: &Value) -> Result<Value> {
        self.post("/products/search/", body, &shapes::SEARCH_RESULT)
    }

    pub fn product(&self, key: &ProductKey) -> Result<Value> {
        let path = match key {
            ProductKey::Sku(sku) => format!("/products/{}/", js::encode_component(sku)),
            ProductKey::Barcode(barcode) => {
                format!("/products/barcode/{}/", js::encode_component(barcode))
            }
        };
        self.get(
            &path,
            &Query::new(),
            &shapes::PRODUCT_DETAIL,
            Some(Fail::Safe("Product not found at Krónan.")),
        )
    }

    pub fn lookup_products(&self, body: &Value) -> Result<Value> {
        self.post("/products/batch/", body, &shapes::LOOKUP_RESULT)
    }

    pub fn categories(&self) -> Result<Value> {
        let categories = self.get("/categories/", &Query::new(), &shapes::CATEGORIES, None)?;
        Ok(json!({ "categories": categories }))
    }

    pub fn category_products(&self, slug: &str, page: i64) -> Result<Value> {
        self.get(
            &format!("/categories/{}/products/", js::encode_component(slug)),
            &vec![("page", page.to_string())],
            &shapes::CATEGORY_PRODUCTS,
            Some(Fail::Safe(
                "Category not found at Krónan. Use a leaf (third-level) slug from list_categories.",
            )),
        )
    }

    pub fn tags(&self) -> Result<Value> {
        let tags = self.get("/products/tags/", &Query::new(), &shapes::TAGS, None)?;
        Ok(json!({ "tags": tags }))
    }

    pub fn products_by_tag(&self, slug: &str, page: i64) -> Result<Value> {
        self.get(
            &format!("/products/by-tag/{}/", js::encode_component(slug)),
            &vec![("page", page.to_string())],
            &shapes::PRODUCT_PAGE,
            Some(Fail::Safe(
                "Tag not found at Krónan. Use a slug from list_product_tags.",
            )),
        )
    }

    pub fn products_on_sale(&self, page: i64) -> Result<Value> {
        self.get(
            "/products/on-sale/",
            &vec![("page", page.to_string())],
            &shapes::PRODUCT_PAGE,
            None,
        )
    }

    pub fn favorite_products(&self, page: i64) -> Result<Value> {
        self.get(
            "/products/favorites/",
            &vec![("page", page.to_string())],
            &shapes::PRODUCT_PAGE,
            None,
        )
    }

    pub fn orders(&self, window: Window, filters: Query) -> Result<Value> {
        self.offset_paged("/orders/", window, filters, &shapes::ORDERS_PAGE)
    }

    pub fn order(&self, token: &str) -> Result<Value> {
        self.get(
            &format!("/orders/{}/", js::encode_component(token)),
            &Query::new(),
            &shapes::ORDER,
            Some(ORDER_NOT_FOUND),
        )
    }

    pub fn active_order(&self) -> Result<Value> {
        self.read_active_order()
    }

    pub fn order_line_summary(&self, query: &Query) -> Result<Value> {
        self.get("/orders/line-summary/", query, &shapes::LINE_SUMMARY, None)
    }

    pub fn purchase_stats(&self, window: Window, query: Query) -> Result<Value> {
        self.offset_paged(
            "/product-purchase-stats/",
            window,
            query,
            &shapes::PURCHASE_STATS_PAGE,
        )
    }

    pub fn shopping_note(&self) -> Result<Value> {
        self.get(
            "/shopping-notes/",
            &Query::new(),
            &shapes::SHOPPING_NOTE,
            None,
        )
    }

    pub fn archived_shopping_note_lines(&self) -> Result<Value> {
        let lines = self.get(
            "/shopping-notes/lines-archived/",
            &Query::new(),
            &shapes::ARCHIVED_LINES,
            None,
        )?;
        Ok(json!({ "lines": lines }))
    }

    pub fn product_lists(&self, window: Window) -> Result<Value> {
        self.offset_paged(
            "/product-lists/",
            window,
            Query::new(),
            &shapes::PRODUCT_LISTS_PAGE,
        )
    }

    pub fn product_list(&self, token: &str) -> Result<Value> {
        self.get(
            &format!("/product-lists/{}/", js::encode_component(token)),
            &Query::new(),
            &shapes::PRODUCT_LIST_DETAIL,
            Some(Fail::Safe(
                "Product list not found at Krónan. Use a token from list_product_lists.",
            )),
        )
    }

    pub fn recipes(&self, window: Window) -> Result<Value> {
        self.offset_paged("/recipes/", window, Query::new(), &shapes::RECIPES_PAGE)
    }

    pub fn search_recipes(&self, body: &Value) -> Result<Value> {
        self.post("/recipes/search/", body, &shapes::RECIPE_SEARCH)
    }

    pub fn recipe(&self, slug: &str) -> Result<Value> {
        self.get(
            &format!("/recipes/{}/", js::encode_component(slug)),
            &Query::new(),
            &shapes::RECIPE_DETAIL,
            Some(Fail::Safe(
                "Recipe not found at Krónan. Use a slug from list_recipes.",
            )),
        )
    }

    pub fn favorite_recipes(&self, window: Window) -> Result<Value> {
        self.offset_paged(
            "/recipes/favorites/",
            window,
            Query::new(),
            &shapes::RECIPES_PAGE,
        )
    }

    pub fn addresses(&self) -> Result<Value> {
        let addresses = self.get("/addresses/", &Query::new(), &shapes::ADDRESSES, None)?;
        Ok(json!({ "addresses": addresses }))
    }

    /// Availability lookups are POSTs upstream but change nothing; reservation is a separate
    /// endpoint.
    pub fn delivery_slots(&self, body: &Value) -> Result<Value> {
        let days = self.post("/slots/delivery/", body, &shapes::DELIVERY_SLOTS)?;
        Ok(json!({ "days": days }))
    }

    pub fn pickup_slots(&self, body: &Value) -> Result<Value> {
        let stores = self.post("/slots/pickup/", body, &shapes::PICKUP_SLOTS)?;
        Ok(json!({ "stores": stores }))
    }

    pub fn checkout(&self) -> Result<Value> {
        self.get("/checkout/", &Query::new(), &shapes::CHECKOUT, None)
    }

    pub fn add_shopping_note_lines(&self, body: &Value) -> Result<Value> {
        self.write_json(
            Method::POST,
            "/shopping-notes/add-lines/",
            &Query::new(),
            Some(body),
            &shapes::SHOPPING_NOTE,
            None,
        )
    }

    pub fn change_shopping_note_line(&self, body: &Value) -> Result<Value> {
        self.write_json(
            Method::PATCH,
            "/shopping-notes/change-line/",
            &Query::new(),
            Some(body),
            &shapes::SHOPPING_NOTE,
            None,
        )
    }

    pub fn toggle_shopping_note_line_complete(&self, token: &str) -> Result<Value> {
        self.write_json(
            Method::PATCH,
            "/shopping-notes/toggle-complete-on-line/",
            &Query::new(),
            Some(&json!({ "token": token })),
            &shapes::SHOPPING_NOTE,
            None,
        )
    }

    pub fn delete_shopping_note_line(&self, token: &str) -> Result<Value> {
        self.write_json(
            Method::DELETE,
            "/shopping-notes/delete-line/",
            &vec![("token", token.to_owned())],
            None,
            &shapes::SHOPPING_NOTE,
            None,
        )
    }

    pub fn clear_shopping_note(&self) -> Result<Value> {
        // Krónan answers 204 with no body; the note itself is kept.
        self.write(
            Method::DELETE,
            "/shopping-notes/delete-shopping-note/",
            &Query::new(),
            None,
            None,
        )?;
        Ok(json!({ "cleared": true }))
    }

    /// Krónan documents this POST as validation only; the checkout is not modified.
    pub fn preview_checkout_lines(&self, body: &Value) -> Result<Value> {
        self.post("/checkout/preview-lines/", body, &shapes::PREVIEW)
    }

    /// `replace` is required input, so Krónan's replace-by-default never applies.
    pub fn set_checkout_lines(&self, body: &Value) -> Result<Value> {
        self.write_json(
            Method::POST,
            "/checkout/lines/",
            &Query::new(),
            Some(body),
            &shapes::CHECKOUT,
            None,
        )
    }

    fn reserve(
        &self,
        tool: &'static str,
        path: &str,
        approval: &Approval,
        body: &Value,
    ) -> Result<Value> {
        let reservation = self.place(
            tool,
            approval,
            |token| self.verify_checkout(token, approval),
            |token| self.charge(token, path, Some(body), &shapes::RESERVE_RESPONSE),
        )?;
        Ok(match reservation {
            None => {
                json!({ "outcome": "unknown", "reservation": null, "message": OUTCOME_UNKNOWN })
            }
            Some(reservation) => {
                json!({ "outcome": "accepted", "reservation": reservation, "message": SLOT_RESERVED })
            }
        })
    }

    pub fn reserve_delivery_slot(&self, approval: &Approval, body: &Value) -> Result<Value> {
        self.reserve(
            "reserve_delivery_slot",
            "/slots/delivery/reserve/",
            approval,
            body,
        )
    }

    pub fn reserve_pickup_slot(&self, approval: &Approval, body: &Value) -> Result<Value> {
        self.reserve(
            "reserve_pickup_slot",
            "/slots/pickup/reserve/",
            approval,
            body,
        )
    }

    /// The placement result of an order tool.
    fn placed(order: Option<Value>, accepted: &str) -> Value {
        match order {
            None => json!({ "outcome": "unknown", "order": null, "message": OUTCOME_UNKNOWN }),
            Some(order) => json!({ "outcome": "accepted", "order": order, "message": accepted }),
        }
    }

    pub fn complete_checkout(&self, approval: &Approval, body: &Value) -> Result<Value> {
        let order = self.place(
            "complete_checkout",
            approval,
            |token| self.verify_checkout(token, approval),
            |token| {
                self.charge(
                    token,
                    "/checkout/complete/",
                    Some(body),
                    &shapes::ORDER_TOKEN_RESPONSE,
                )
            },
        )?;
        Ok(Self::placed(order, ORDER_PLACED))
    }

    pub fn add_checkout_to_order(
        &self,
        approval: &Approval,
        expected_order: &str,
    ) -> Result<Value> {
        let order = self.place(
            "add_checkout_to_order",
            approval,
            |token| {
                let checkout = self.verify_checkout(token, approval)?;
                let active = self.read_active_order_as(token)?;

                if active["order"].is_null() {
                    return Err(NO_ACTIVE_ORDER);
                }

                if active["order"]["orderToken"] != expected_order {
                    return Err(ORDER_REPLACED);
                }
                Ok(checkout)
            },
            // The endpoint documents no request body.
            |token| {
                self.charge(
                    token,
                    "/checkout/add-to-order/",
                    None,
                    &shapes::ORDER_TOKEN_RESPONSE,
                )
            },
        )?;
        Ok(Self::placed(order, LINES_ADDED))
    }

    /// A placed-order change: never retried, and any failure after sending may have applied it.
    fn change_order(&self, token: &str, action: &str, body: &Value) -> Result<Value> {
        self.write_json(
            Method::POST,
            &format!("/orders/{}/{action}/", js::encode_component(token)),
            &Query::new(),
            Some(body),
            &shapes::ORDER,
            Some(ORDER_CHANGE_UNCONFIRMED),
        )
    }

    pub fn delete_order_lines(&self, token: &str, body: &Value) -> Result<Value> {
        self.change_order(token, "delete-lines", body)
    }

    pub fn lower_order_line_quantities(&self, token: &str, body: &Value) -> Result<Value> {
        self.change_order(token, "lower-quantity-lines", body)
    }

    pub fn toggle_order_line_substitution(&self, token: &str, body: &Value) -> Result<Value> {
        self.change_order(token, "lines-toggle-substitution", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_count_returned_items_and_queries_encode_like_url_search_params() {
        let page = json!({ "count": 60, "next": "https://x/?offset=50", "results": [1, 2] });
        assert_eq!(
            offset_page(
                page,
                Window {
                    limit: 100,
                    offset: 10
                }
            )
            .to_string(),
            r#"{"count":60,"limit":100,"offset":10,"hasNextPage":true,"nextOffset":12,"results":[1,2]}"#
        );
        let last = json!({ "count": 2, "next": null, "results": [1] });
        assert_eq!(
            offset_page(
                last,
                Window {
                    limit: 20,
                    offset: 1
                }
            )["nextOffset"],
            Value::Null
        );
        assert_eq!(
            query_string(&vec![
                ("name_contains", "mjólk & co".to_owned()),
                ("skus", "A".to_owned())
            ]),
            "?name_contains=mj%C3%B3lk+%26+co&skus=A"
        );
        assert_eq!(failure(404, None), Some(UPSTREAM_ERROR));
        assert_eq!(failure(204, None), None);
    }
}
