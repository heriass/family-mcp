//! packages/infomentor-mcp/src/http.ts's `InfoMentorHttp`: manual redirects, so cookie handling and
//! destination validation run on every hop, the rate-limit pause, the body cap and the parent
//! bootstrap read. Everything here blocks; it runs on a client's blocking thread.

use std::time::Duration;

use mcp_runtime::BodyError;
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, COOKIE, HeaderMap, HeaderName, HeaderValue, LOCATION,
    ORIGIN, REFERER, RETRY_AFTER, SET_COOKIE, USER_AGENT,
};
use serde_json::Value;
use tokio::time::Instant;
use url::Url;

use crate::error::{CANCELLED, Code, Fail, LOGIN_REQUIRED, Result};
use crate::html;
use crate::jar::Jar;
use crate::js;
use crate::session::{MAX_RATE_LIMIT_MS, PARENT_URL, origin, trusted_url};
use crate::shapes;
use crate::signal::Signal;
use crate::upstream::Net;

/// A response body larger than this is refused before it is buffered.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The client the TypeScript release identified itself as; InfoMentor sees the same client.
const AGENT: &str = "Bun/1.4.2";

/// `HttpPage`.
pub struct Page {
    pub url: Url,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pupil {
    pub id: String,
    pub name: String,
    pub selected: bool,
    pub switch_url: Option<String>,
}

/// `parentSchema`'s output.
#[derive(Debug, Clone, PartialEq)]
pub struct Parent {
    pub account_id: String,
    pub pupils: Vec<Pupil>,
    pub apps: Vec<String>,
}

impl Parent {
    fn from_value(value: &Value) -> Option<Self> {
        let value = shapes::parse(&shapes::PARENT, value)?;
        let text = |value: &Value, key: &str| value[key].as_str().map(str::to_owned);
        Some(Parent {
            account_id: text(&value["account"]["currentUser"], "id")?,
            pupils: value["account"]["pupils"]
                .as_array()?
                .iter()
                .map(|pupil| {
                    Some(Pupil {
                        id: text(pupil, "id")?,
                        name: text(pupil, "name")?,
                        selected: pupil["selected"].as_bool()?,
                        switch_url: text(pupil, "switchPupilUrl"),
                    })
                })
                .collect::<Option<_>>()?,
            apps: value["apps"]
                .as_array()?
                .iter()
                .map(|app| text(app, "codeName"))
                .collect::<Option<_>>()?,
        })
    }

    pub fn has_timetable(&self) -> bool {
        self.apps.iter().any(|app| app == "timetable")
    }
}

/// `parseParent`: the bootstrap JSON assignment; scripts returned by the school site never run.
pub fn parse_parent(html: &str) -> Result<Parent> {
    html::parent_scripts(html)
        .iter()
        .filter_map(|data| js::parse(data).as_ref().and_then(Parent::from_value))
        .next_back()
        .ok_or(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor parent data has changed or is unavailable.",
        ))
}

