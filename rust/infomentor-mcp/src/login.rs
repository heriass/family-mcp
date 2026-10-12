//! packages/infomentor-mcp/src/login.ts: one credential submission to InfoMentor's login form,
//! the observed hidden-form relay, and the explicit login and import that commit a verified
//! session with its sign-in. No page JavaScript runs. Everything here blocks.

use std::path::Path;
use std::time::Duration;

use zeroize::Zeroize;

use crate::client::Options;
use crate::error::{CANCELLED, Code, Fail, Result};
use crate::html;
use crate::http::Http;
use crate::jar::Jar;
use crate::js;
use crate::session::{
    LOGIN_URL, PARENT_URL, SavedSession, capture, origin, read_session, session_path, trusted_url,
};
use crate::signal::Signal;
use crate::store::{
    Credentials, Prior, Record, change_session, commit_change, prepare_change, read_credentials,
    resolve,
};
use crate::upstream::Net;

const LOGIN_TIMEOUT: Fail = Fail::new(
    Code::LoginTimeout,
    "Sign-in timed out. The previous saved session was kept.",
);

const PASSWORD_FIELD: &str = "login_ascx$txtLykilord";

/// `loginDeadline`.
pub fn login_deadline(timeout_ms: f64) -> Result<Signal> {
    if timeout_ms.fract() != 0.0 || !(1.0..=3_600_000.0).contains(&timeout_ms) {
        return Err(Fail::config(
            "Login timeout must be between 1 millisecond and one hour.",
        ));
    }
    Ok(Signal::timeout(Duration::from_millis(timeout_ms as u64)))
}

/// An environment variable as `process.env` holds it.
fn env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `options.credentialsFile ?? INFOMENTOR_CREDENTIALS_FILE`, when not empty, as configured.
fn configured_file(options: &Options) -> Option<String> {
    match &options.credentials_file {
        Some(file) => Some(file.to_string_lossy().into_owned()),
        None => env("INFOMENTOR_CREDENTIALS_FILE"),
    }
    .filter(|file| !file.is_empty())
}

/// `hasConfiguredCredentials`: a set but empty username or password still counts.
pub fn has_configured_credentials(options: &Options) -> bool {
    configured_file(options).is_some()
        || env("INFOMENTOR_USERNAME").is_some()
        || env("INFOMENTOR_PASSWORD").is_some()
}

/// `resolveCredentials`: `stored` (renewal) first, then the configured sources. The file is the
/// credentials file read, as configured.
pub fn resolve_credentials(
    options: &Options,
    signal: &Signal,
    stored: Option<&Credentials>,
) -> Result<(Credentials, Option<String>)> {
    signal.check()?;

    if let Some(stored) = stored {
        return Ok((stored.clone(), None));
    }

    if let Some(file) = configured_file(options) {
        return Ok((
            read_credentials(&resolve(Path::new(&file))?, signal)?,
            Some(file),
        ));
    }
    let username = env("INFOMENTOR_USERNAME");
    let mut password = env("INFOMENTOR_PASSWORD");

    if username.is_none() && password.is_none() {
        return Err(Fail::config(
            "Credentials required. Use the app’s private secret input for INFOMENTOR_USERNAME (kennitala or InfoMentor username; no email required) and INFOMENTOR_PASSWORD, then run infomentor-mcp login with those secrets injected into its environment. If the MCP process already has them, call infomentor_login. Alternatively supply credentialsFile or importFile. Never put secret values in chat or MCP arguments.",
        ));
    }
    let configured = match (&username, &password) {
        (Some(username), Some(password)) => Credentials::new(username, password),
        _ => None,
    };
    password.zeroize();
    configured.map(|credentials| (credentials, None)).ok_or(Fail::config(
        "Use the app’s private secret input to provide both INFOMENTOR_USERNAME (kennitala or InfoMentor username; no email required) and INFOMENTOR_PASSWORD to the login process. Never put their values in chat or MCP arguments.",
    ))
}

/// `new URL(relative, base)`, as a trusted InfoMentor URL. A URL that does not parse is not an
/// InfoMentor failure.
fn trusted_join(base: &url::Url, relative: &str) -> Result<url::Url> {
    let joined = base.join(relative).map_err(|_| Fail::Unknown)?;
    trusted_url(joined.as_str())
}

fn login_origin() -> String {
    url::Url::parse(LOGIN_URL)
        .map(|url| origin(&url))
        .unwrap_or_default()
}

