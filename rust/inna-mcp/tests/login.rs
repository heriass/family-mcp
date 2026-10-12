//! The phone sign-in, executed: tests/ts/login.test.ts:183 against `inna-mcp auth login`, as the
//! rust-login.ts drop-in ran it: the phone number on standard input, every sign-in host on one
//! loopback upstream answering as login.test.ts's `provider()`, ported here, and the poll waits
//! recorded in INNA_TEST_WAITS instead of slept. The authenticate request is held until the
//! binary has shown the security code, so the code is seen before that request, and the school
//! session is verified with GetLoggedInUser, answered as rust-login.ts's `user()`. What the
//! TypeScript provider checked with `expect` is a problem here, and fails the test. Needs no Bun;
//! needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::case::Gate;
use support::{Home, PATIENCE, Reply, Request, Upstream};
use url::Url;

/// `Buffer.from(text).toString('base64url')`.
fn base64url(text: &str) -> String {
    const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = text.as_bytes();
    let mut out = String::new();

    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });

        for index in 0..=chunk.len() {
            out.push(DIGITS[(value >> (18 - 6 * index) & 63) as usize] as char);
        }
    }
    out
}

/// login.test.ts's `token`: a JWT-shaped bearer token.
fn token() -> String {
    let encode = base64url;
    format!(
        "{}.{}.{}-",
        encode(r#"{"alg":"HS256","typ":"JWT"}"#),
        encode(r#"{"sub":"synthetic-subject"}"#),
        "x".repeat(32)
    )
}

/// login.test.ts's `session(data, success, wait)`.
fn session(data: &str, success: bool, wait: u64) -> Value {
    json!({
        "isSuccess": success,
        "retryWaitTime": wait,
        "retries": 10,
        "nexusUrl": null,
        "data": data,
        "timeoutErrorMessage": "Synthetic expired message",
        "isFirstPoll": false,
        "scriptId": null,
        "sessionId": null,
        "deviceLinkUrl": null,
    })
}

fn redirect(location: &str, cookie: Option<&str>) -> Reply {
    let mut headers = vec![("location".to_owned(), location.to_owned())];
    headers.extend(cookie.map(|cookie| ("set-cookie".to_owned(), cookie.to_owned())));
    Reply {
        status: 302,
        headers,
        body: Vec::new(),
    }
}

/// `Response.json(value, {headers})`.
fn json_reply(value: &Value, cookie: Option<&str>) -> Reply {
    let mut headers = vec![(
        "content-type".to_owned(),
        "application/json;charset=utf-8".to_owned(),
    )];
    headers.extend(cookie.map(|cookie| ("set-cookie".to_owned(), cookie.to_owned())));
    Reply {
        status: 200,
        headers,
        body: value.to_string().into_bytes(),
    }
}

/// `new Response(text)`.
fn text(body: &str) -> Reply {
    Reply {
        status: 200,
        headers: vec![(
            "content-type".to_owned(),
            "text/plain;charset=utf-8".to_owned(),
        )],
        body: body.as_bytes().to_vec(),
    }
}

/// rust-login.ts's `user(userId)`.
fn user(user_id: u64) -> Value {
    json!({
        "userId": user_id,
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

#[derive(Default)]
struct Provider {
    calls: Vec<(Url, String)>,
    polls: usize,
    selections: Vec<Vec<(String, String)>>,
    codes: Vec<String>,
    problems: Vec<String>,
}

fn expect(problems: &mut Vec<String>, ok: bool, what: &str) {
    if !ok {
        problems.push(format!("expected {what}"));
    }
}

/// login.test.ts's `provider().fetcher`, with rust-login.ts's GetLoggedInUser and held
/// authenticate request in front of it.
fn answer(state: &mut Provider, request: &Request, token: &str) -> Reply {
    let url = Url::parse(&request.url).unwrap();
    let host = url.host_str().unwrap_or_default().to_owned();
    let header = |name: &str| request.headers.get(name).and_then(Value::as_str);
    let body: Value = match request.body.as_str() {
        "" => json!({}),
        body => serde_json::from_str(body).unwrap_or(json!({})),
    };
    let problems = &mut state.problems;
    expect(
        problems,
        [
            "r.inna.is",
            "inna.is",
            "heimdallur.inna.is",
            "innskra.island.is",
            "nam.inna.is",
        ]
        .contains(&host.as_str()),
        "a sign-in host",
    );
    state.calls.push((url.clone(), request.method.clone()));
    expect(
        problems,
        !header("cookie").is_some_and(|cookie| cookie.contains("DECOY")),
        "no unrelated school cookie sent",
    );

    if let Some(authorization) = header("authorization") {
        expect(
            problems,
            host == "inna.is",
            "the bearer token only at inna.is",
        );
        expect(
            problems,
            authorization == format!("Bearer {token}"),
            "the bearer token",
        );
    }
    // The compared headers do not carry X-CSRF-TOKEN-IDS; the frozen phone-login cases check it.
    let returned = "/connect/authorize/callback?state=synthetic";

    match format!("{host}{}", url.path()).as_str() {
        "nam.inna.is/api/UserData/GetLoggedInUser" => json_reply(&user(2), None),
        "r.inna.is/auth/island" => redirect("https://heimdallur.inna.is/auth/island/login", None),
        "heimdallur.inna.is/auth/island/login" => {
            redirect("https://innskra.island.is/connect/authorize", None)
        }
        "innskra.island.is/connect/authorize" => redirect(
            &format!(
                "https://innskra.island.is/app/login?ReturnUrl={}",
                url::form_urlencoded::byte_serialize(returned.as_bytes())
                    .collect::<String>()
                    .replace('+', "%20")
            ),
            None,
        ),
        "innskra.island.is/app/login" => text("<html>synthetic phone shell</html>"),
        "innskra.island.is/login/context" => json_reply(
            &json!({"identityProviderRestrictions": []}),
            Some("CSRF-TOKEN-IDS=synthetic-csrf; Path=/; Secure"),
        ),
        "innskra.island.is/login/phone" => json_reply(
            &json!({"displayCode": "1234", "verificationProperties": "synthetic-verification"}),
            None,
        ),
        "innskra.island.is/login/phone/check-device" => {
            expect(
                problems,
                body == json!({
                    "returnUrl": "%2Fconnect%2Fauthorize%2Fcallback%3Fstate%3Dsynthetic",
                    "userIdentifier": "5550000",
                }),
                "the check-device body",
            );
            json_reply(
                &json!({"isTwoFactorRequired": false, "isNewLoginRestricted": false}),
                None,
            )
        }
        "innskra.island.is/login/phone/authenticate" => {
            expect(
                problems,
                body["verificationProperties"] == "synthetic-verification",
                "the verification properties",
            );
            json_reply(
                &json!({"userIdentifier": null, "session": session("synthetic-first", false, 2000)}),
                None,
            )
        }
        "innskra.island.is/login/phone/poll" => {
            expect(
                problems,
                body.get("userIdentifier").is_none(),
                "no user identifier",
            );
            let data = if state.polls > 0 {
                "synthetic-next"
            } else {
                "synthetic-first"
            };
            expect(problems, body["data"] == data, "the poll's data");
            state.polls += 1;
            json_reply(
                &match state.polls {
                    1 => session("synthetic-next", false, 3000),
                    _ => session("synthetic-success", true, 2000),
                },
                None,
            )
        }
        "innskra.island.is/login/phone/signin" => {
            expect(
                problems,
                body["session"]["data"] == "synthetic-success"
                    && body["session"]["isSuccess"] == true,
                "the successful session",
            );
            json_reply(
                &json!({"validReturnUrl": returned}),
                Some("innskra=synthetic-identity-cookie; Path=/; Secure"),
            )
        }
        "innskra.island.is/connect/authorize/callback" => {
            redirect("https://heimdallur.inna.is/auth/island/callback", None)
        }
        "heimdallur.inna.is/auth/island/callback" => {
            redirect("https://innskra.island.is/connect/endsession", None)
        }
        "innskra.island.is/connect/endsession" => {
            redirect("https://innskra.island.is/logout?logoutId=synthetic", None)
        }
        "innskra.island.is/logout" => text(
            r#"<a class="other PostLogoutRedirectUri" href="https://heimdallur.inna.is/auth/island/logout-callback?state=synthetic">Continue</a>"#,
        ),
        "heimdallur.inna.is/auth/island/logout-callback" => {
            redirect("https://inna.is/auth/island/callback?token=synthetic", None)
        }
        "inna.is/auth/island/callback" => text(&format!(
            r#"<script>var jwt = "{token}"; store.set('id_token',jwt);</script>"#
        )),
        "inna.is/auth/access" => json_reply(
            &json!([{"ssn": "DO-NOT-SAVE", "system": 1, "user_id": 2, "status": 3, "is_access": true}]),
            None,
        ),
        "inna.is/auth/user-terms-confirmed" => json_reply(&json!({"confirmed": true}), None),
        "inna.is/auth/system" => {
            expect(problems, request.method == "POST", "a POST selection");
            expect(problems, body == json!({}), "an empty selection body");
            let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
            expect(
                problems,
                keys == ["i", "system", "user_id", "status"],
                "the selection's query keys",
            );
            state.selections.push(pairs);
            json_reply(
                &json!({"url": "http://nam.inna.is/auth/token?token=synthetic"}),
                None,
            )
        }
        "nam.inna.is/auth/token" => {
            expect(
                problems,
                header("authorization").is_none(),
                "no bearer token at nam.inna.is",
            );
            redirect(
                "http://nam.inna.is/auth/system",
                Some("SESSION=synthetic-school; Path=/; Secure; HttpOnly"),
            )
        }
        "nam.inna.is/auth/system" => redirect(
            "http://nam.inna.is/Components/Students/Students.html",
            Some("XSRF-TOKEN=synthetic-xsrf; Path=/"),
        ),
        // An unrelated cookie of the school host: only SESSION and XSRF-TOKEN are kept (review of
        // PR #27, finding 3).
        "nam.inna.is/Components/Students/Students.html" => {
            let mut reply = text("<html>Synthetic student application</html>");
            reply.headers.push((
                "set-cookie".into(),
                "DECOY=synthetic-unrelated-school; Path=/; Secure".into(),
            ));
            reply
        }
        other => {
            problems.push(format!("Unexpected synthetic login request: {other}"));
            Reply::dropped()
        }
    }
}

/// login.test.ts:183
#[test]
fn phone_login_replaces_polling_completes_logout_relay_upgrades_http_keeps_school_cookies() {
    assert_eq!(
        base64url(r#"{"alg":"HS256","typ":"JWT"}"#),
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"
    );
    let home = Home::new("phone-login");
    let token = token();
    let state = Arc::new(Mutex::new(Provider::default()));
    let shown = Gate::default();
    let stderr = Arc::new(Mutex::new(String::new()));
    let (provider, code, written, bearer) =
        (state.clone(), shown.clone(), stderr.clone(), token.clone());
    let upstream = Upstream::new(move |request| {
        if request.line() == "POST /login/phone/authenticate" {
            {
                let state = provider.lock().unwrap();
                let early = state
                    .calls
                    .iter()
                    .any(|(url, _)| url.path() == "/login/phone/authenticate");
                assert!(!early, "authenticate before the code was shown");
            }
            // rust-login.ts: the request waits until the binary has shown the code.
            code.reached();
            let text = written.lock().unwrap().clone();
            let line = text
                .lines()
                .find(|line| {
                    line.starts_with("Security code ") && line.contains(": verify the match")
                })
                .unwrap();
            provider
                .lock()
                .unwrap()
                .codes
                .push(line["Security code ".len()..][..4].to_owned());
        }
        answer(&mut provider.lock().unwrap(), request, &bearer)
    });
    let waits = home.path("waits");
    let env = vec![
        (
            "INNA_SESSION_FILE".to_owned(),
            home.path("legacy/session.json").display().to_string(),
        ),
        ("INNA_TEST_WAITS".to_owned(), waits.display().to_string()),
    ];
    let mut child = home
        .command(&upstream.origin, &env, &["auth", "login"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"5550000\n").unwrap();
    let pipe = child.stderr.take().unwrap();
    let (sink, seen) = (stderr.clone(), shown.clone());
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let line = line.unwrap();
            let shows = line.starts_with("Security code ") && line.contains(": verify the match");
            let mut text = sink.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
            drop(text);

            if shows {
                // Lets the held authenticate request go on.
                seen.arrive();
            }
        }
    });
    let status = support::wait(&mut child, PATIENCE).expect("the sign-in finished");
    reader.join().unwrap();
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    let errors = stderr.lock().unwrap().clone();
    assert!(status.success(), "{errors}");
    assert!(
        stdout.ends_with("Signed in. Saved in an encrypted file.\n"),
        "{stdout}"
    );
    let state = state.lock().unwrap();
    assert!(state.problems.is_empty(), "{}", state.problems.join("\n"));
    assert_eq!(state.codes, ["1234"]);
    // The verification follows the page that set the decoy, so it would have carried it.
    let at = |path: &str| state.calls.iter().rposition(|(url, _)| url.path() == path);
    assert!(
        at("/api/UserData/GetLoggedInUser") > at("/Components/Students/Students.html"),
        "{:?}",
        state.calls
    );
    assert!(!format!("{stdout}{errors}").contains("synthetic-unrelated-school"));
    let selection = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    assert_eq!(
        state.selections,
        [selection(&[
            ("i", "0"),
            ("system", "1"),
            ("user_id", "2"),
            ("status", "3")
        ])]
    );
    let waited: Vec<String> = std::fs::read_to_string(&waits)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(waited, ["2000", "3000"]);
    let saved: Value = serde_json::from_str(&home.record().unwrap()).unwrap();
    let jar = saved["jar"].as_str().unwrap();
    assert!(!jar.contains(&token));
    assert!(!jar.contains("synthetic-identity-cookie"));
    assert!(!jar.contains("DO-NOT-SAVE"));
    assert!(!jar.contains("synthetic-unrelated-school"));
    let jar: Value = serde_json::from_str(jar).unwrap();
    let school: Vec<&Value> = jar["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cookie| cookie["domain"] == "nam.inna.is")
        .collect();
    let mut keys: Vec<&str> = school
        .iter()
        .map(|cookie| cookie["key"].as_str().unwrap())
        .collect();
    keys.sort();
    assert_eq!(keys, ["SESSION", "XSRF-TOKEN"]);
    assert!(
        school.iter().all(|cookie| cookie["secure"] == true),
        "{school:?}"
    );
}