/// `URLSearchParams.toString()`.
pub fn form_body(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", js::encode_query(name), js::encode_query(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The challenge pages `http.ts` recognizes, matched case-insensitively.
fn is_challenge(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    if [
        "challenge-running",
        "challenge-stage",
        "challenges.cloudflare.com",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    lower.match_indices("<title").any(|(at, tag)| {
        let rest = &lower[at + tag.len()..];
        rest.find('>').is_some_and(|end| {
            let title = rest[end + 1..].trim_start_matches(js::is_space);
            ["just a moment", "security check", "verify you are human"]
                .iter()
                .any(|phrase| title.starts_with(phrase))
        })
    })
}

/// `/\/authentication\/authentication\/login(?:callback)?\b/i` at `at`, or anywhere.
fn login_path(path: &str, anchored: bool) -> bool {
    const LOGIN: &str = "/authentication/authentication/login";
    let lower = path.to_ascii_lowercase();
    let boundary = |rest: &str| {
        !rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    lower.match_indices(LOGIN).any(|(at, _)| {
        let rest = &lower[at + LOGIN.len()..];
        (!anchored || at == 0)
            && (boundary(rest) || rest.strip_prefix("callback").is_some_and(boundary))
    })
}

/// `requireSchoolPage`: a page from the parent site that is not its sign-in page.
fn require_school_page(page: &Page) -> Result<()> {
    let parent = Url::parse(PARENT_URL).map_err(|_| Fail::Unknown)?;

    if origin(&page.url) != origin(&parent) || login_path(page.url.path(), true) {
        return Err(LOGIN_REQUIRED);
    }
    Ok(())
}

/// A header value as fetch's `Headers` reads it: each byte one character.
fn latin1(value: &HeaderValue) -> String {
    value
        .as_bytes()
        .iter()
        .map(|&byte| char::from(byte))
        .collect()
}

fn debug_url(url: &Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
}

fn cookie_names(header: &str) -> Vec<String> {
    header
        .split(';')
        .filter_map(|cookie| cookie.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn debug_step(url: &Url, post: bool, browser: bool, cookies: &str, status: u16, headers: &HeaderMap) {
    let location = headers
        .get(LOCATION)
        .and_then(|value| url.join(&latin1(value)).ok())
        .map(|url| debug_url(&url));
    let set_cookies: Vec<String> = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| cookie_names(latin1(value).split(';').next().unwrap_or("")).into_iter().next())
        .collect();
    eprintln!(
        "[infomentor-debug] {} {:?} status={} location={:?} sent_cookies={:?} set_cookies={:?} mode={}",
        if post { "POST" } else { "GET" },
        debug_url(url),
        status,
        location,
        cookie_names(cookies),
        set_cookies,
        if browser { "browser" } else { "normal" },
    );
}

/// Never log arbitrary upstream text: even an error page can echo credentials or tokens.
/// Retain only public generic error phrases and HTML structure from the first 300 characters.
fn debug_preview(url: &Url, text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if login_path(url.path(), true)
        || ["<form", "<input", "openid_message", "oauth_token", "__viewstate", "__eventvalidation"]
            .iter()
            .any(|marker| lower.contains(marker))
    {
        return None;
    }
    let prefix: String = text.chars().take(300).collect();
    let mut rest = prefix.as_str();
    let mut preview = String::new();
    while !rest.is_empty() {
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else {
                preview.push_str("[redacted]");
                break;
            };
            let tag = rest[1..end].trim();
            let closing = tag.starts_with('/');
            let name = tag.trim_start_matches('/').split_ascii_whitespace().next().unwrap_or("");
            let name = name.trim_end_matches('/').to_ascii_lowercase();
            if ["html", "head", "title", "body", "h1", "h2", "p", "div", "span", "br", "hr", "pre"]
                .contains(&name.as_str())
            {
                preview.push('<');
                if closing { preview.push('/'); }
                preview.push_str(&name);
                preview.push('>');
            } else {
                preview.push_str("[redacted]");
            }
            rest = &rest[end + 1..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let content = rest[..end].trim();
            if !content.is_empty() {
                match content {
                    "Internal Server Error" | "Service Unavailable" | "Bad Request" | "Forbidden"
                    | "Access Denied" | "Not Found" | "Too Many Requests" | "The service is unavailable." => preview.push_str(content),
                    _ => preview.push_str("[redacted]"),
                }
            }
            rest = &rest[end..];
        }
    }
    Some(preview.chars().take(300).collect())
}

fn debug_error_body(url: &Url, text: &str) {
    if let Some(preview) = debug_preview(url, text) {
        eprintln!("[infomentor-debug] error_body={preview:?}");
    }
}

/// A header value as fetch's `Headers` takes it; `None` where it throws.
fn header(text: &str) -> Option<HeaderValue> {
    let bytes: Option<Vec<u8>> = text.chars().map(|c| u8::try_from(c).ok()).collect();
    HeaderValue::from_bytes(&bytes?).ok()
}

/// Why the request loop stopped without an InfoMentor answer: fetch threw, or the signal aborted.
enum Stop {
    Fail(Fail),
    Failed,
}

impl From<Fail> for Stop {
    fn from(fail: Fail) -> Self {
        Stop::Fail(fail)
    }
}

/// `InfoMentorHttp`.
pub struct Http {
    pub jar: Jar,
    cooldown_until: f64,
    pub parent: Option<Parent>,
    net: Net,
}

impl Http {
    pub fn new(jar: Jar, cooldown_until: f64, net: Net) -> Self {
        Self {
            jar,
            cooldown_until,
            parent: None,
            net,
        }
    }

    /// Epoch milliseconds until which InfoMentor asked this session to pause; 0 when it did not.
    pub fn rate_limited_until(&self) -> f64 {
        self.cooldown_until
    }

    /// `request(value, fields, signal, source)`.
    pub fn request(
        &mut self,
        value: &str,
        fields: Option<&[(String, String)]>,
        signal: &Signal,
        source: &str,
    ) -> Result<Page> {
        self.request_mode(value, fields, signal, source, false)
    }

    /// Browser headers for a login request and every redirect it follows.
    pub fn request_login(
        &mut self,
        value: &str,
        fields: Option<&[(String, String)]>,
        signal: &Signal,
        source: &str,
    ) -> Result<Page> {
        self.request_mode(value, fields, signal, source, true)
    }

    fn request_mode(
        &mut self,
        value: &str,
        fields: Option<&[(String, String)]>,
        signal: &Signal,
        source: &str,
        browser_login: bool,
    ) -> Result<Page> {
        let now = js::now_ms();

        if now < self.cooldown_until {
            return Err(Fail::Im {
                code: Code::RateLimited,
                message: "InfoMentor requested a pause. Wait before retrying.",
                retry_after_ms: Some(self.cooldown_until - js::now_ms()),
            });
        }
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let request_signal = signal.any(&Signal::timeout(REQUEST_TIMEOUT));
        let url = trusted_url(value)?;
        let body = fields.map(form_body);
        let previous = trusted_url(source)?;

        match self.hops(url, body, previous, &request_signal, browser_login) {
            Ok(page) => Ok(page),
            Err(_) if signal.aborted() => Err(CANCELLED),
            Err(Stop::Fail(fail @ Fail::Im { .. })) => Err(fail),
            Err(_) if Instant::now() >= deadline => Err(Fail::new(
                Code::NetworkError,
                "InfoMentor request timed out.",
            )),
            Err(_) => Err(Fail::new(
                Code::NetworkError,
                "InfoMentor request failed. Check the network.",
            )),
        }
    }

    fn hops(
        &mut self,
        mut url: Url,
        mut body: Option<String>,
        mut previous: Url,
        signal: &Signal,
        browser_login: bool,
    ) -> std::result::Result<Page, Stop> {
        let debug = std::env::var("INFOMENTOR_DEBUG").is_ok_and(|value| value == "1");
        for _ in 0..10 {
            signal.check()?;
            let mut headers = HeaderMap::new();
            let mut set = |name: HeaderName, value: &str| -> std::result::Result<(), Stop> {
                headers.insert(name, header(value).ok_or(Stop::Failed)?);
                Ok(())
            };
            if browser_login {
                set(
                    ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )?;
                set(ACCEPT_LANGUAGE, "is-IS,is;q=0.9,en;q=0.8")?;
                set(
                    USER_AGENT,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
                )?;
            } else {
                set(ACCEPT, "application/json, text/html;q=0.9")?;
                set(USER_AGENT, AGENT)?;
            }
            set(REFERER, previous.as_str())?;
            let cookies = self.jar.header(&url);

            if !cookies.is_empty() {
                set(COOKIE, &cookies)?;
            }

            if body.is_some() {
                set(CONTENT_TYPE, "application/x-www-form-urlencoded")?;
                set(ORIGIN, &origin(&previous))?;
            }
            let response = self
                .net
                .send(signal, &url, headers, body.clone())
                .ok_or(Stop::Failed)?
                .ok_or(Stop::Failed)?;

            for cookie in response.headers.get_all(SET_COOKIE) {
                self.jar.set(&latin1(cookie), &url);
            }

            let status = response.status;

            if debug {
                debug_step(&url, body.is_some(), browser_login, &cookies, status, &response.headers);
            }

            if [301, 302, 303, 307, 308].contains(&status) {
                let location = response.headers.get(LOCATION).map(latin1);
                // The body is never read; dropping the response cancels it.
                drop(response);
                let location = location.ok_or(Fail::new(
                    Code::UnexpectedPage,
                    "InfoMentor returned a redirect without a destination.",
                ))?;
                // `new URL(location, url)` throwing is not an InfoMentorError.
                let next = url.join(&location).map_err(|_| Stop::Failed)?;
                let next = trusted_url(next.as_str())?;

                if [301, 302, 303].contains(&status) {
                    body = None;
                }

                // Credentials are posted only to the login form's origin, never forwarded by a
                // 307/308.
                if body.is_some() && origin(&next) != origin(&url) {
                    return Err(Fail::new(
                        Code::UnexpectedPage,
                        "InfoMentor requested an unsupported cross-origin form redirect.",
                    )
                    .into());
                }
                previous = std::mem::replace(&mut url, next);
                continue;
            }

            if status == 429 {
                let retry = response.headers.get(RETRY_AFTER).map(latin1);
                if debug {
                    if let Some(Ok(bytes)) = self.net.read(signal, response, MAX_BODY_BYTES) {
                        debug_error_body(&url, &String::from_utf8_lossy(&bytes));
                    }
                } else {
                    drop(response);
                }
                let now = js::now_ms();
                let milliseconds = match retry.as_deref() {
                    Some(retry)
                        if !retry.is_empty() && retry.bytes().all(|b| b.is_ascii_digit()) =>
                    {
                        retry.parse::<f64>().unwrap_or(f64::INFINITY) * 1000.0
                    }
                    Some(retry) if !retry.is_empty() => js::parse_date(retry) - now,
                    _ => f64::NAN,
                };
                let wait = match milliseconds.is_finite() && milliseconds > 0.0 {
                    true => milliseconds,
                    false => 60_000.0,
                };
                let now = js::now_ms();
                let cooldown = wait.min(MAX_RATE_LIMIT_MS);
                self.cooldown_until = now + cooldown;
                return Err(Fail::Im {
                    code: Code::RateLimited,
                    message: "InfoMentor is limiting requests. No automatic retry was made.",
                    retry_after_ms: Some(cooldown),
                }
                .into());
            }
            let bytes = match self.net.read(signal, response, MAX_BODY_BYTES) {
                Some(Ok(bytes)) => bytes,
                Some(Err(BodyError::TooLarge)) => {
                    return Err(Fail::new(
                        Code::UnexpectedPage,
                        "InfoMentor returned an unexpectedly large response.",
                    )
                    .into());
                }
                Some(Err(BodyError::Failed)) | None => return Err(Stop::Failed),
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();

            if debug && !(200..=299).contains(&status) {
                debug_error_body(&url, &text);
            }

            if is_challenge(&text) {
                return Err(Fail::new(
                    Code::ChallengeRequired,
                    "InfoMentor requires an interactive security check. Direct HTTP login cannot complete it; no automatic retry was made.",
                )
                .into());
            }

            match status {
                401 => return Err(LOGIN_REQUIRED.into()),
                403 => {
                    return Err(Fail::new(
                        Code::AccessDenied,
                        "InfoMentor denied access. Check the account before retrying.",
                    )
                    .into());
                }
                200..=299 => return Ok(Page { url, text }),
                _ => {
                    return Err(Fail::new(
                        Code::NetworkError,
                        "InfoMentor returned an error. Try again later.",
                    )
                    .into());
                }
            }
        }
        Err(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor returned too many redirects.",
        )
        .into())
    }

    /// `isAuthenticated`: InfoMentor's own check, an empty form post.
    pub fn is_authenticated(&mut self, signal: &Signal) -> Result<bool> {
        let page = self.request(
            &format!("{PARENT_URL}authentication/authentication/isauthenticated/"),
            Some(&[]),
            signal,
            PARENT_URL,
        )?;
        let answer = js::trim(&page.text);

        if answer == "true" || answer == "false" {
            return Ok(answer == "true");
        }

        if login_path(page.url.path(), false) {
            return Ok(false);
        }
        Err(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor returned an unsupported authentication response.",
        ))
    }

    /// `requireAuthentication`.
    pub fn require_authentication(&mut self, signal: &Signal) -> Result<()> {
        match self.is_authenticated(signal)? {
            true => Ok(()),
            false => Err(LOGIN_REQUIRED),
        }
    }

    /// `readAppData`: InfoMentor's read endpoints use form posts, including paging and search.
    /// `shape` is the schema; `None` is a parse failure.
    pub fn read_app_data<T>(
        &mut self,
        path: &str,
        fields: &[(&str, String)],
        signal: &Signal,
        shape: impl FnOnce(&Value) -> Option<T>,
    ) -> Result<T> {
        let fields: Vec<(String, String)> = fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        let page = self.request(
            &format!("{PARENT_URL}{path}"),
            Some(&fields),
            signal,
            PARENT_URL,
        )?;
        require_school_page(&page)?;
        js::parse(&page.text)
            .as_ref()
            .and_then(shape)
            .ok_or(Fail::new(
                Code::UnexpectedPage,
                "InfoMentor school data is unavailable or its format has changed.",
            ))
    }

    /// `readParent(signal, childId)`: the parent page, after selecting `child_id` when given.
    pub fn read_parent(&mut self, signal: &Signal, child_id: Option<&str>) -> Result<Parent> {
        let page = self.request(PARENT_URL, None, signal, PARENT_URL)?;
        require_school_page(&page)?;
        let mut parent = parse_parent(&page.text)?;

        if let Some(child_id) = child_id {
            let child = parent
                .pupils
                .iter()
                .find(|pupil| pupil.id == child_id)
                .ok_or(Fail::config(
                    "This child is not registered in the account. Use a child ID from infomentor_get_overview.",
                ))?;

            if !child.selected {
                let link = child
                    .switch_url
                    .as_deref()
                    .filter(|link| !link.is_empty())
                    .ok_or(Fail::new(
                        Code::UnexpectedPage,
                        "InfoMentor did not provide a child switch link.",
                    ))?;
                // `new URL(link, PARENT_URL)` throwing is not an InfoMentorError.
                let url = Url::parse(PARENT_URL)
                    .and_then(|base| base.join(link))
                    .map_err(|_| Fail::Unknown)?;
                let url = trusted_url(url.as_str())?;
                // `/^\/Account\/PupilSwitcher\/SwitchPupil\/\d+$/i`.
                const SWITCH: &str = "/account/pupilswitcher/switchpupil/";
                let switch = url.path().get(..SWITCH.len()).is_some_and(|start| {
                    start.eq_ignore_ascii_case(SWITCH)
                        && url.path().len() > SWITCH.len()
                        && url.path()[SWITCH.len()..]
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                });

                if origin(&url) != "https://minn.infomentor.is"
                    || !switch
                    || url.query().is_some_and(|query| !query.is_empty())
                    || url.fragment().is_some_and(|fragment| !fragment.is_empty())
                {
                    return Err(Fail::new(
                        Code::UnexpectedPage,
                        "InfoMentor returned an unsupported child switch link.",
                    ));
                }
                let switched = self
                    .request(url.as_str(), None, signal, PARENT_URL)
                    .and_then(|_| self.read_parent(signal, None));
                parent = switched.map_err(|fail| {
                    fail.rewrap(
                        "InfoMentor could not select the child. Selection may have changed; refresh infomentor_get_overview before continuing.",
                    )
                })?;
            }
            let mut selected = parent.pupils.iter().filter(|pupil| pupil.selected);

            match (selected.next(), selected.next()) {
                (Some(only), None) if only.id == child_id => {}
                _ => {
                    return Err(Fail::new(
                        Code::UnexpectedPage,
                        "InfoMentor did not confirm the requested child. Selection may have changed; refresh infomentor_get_overview before continuing.",
                    ));
                }
            }
        }
        self.parent = Some(parent.clone());
        Ok(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_metadata_omits_query_fragments_and_cookie_values() {
        let url = Url::parse("https://user:password@minn.infomentor.is/path?oauth_token=secret#secret").unwrap();
        assert_eq!(debug_url(&url), "https://minn.infomentor.is/path");
        assert_eq!(cookie_names("IMHome=secret; transaction=another"), ["IMHome", "transaction"]);
    }

    #[test]
    fn debug_error_previews_are_bounded_and_never_include_dynamic_text_or_attributes() {
        let url = Url::parse("https://im1.infomentor.is/Production/Mentor/Villa.aspx").unwrap();
        let text = "<html><body data-token='secret-token'><h1>Service Unavailable</h1><p>username=secret-user password=secret-password Cookie=secret-cookie https://example.test/?token=secret-query</p></body></html>";
        let preview = debug_preview(&url, text).unwrap();
        assert!(preview.contains("Service Unavailable"));
        assert!(!preview.contains("secret"));
        assert!(!preview.contains("data-token"));
        assert!(!preview.contains('?'));
        assert!(debug_preview(&url, &"é".repeat(1000)).unwrap().chars().count() <= 300);
        assert_eq!(debug_preview(&url, "<form><input value='secret'></form>"), None);
        assert_eq!(debug_preview(&url, "openid_message oauth_token=secret"), None);
        let login = Url::parse("https://minn.infomentor.is/Authentication/Authentication/Login").unwrap();
        assert_eq!(debug_preview(&login, "Service Unavailable"), None);
    }

    #[test]
    fn challenge_and_login_pages_are_recognized() {
        assert!(is_challenge("<div class=Challenge-Running>"));
        assert!(is_challenge("<TITLE lang=en>\n Just a moment...</title>"));
        assert!(is_challenge("x challenges.cloudflare.com/turnstile"));
        assert!(!is_challenge("<title>Just a  moment</title>"));
        assert!(!is_challenge("<title x=>>Verify you are human"));
        assert!(login_path(
            "/Authentication/Authentication/LoginCallback",
            true
        ));
        assert!(login_path("/authentication/authentication/login/", true));
        assert!(!login_path("/authentication/authentication/loginx", true));
        assert!(!login_path("/x/authentication/authentication/login", true));
        assert!(login_path("/x/authentication/authentication/login", false));
        assert_eq!(
            form_body(&[("a b".to_owned(), "Skólaferð & nesti".to_owned())]),
            "a+b=Sk%C3%B3laferð+%26+nesti".replace('ð', "%C3%B0")
        );
    }

    #[test]
    fn the_parent_bootstrap_is_read_without_evaluation() {
        let model = r#"{"account":{"currentUser":{"id":"p"},"pupils":[{"id":"1","name":"N","selected":true,"switchPupilUrl":null}]},"apps":[{"codeName":"timetable"}]}"#;
        let parent = parse_parent(&format!(
            "<script>IMHome.home.homeData = {model}; IMHome.home.init(1);</script><script>IMHome.home.homeData = {{bad}}; IMHome.home.init(1);</script>"
        ))
        .unwrap();
        assert_eq!(parent.account_id, "p");
        assert!(parent.has_timetable());
        assert_eq!(
            parse_parent("<script>IMHome.home.homeData = {bad}; IMHome.home.init(1);</script>")
                .unwrap_err(),
            Fail::new(
                Code::UnexpectedPage,
                "InfoMentor parent data has changed or is unavailable."
            )
        );
    }
}