/// `authenticate`: establish the OAuth handoff, submit credentials once, then relay the result.
pub fn authenticate(http: &mut Http, credentials: Credentials, signal: &Signal) -> Result<()> {
    // The parent entry point establishes the OAuth transaction before Mentor can accept a
    // password. Http follows the login redirect while retaining and validating every cookie.
    let entry = http.request(PARENT_URL, None, signal, PARENT_URL)?;
    let handoff = html::parse_forms(&entry.text)
        .into_iter()
        .find(|form| form.id == "openid_message")
        .ok_or(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor returned an unsupported authentication handoff.",
        ))?;
    let destination = trusted_join(&entry.url, &handoff.action)?;
    if handoff.method != "post"
        || !handoff.has("oauth_token")
        || destination.as_str() != "https://im1.infomentor.is/Production/Mentor/"
    {
        return Err(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor returned an unsupported authentication handoff.",
        ));
    }
    let page = http.request(
        destination.as_str(),
        Some(&handoff.fields),
        signal,
        entry.url.as_str(),
    )?;
    let form = html::parse_forms(&page.text)
        .into_iter()
        .find(|form| form.has("__VIEWSTATE") && form.has("__EVENTVALIDATION"));
    let Some(mut form) = form.filter(|form| form.method == "post") else {
        return Err(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor returned an unsupported login form.",
        ));
    };
    let action = trusted_join(&page.url, &form.action)?;

    if origin(&action) != login_origin() {
        return Err(Fail::new(
            Code::UnexpectedPage,
            "InfoMentor changed its password form destination. Login stopped.",
        ));
    }
    form.set("login_ascx$txtNotandanafn", &credentials.username);
    form.set(PASSWORD_FIELD, &credentials.password);
    form.set("login_ascx$btnLogin", "Innskrá");
    drop(credentials);
    let response = http.request(
        action.as_str(),
        Some(&form.fields),
        signal,
        page.url.as_str(),
    );

    for (name, value) in &mut form.fields {
        if name == PASSWORD_FIELD {
            value.zeroize();
        }
    }
    let response = response?;
    let relay = html::parse_forms(&response.text)
        .into_iter()
        .find(|form| form.id == "openid_message");

    if let Some(relay) = relay {
        let destination = trusted_join(&response.url, &relay.action)?;

        if relay.method != "post"
            || !relay.has("oauth_token")
            || origin(&destination) != login_origin()
        {
            return Err(Fail::new(
                Code::UnexpectedPage,
                "InfoMentor returned an unsupported authentication handoff.",
            ));
        }
        http.request(
            destination.as_str(),
            Some(&relay.fields),
            signal,
            response.url.as_str(),
        )?;
    }

    if !http.is_authenticated(signal)? {
        return Err(Fail::new(
            Code::LoginRequired,
            "InfoMentor did not accept the login. Check your credentials; no automatic retry was made.",
        ));
    }
    Ok(())
}

/// `createAuthenticatedHttp`: a verified candidate; the caller commits only after checking the
/// account and context. `caller` is the signal of whoever asked; `deadline` the login's.
pub fn create_authenticated_http(
    net: &Net,
    caller: &Signal,
    credentials: Credentials,
    deadline: &Signal,
) -> Result<Http> {
    let signal = caller.any(deadline);
    let created = (|| {
        signal.check()?;
        let mut http = Http::new(Jar::default(), 0.0, net.clone());
        authenticate(&mut http, credentials, &signal)?;
        http.read_parent(&signal, None)?;
        Ok(http)
    })();
    created.map_err(|fail| ended(fail, caller, deadline))
}

/// A login's failure after its deadline or its caller stopped it.
fn ended(fail: Fail, caller: &Signal, deadline: &Signal) -> Fail {
    match (deadline.aborted() && !caller.aborted(), caller.aborted()) {
        (true, _) => LOGIN_TIMEOUT,
        (false, true) => CANCELLED,
        (false, false) => fail,
    }
}

/// `sessionFromHttp`: the jar's cookies, the verified account and selected child, and an active
/// rate-limit pause.
pub fn session_from_http(http: &Http) -> Result<SavedSession> {
    let mut session = capture(&http.jar)?;

    if let Some(parent) = &http.parent {
        session.account_id = Some(parent.account_id.clone());
        let mut selected = parent.pupils.iter().filter(|pupil| pupil.selected);

        if let (Some(only), None) = (selected.next(), selected.next()) {
            session.selected_child_id = Some(only.id.clone());
        }
    }

    if http.rate_limited_until() > js::now_ms() {
        session.rate_limited_until = Some(js::iso_string(http.rate_limited_until()));
    }
    Ok(session)
}

/// `httpFromSession`: cookies plus the pause InfoMentor requested.
pub fn http_from_session(session: &SavedSession, net: &Net) -> Http {
    Http::new(session.jar(), session.cooldown(), net.clone())
}

/// `requireSameAccount`: an explicit login or import must not silently switch the saved account.
/// A session that cannot be read safely cannot establish its account, so replacing it needs the
/// explicit override too.
fn require_same_account(previous: &Prior, candidate: &SavedSession, allow: bool) -> Result<()> {
    match (allow, previous) {
        (true, _) | (false, Prior::Nothing) => Ok(()),
        (false, Prior::Unknown) => Err(Fail::config(
            "The existing InfoMentor session cannot be verified. The previous session was kept. Log out first, or pass allowAccountChange to replace it.",
        )),
        (false, Prior::Session(previous)) => match (&previous.account_id, &candidate.account_id) {
            (Some(previous), Some(candidate)) if previous != candidate => Err(Fail::config(
                "The new sign-in belongs to a different InfoMentor account than the saved session. The previous session was kept. Log out first, or pass allowAccountChange to replace it.",
            )),
            _ => Ok(()),
        },
    }
}

