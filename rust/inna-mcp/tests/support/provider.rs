//! tests/ts/integration.test.ts's `Provider`: the synthetic Inna its drop-in cases ran the binary
//! against, ported field for field. Its answers are byte for byte what the TypeScript one served
//! (fixtures/contract records them, and `answers_as_the_typescript_provider_did` compares). What
//! the TypeScript one checked with `expect` is a problem here: the request is dropped, as its
//! thrown expectation dropped it, and `finish` fails the test.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value, json};
use url::Url;

use super::{Reply, Request, Upstream};

/// One request as the provider recorded it.
#[derive(Debug, Clone)]
pub struct Call {
    pub url: Url,
    pub method: String,
    pub body: Option<String>,
}

/// An access-list entry: the live shape's digit strings, without an id key.
#[derive(Debug, Clone)]
pub struct Entry {
    pub system: &'static str,
    pub status: &'static str,
    pub skoli_id: Option<&'static str>,
    pub skoli_heiti: &'static str,
}

/// The provider's switches and recordings, as the TypeScript fields.
pub struct State {
    pub calls: Vec<Call>,
    pub posts: usize,
    pub fail_post: bool,
    pub redirect: bool,
    pub rate_limit: bool,
    pub ignore_switch: bool,
    pub switch_rate_limit: bool,
    pub unauthorized: bool,
    pub switch_unauthorized: bool,
    pub rotation: String,
    pub numeric: bool,
    pub switch_location: String,
    pub switches: usize,
    pub user: Value,
    pub selected: String,
    pub order: Vec<String>,
    pub contexts: HashMap<String, Value>,
    pub entries: HashMap<String, Entry>,
    pub problems: Vec<String>,
}