/// `login`: sign in and save the verified session with the sign-in it used in one store write,
/// then remove the plaintext file. Returns the credentials file read, as configured, if any.
pub fn login(
    net: &Net,
    options: &Options,
    caller: &Signal,
    allow_account_change: bool,
    timeout_ms: f64,
) -> Result<Option<String>> {
    let legacy = session_path(options.session_file.as_deref())?;
    let deadline = login_deadline(timeout_ms)?;
    let signal = caller.any(&deadline);
    let bridge = signal.store_cancel(&net.handle);
    // Once the record is written, a late plaintext-removal failure reports itself, not a timeout.
    let mut committed = false;
    let outcome = change_session(
        &legacy,
        options.keys.clone(),
        &bridge.cancel,
        |store, record| {
            let (credentials, file) = resolve_credentials(options, &signal, None)?;
            let previous = prepare_change(store, record, &legacy)?;
            let http = create_authenticated_http(net, caller, credentials.clone(), &deadline)?;
            let session = session_from_http(&http)?;
            require_same_account(&previous.session, &session, allow_account_change)?;
            signal.check()?;
            let value = Record {
                session: Some(session),
                credentials: Some(credentials),
            };
            commit_change(store, &legacy, &value, || committed = true)?;
            Ok(file)
        },
    );

    finish(outcome, committed, caller, &deadline)
}

/// Once the record is written, a login's failure is its own, not a timeout or cancellation.
fn finish<T>(outcome: Result<T>, committed: bool, caller: &Signal, deadline: &Signal) -> Result<T> {
    match outcome {
        Err(fail) if !committed => Err(ended(fail, caller, deadline)),
        outcome => outcome,
    }
}

/// `importSession`: verify and save an exported session; a stored sign-in is kept only for the
/// same account.
pub fn import_session(
    net: &Net,
    options: &Options,
    file: &Path,
    signal: &Signal,
    allow_account_change: bool,
) -> Result<()> {
    signal.check()?;
    let legacy = session_path(options.session_file.as_deref())?;
    let bridge = signal.store_cancel(&net.handle);

    change_session(
        &legacy,
        options.keys.clone(),
        &bridge.cancel,
        |store, record| {
            let imported = read_session(&resolve(file)?)?;
            let previous = prepare_change(store, record, &legacy)?;
            let mut http = http_from_session(&imported, net);
            http.require_authentication(signal)?;
            http.read_parent(signal, None)?;
            let session = session_from_http(&http)?;
            require_same_account(&previous.session, &session, allow_account_change)?;
            signal.check()?;
            let kept = previous
                .record
                .as_ref()
                .and_then(|record| record.session.as_ref())
                .and_then(|session| session.account_id.clone());
            let credentials = match kept {
                Some(kept) if session.account_id.as_ref() == Some(&kept) => {
                    previous.record.and_then(|record| record.credentials)
                }
                _ => None,
            };
            commit_change(
                store,
                &legacy,
                &Record {
                    session: Some(session),
                    credentials,
                },
                || {},
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::Controller;

    fn aborted() -> Signal {
        let controller = Controller::default();
        controller.abort();
        controller.signal()
    }

    #[test]
    fn a_committed_login_reports_its_own_failure_after_the_deadline() {
        // integration.test.ts: 'a login whose store write committed reports a late removal
        // failure, not a timeout', whose slow key provider the binary cannot take.
        let removal = Fail::config(
            "Cannot remove the plaintext InfoMentor session file. Any encrypted-store change already completed; remove it by hand.",
        );
        let waiting = Signal::default();
        let finished = |committed, caller: &Signal, deadline: &Signal| {
            finish::<()>(Err(removal), committed, caller, deadline).unwrap_err()
        };
        assert_eq!(finished(true, &waiting, &aborted()), removal);
        assert_eq!(finished(true, &aborted(), &waiting), removal);
        assert_eq!(finished(false, &waiting, &aborted()), LOGIN_TIMEOUT);
        assert_eq!(finished(false, &aborted(), &aborted()), CANCELLED);
        assert_eq!(finished(false, &waiting, &waiting), removal);
    }

    #[test]
    fn login_deadlines_are_whole_milliseconds_up_to_an_hour() {
        for invalid in [0.0, -1.0, 1.5, 3_600_001.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                login_deadline(invalid).err(),
                Some(Fail::config(
                    "Login timeout must be between 1 millisecond and one hour."
                )),
                "{invalid}"
            );
        }
        assert!(login_deadline(1.0).is_ok() && login_deadline(3_600_000.0).is_ok());
    }
}