/// integration.test.ts's `student`.
pub fn student() -> Value {
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

/// integration.test.ts's `sibling`: the student with another user, student, school and name.
pub fn sibling() -> Value {
    let mut sibling = student();
    for (key, value) in [
        ("userId", json!(5)),
        ("studentId", json!("6")),
        ("schoolId", json!("8")),
        ("studentName", json!("Synthetic sibling")),
        ("schoolLong", json!("Synthetic second school")),
    ] {
        sibling[key] = value;
    }
    sibling
}

pub const SIBLING: &str = "5";

/// integration.test.ts's `request`.
pub fn request() -> Value {
    json!({
        "kind": "sick",
        "dateFrom": "2040-01-02",
        "dateTo": "2040-01-02",
        "reason": "Synthetic reason",
    })
}

/// What integration.test.ts's `expectClean` refuses in any output.
pub const DECOYS: [&str; 4] = ["9999999999", "8888888", "DO-NOT-RETURN", "auth/token"];

pub fn expect_clean(text: &str, at: &str) {
    for decoy in DECOYS {
        assert!(!text.contains(decoy), "{at}: holds {decoy}: {text}");
    }
}

impl Default for State {
    fn default() -> Self {
        let entry = |system, status, skoli_heiti| Entry {
            system,
            status,
            skoli_id: Some(match skoli_heiti {
                "Synthetic second school" => "8",
                _ => "3",
            }),
            skoli_heiti,
        };
        Self {
            calls: Vec::new(),
            posts: 0,
            fail_post: false,
            redirect: false,
            rate_limit: false,
            ignore_switch: false,
            switch_rate_limit: false,
            unauthorized: false,
            switch_unauthorized: false,
            rotation: "synthetic-rotated".into(),
            numeric: false,
            switch_location: "http://nam.inna.is/Components/Students/Students.html".into(),
            switches: 0,
            user: student(),
            selected: "1".into(),
            order: vec!["1".into(), SIBLING.into(), "9".into()],
            contexts: HashMap::from([("1".into(), student()), (SIBLING.into(), sibling())]),
            // The last entry is another Inna application.
            entries: HashMap::from([
                ("1".into(), entry("1", "1", "Synthetic school")),
                (SIBLING.into(), entry("1", "2", "Synthetic second school")),
                ("9".into(), entry("2", "1", "Synthetic school")),
            ]),
            problems: Vec::new(),
        }
    }
}

/// `Response.json(value)` as Bun serves it.
fn json_reply(value: &Value) -> Reply {
    Reply {
        status: 200,
        headers: vec![(
            "content-type".into(),
            "application/json;charset=utf-8".into(),
        )],
        body: value.to_string().into_bytes(),
    }
}

/// The provider; clones share one state.
#[derive(Clone, Default)]
pub struct Provider(Arc<Mutex<State>>);

impl Provider {
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// A loopback Inna that answers as this provider.
    pub fn upstream(&self) -> Upstream {
        let provider = self.clone();
        Upstream::new(move |request| provider.answer(request))
    }

    /// The path of each recorded request from `from` on.
    pub fn paths(&self, from: usize) -> Vec<String> {
        let state = self.state();
        state.calls[from..]
            .iter()
            .map(|call| call.url.path().to_owned())
            .collect()
    }

    /// Fails if any request broke what the TypeScript provider expected.
    pub fn finish(&self, at: &str) {
        let problems = std::mem::take(&mut self.state().problems);
        assert!(problems.is_empty(), "{at}: {}", problems.join("\n"));
    }

    /// `fetch(url, options)`.
    pub fn answer(&self, request: &Request) -> Reply {
        let mut state = self.state();
        match answer(&mut state, request) {
            Ok(reply) => reply,
            Err(problem) => {
                if !problem.starts_with("Synthetic") {
                    state
                        .problems
                        .push(format!("{} {}: {problem}", request.method, request.url));
                }
                Reply::dropped()
            }
        }
    }
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(Value::as_str)
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    match ok {
        true => Ok(()),
        false => Err(format!("expected {what}")),
    }
}

fn access(state: &State) -> Value {
    let entries = state.order.iter().map(|key| {
        let entry = state.entries.get(key);
        let on = *key == state.selected;
        let mut object = Map::new();
        object.insert("kennitala".into(), json!("9999999999"));
        object.insert("notandi_id".into(), json!("8888888"));
        object.insert(
            "url_login".into(),
            json!("https://nam.inna.is/auth/token?token=DO-NOT-RETURN"),
        );

        if let Some(entry) = entry {
            object.insert("system".into(), json!(entry.system));
            object.insert("status".into(), json!(entry.status));

            if let Some(skoli_id) = entry.skoli_id {
                object.insert("skoli_id".into(), json!(skoli_id));
            }
            object.insert("skoli_heiti".into(), json!(entry.skoli_heiti));
        }
        let number = |text: &str| {
            text.parse::<f64>()
                .map_or(Value::Null, |value| json!(value as i64))
        };
        object.insert(
            "userId".into(),
            match state.numeric {
                true => number(key),
                false => json!(key),
            },
        );
        // `system: undefined` leaves the key out; `Number(undefined)` is NaN, written as null.
        match (state.numeric, entry) {
            (true, Some(entry)) => object.insert("system".into(), number(entry.system)),
            (true, None) => object.insert("system".into(), Value::Null),
            (false, Some(entry)) => object.insert("system".into(), json!(entry.system)),
            (false, None) => None,
        };
        object.insert(
            "loggedIn".into(),
            match state.numeric {
                true => json!(on),
                false => json!(if on { "1" } else { "0" }),
            },
        );
        object.insert("isStudent".into(), json!("1"));
        object.insert("virkur".into(), json!("1"));
        object.insert("adgangur".into(), json!("1"));
        object.insert(
            "nafn".into(),
            state
                .contexts
                .get(key)
                .map_or(json!("Synthetic staff"), |context| {
                    context["studentName"].clone()
                }),
        );
        object.insert("title".into(), json!("Synthetic role"));
        object.insert("tegund".into(), json!("Synthetic kind"));
        object.insert("skoli_audk".into(), json!("SYN"));
        object.insert("showPersonalInfoConfirmationModal".into(), json!(false));
        Value::Object(object)
    });
    Value::Array(entries.collect())
}

fn switch_student(state: &mut State, url: &Url, request: &Request) -> Result<Reply, String> {
    check(request.method == "GET", "a GET switch")?;
    check(request.body.is_empty(), "no switch body")?;
    check(
        header(request, "x-requested-by").is_none(),
        "no X-Requested-By",
    )?;
    check(header(request, "x-xsrf-token").is_none(), "no X-XSRF-TOKEN")?;
    check(
        header(request, "authorization").is_none(),
        "no Authorization",
    )?;
    state.switches += 1;

    if state.switch_rate_limit {
        return Ok(Reply {
            status: 429,
            headers: vec![("retry-after".into(), "120".into())],
            body: Vec::new(),
        });
    }
    if state.switch_unauthorized {
        return Ok(Reply::empty(401));
    }
    let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let param = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    // `Number(null)` is 0.
    let index = param("i").map_or(Some(0), |text| match text {
        "" => Some(0),
        text => text.parse::<usize>().ok(),
    });
    let key = index.and_then(|index| state.order.get(index)).cloned();
    let entry = key.as_ref().and_then(|key| state.entries.get(key)).cloned();
    let keys: Vec<&str> = params.iter().map(|(key, _)| key.as_str()).collect();
    check(
        keys == ["i", "system", "status", "user_id"],
        "the switch query keys",
    )?;
    check(param("system") == Some("1"), "system 1")?;
    let undefined = "undefined";
    check(
        param("system") == Some(entry.as_ref().map_or(undefined, |entry| entry.system)),
        "the entry's system",
    )?;
    check(
        param("status") == Some(entry.as_ref().map_or(undefined, |entry| entry.status)),
        "the entry's status",
    )?;
    check(
        param("user_id") == Some(key.as_deref().unwrap_or("")),
        "the entry's user id",
    )?;

    if let Some(key) = key.filter(|_| !state.ignore_switch) {
        if let Some(context) = state.contexts.get(&key) {
            state.user = context.clone();
        }
        state.selected = key;
    }
    Ok(Reply {
        status: 303,
        headers: vec![
            ("location".into(), state.switch_location.clone()),
            (
                "set-cookie".into(),
                format!(
                    "SESSION=synthetic-switched-{}; Path=/; Secure; HttpOnly",
                    state.switches
                ),
            ),
        ],
        body: b"<html>Synthetic redirect</html>".to_vec(),
    })
}

fn answer(state: &mut State, request: &Request) -> Result<Reply, String> {
    let url = Url::parse(&request.url).map_err(|error| error.to_string())?;
    check(
        url.origin().ascii_serialization() == "https://nam.inna.is",
        "Inna's origin",
    )?;
    check(
        header(request, "cookie").is_some_and(|cookie| cookie.contains("SESSION=synthetic-")),
        "a synthetic SESSION cookie",
    )?;
    state.calls.push(Call {
        url: url.clone(),
        method: request.method.clone(),
        body: (!request.body.is_empty()).then(|| request.body.clone()),
    });

    if url.path() == "/auth/system" {
        return switch_student(state, &url, request);
    }
    if url.path() == "/Components/Students/Students.html" {
        return Ok(Reply {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: b"<html>Synthetic application</html>".to_vec(),
        });
    }
    check(url.path().starts_with("/api/"), "an API path")?;
    check(
        header(request, "x-requested-by") == Some("XMLHttpRequest"),
        "X-Requested-By",
    )?;
    check(
        header(request, "x-xsrf-token") == Some("synthetic-xsrf"),
        "X-XSRF-TOKEN",
    )?;

    if state.redirect {
        return Ok(Reply {
            status: 302,
            headers: vec![(
                "location".into(),
                "https://example.invalid/credential-trap".into(),
            )],
            body: Vec::new(),
        });
    }
    if state.rate_limit {
        return Ok(Reply {
            status: 429,
            headers: vec![("retry-after".into(), "60".into())],
            body: Vec::new(),
        });
    }
    if state.unauthorized {
        return Ok(Reply::empty(401));
    }
    if url.path() == "/api/UserData/GetLoggedInUser" {
        let mut user = state.user.as_object().unwrap().clone();
        user.insert("access".into(), access(state));
        user.insert("studentIdNumber".into(), json!("DO-NOT-RETURN"));
        user.insert("privateToken".into(), json!("DO-NOT-RETURN"));
        let mut reply = json_reply(&Value::Object(user));
        reply.headers.push((
            "set-cookie".into(),
            format!("SESSION={}; Path=/; Secure; HttpOnly", state.rotation),
        ));
        return Ok(reply);
    }
    if url.path() == "/api/RegisterAbsence/AddNewLeave" {
        state.posts += 1;
        check(request.method == "POST", "a POST write")?;

        if state.fail_post {
            return Err("Synthetic lost response".into());
        }
        return Ok(json_reply(&json!({"id": 123})));
    }
    let body = match url.path() {
        "/api/StudentTerms/GetStudentTerms" => {
            json!([{"termId": "4", "termCode": "Synthetic term"}])
        }
        "/api/ModulesAndBooklist/GetModulesAndBooklist"
        | "/api/Homework/GetStudentHomework"
        | "/api/StudentGrades/GetStudentGrades"
        | "/api/RegisterAbsence/GetLeaves"
        | "/api/RegisterAbsence/GetStudentRegisteredAbsences" => json!([]),
        "/api/Announcements/GetStudentAnnouncements" => json!([{
            "announcementId": "12",
            "date": "02.01.2040",
            "title": "Synthetic school-wide announcement",
            "sender": "Synthetic school",
            "contentHtml": "<p>Synthetic notice</p>",
            "hasOpened": false,
        }]),
        "/api/Timetable/GetTimetable" => json!([
            {
                "start": "2040-01-02T10:00:00",
                "end": "2040-01-02T11:00:00",
                "titleShort": "Synthetic lesson",
                "allDay": false,
            },
            {"start": "2040-01-02", "end": "2040-01-03", "titleShort": "Synthetic event", "allDay": true},
        ]),
        "/api/GetAssignments/GetStudentAssignments" => json!([{
            "assignmentId": "5",
            "name": "Synthetic assignment",
            "module": "Synthetic course",
            "type": "0",
            "assignedFullDate": "01.01.2040",
            "handInFullDate": "03.01.2040",
            "handedIn": 0,
            "isOpen": 1,
            "projectId": "6",
        }]),
        "/api/GetAssignments/GetAssignmentInfo" => json!({
            "assignmentId": "5",
            "name": "Synthetic assignment",
            "description": "<p>Instructions</p><script>discard()</script>",
            "moduleName": "Synthetic course",
            "groupId": "7",
            "groupName": "1",
            "moduleTermId": "8",
            "returnDate": "03.01.2040",
            "type": 0,
            "exam": 0,
            "weight": "20",
            "projectId": "6",
            "groupReturnSize": 0,
        }),
        "/api/GetAssignments/Groups/7/StudentProjects" => json!({
            "assignments": [{
                "id": 5,
                "name": "Synthetic assessment",
                "type": 0,
                "weight": 20,
                "grade": "8",
                "returnDate": 1,
                "assignDate": 1,
                "handedIn": true,
            }],
            "categories": [],
        }),
        "/api/Attendance/GetAttendance" => json!({
            "absencesTotal": [],
            "leaveOfAbsencesTotal": [],
            "attendanceTerm": {"realAttendance": "90", "attendance": "95"},
            "dateFrom": "01.01.2040",
            "dateTo": "01.05.2040",
            "termName": "Synthetic term",
            "nrClassesTotal": 10,
            "absencePointsTotal": "1",
            "modules": [],
        }),
        "/api/Attachment/GetModuleFiles" => json!([{
            "fileGroupId": "9",
            "groupId": "7",
            "fileGroup": "Synthetic group",
            "files": [{
                "name": "Synthetic file",
                "fileId": "10",
                "closed": false,
                "link": "https://example.invalid/do-not-fetch",
            }],
        }]),
        "/api/Messages/GetReceivedMessages" => json!({
            "count": 1,
            "messages": [{
                "messagesId": "11",
                "table": "A",
                "title": "Synthetic message",
                "sender": "Synthetic sender",
                "date": "02.01.2040",
                "status": "S",
            }],
        }),
        "/api/Messages/GetMessageDetails" => json!({
            "title": "Synthetic message",
            "message": "<p>Hello &amp; welcome</p>",
            "dateCreated": "02.01.2040",
            "dateSent": "02.01.2040",
            "sentTo": "Synthetic recipient",
            "type": "A",
            "attachmentList": [],
        }),
        "/api/RegisterAbsence/GetRegisterAbsences" => json!({
            "todayAllowed": true,
            "tomorrowAllowed": true,
            "today": false,
            "tomorrow": false,
            "doctorsNote": "",
            "comment": "",
        }),
        _ => return Err("Unimplemented synthetic endpoint".into()),
    };
    Ok(json_reply(&body))
}
