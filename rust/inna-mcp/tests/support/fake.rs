//! tests/ts/fake-inna.ts, the fake nam.inna.is of parity.ts and renewal.test.ts, in its plain mode
//! (renewal.test.ts never set `odd`), with renewal.test.ts's controls: a session that expires
//! `ttl` after its last renewal, the gaps between renewals, one-shot 401s by path, a hook after
//! each renewal, planted answers and delays. Waiting on it is waiting on a condition of its state,
//! never a fixed sleep.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use super::{PATIENCE, Reply, Request, Upstream};

/// A planted answer for a path.
#[derive(Debug, Clone)]
pub struct Planted {
    pub status: u16,
    pub body: String,
}

impl Planted {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
        }
    }
}

/// One request the fake saw: when, and the status it answered.
#[derive(Debug, Clone)]
pub struct Seen {
    pub request: Request,
    pub path: String,
    pub at: Instant,
    pub status: Option<u16>,
}

type Hook = Box<dyn FnMut(&mut State) + Send>;

pub struct State {
    pub selected: String,
    pub ignore_switch: bool,
    pub switch_location: String,
    pub planted: HashMap<String, Planted>,
    pub delays: HashMap<String, Duration>,
    pub rotations: u64,
    pub ttl: Option<Duration>,
    pub renewed_at: Option<Instant>,
    pub renewal_gaps: Option<Vec<Duration>>,
    pub once401: HashMap<String, u32>,
    pub after_user: Option<Hook>,
    pub seen: Vec<Seen>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            selected: "1".into(),
            ignore_switch: false,
            switch_location: "http://nam.inna.is/Components/Students/Students.html".into(),
            planted: HashMap::new(),
            delays: HashMap::new(),
            rotations: 0,
            ttl: None,
            renewed_at: None,
            renewal_gaps: None,
            once401: HashMap::new(),
            after_user: None,
            seen: Vec::new(),
        }
    }
}

impl State {
    /// The path of each request seen from `from` on.
    pub fn paths(&self, from: usize) -> Vec<String> {
        self.seen[from..]
            .iter()
            .map(|seen| seen.path.clone())
            .collect()
    }
}

#[derive(Clone, Default)]
pub struct Fake(Arc<(Mutex<State>, Condvar)>);

const ORDER: [&str; 3] = ["1", "5", "9"];

const HTML: &str = "<p>A &amp; B&nbsp;&lt;tag&gt;</p><style>hidden</style><script>hidden()</script><ul><li>one<li>two</ul><div>  x \t y </div><br>tail &#128512; &unknown; <a href=\"https://example.invalid/do-not-fetch\">link</a>";

/// fake-inna.ts's `pick(0..=5)` of its dates.
const DATES: [&str; 6] = [
    "02.01.2040",
    "2040-01-02",
    "2040-01-02T10:00:00",
    "2040-01-02T10:00:00.5+01:00",
    "2040-01-02 10:00",
    "02.01.2040 10:00:00",
];

fn permissions(mut context: Value) -> Value {
    let extra = json!({
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
    });
    context
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    context
}

fn context(key: &str) -> Value {
    match key {
        "5" => {
            let mut name: String = "Synthetic sibling 🦊 with a name that runs on".into();
            // `padEnd(300, '·')` counts UTF-16 units; the fox is two.
            while name.encode_utf16().count() < 300 {
                name.push('·');
            }
            permissions(json!({
                "userId": 5,
                "studentId": "6",
                "schoolId": "8",
                "studentName": name,
                "name": "Synthetic guardian",
                "schoolLong": "Synthetic second school",
                "defaultTermId": "4",
            }))
        }
        _ => permissions(json!({
            "userId": 1,
            "studentId": "2",
            "schoolId": "3",
            "studentName": "Synthetic student",
            "name": "Synthetic guardian",
            "schoolLong": "Synthetic school",
            "defaultTermId": "4",
        })),
    }
}

fn access(state: &State) -> Value {
    let entries = ORDER.map(|key| {
        let mut entry = match key {
            "1" => json!({"system": "1", "status": "1", "skoli_id": "3", "skoli_heiti": "Synthetic school"}),
            "5" => json!({"system": 1, "status": 2, "skoli_id": 8, "skoli_heiti": "Synthetic second school", "title": 7}),
            _ => json!({"system": "2", "status": "1", "skoli_id": "3", "skoli_heiti": "Synthetic school"}),
        };
        let object = entry.as_object_mut().unwrap();
        object.insert("userId".into(), json!(key));
        object.insert(
            "loggedIn".into(),
            match (key == state.selected, key) {
                (true, _) => json!("1"),
                (false, "5") => json!(false),
                (false, _) => json!("0"),
            },
        );
        object.insert(
            "nafn".into(),
            match key {
                "9" => json!("Synthetic staff"),
                key => json!(format!("Synthetic name {key}")),
            },
        );
        object.insert("kennitala".into(), json!("9999999999"));
        object.insert(
            "url_login".into(),
            json!("https://nam.inna.is/auth/token?token=DO-NOT-RETURN"),
        );
        entry
    });
    json!(entries)
}

/// `feed(path, false)`.
fn feed(path: &str) -> Option<Value> {
    Some(match path {
        "/api/StudentTerms/GetStudentTerms" => {
            json!([{"termId": "4", "termCode": "Synthetic term", "extra": 1}])
        }
        "/api/ModulesAndBooklist/GetModulesAndBooklist" => json!([{
            "moduleId": "1", "moduleTermId": "2", "moduleName": "Synthetic module", "moduleName2": "",
            "subjectName": "Synthetic subject", "groupId": "7", "groupName": "Synthetic group",
            "termId": "4", "dateFrom": DATES[0], "dateTo": DATES[1],
        }]),
        "/api/Homework/GetStudentHomework" => {
            json!([{"id": 0.5, "date": DATES[0], "moduleName": "Synthetic", "text": HTML}])
        }
        "/api/Announcements/GetStudentAnnouncements" => json!([{
            "announcementId": "12", "date": DATES[0], "title": "Synthetic announcement",
            "sender": "Synthetic school", "contentHtml": HTML, "hasOpened": true,
        }]),
        "/api/Timetable/GetTimetable" => json!([{
            "start": DATES[0], "end": DATES[3], "titleShort": "Synthetic lesson", "allDay": false,
            "teacher": "Synthetic teacher",
        }]),
        "/api/GetAssignments/GetStudentAssignments" => json!([{
            "assignmentId": "5", "name": "Synthetic assignment", "module": "Synthetic course",
            "type": "0", "assignedFullDate": DATES[0], "handInFullDate": DATES[5], "handedIn": 0,
            "isOpen": 1, "projectId": "6",
        }]),
        "/api/GetAssignments/GetAssignmentInfo" => json!({
            "assignmentId": "5", "name": "Synthetic assignment",
            "description": "<p>Instructions</p><script>discard()</script>",
            "moduleName": "Synthetic course", "groupId": "7", "groupName": "1", "moduleTermId": "8",
            "returnDate": "03.01.2040", "type": 0, "exam": 0, "weight": "20", "projectId": "6",
            "groupReturnSize": 0,
        }),
        "/api/StudentGrades/GetStudentGrades" => json!([{
            "moduleTermId": "8", "termId": "4", "moduleName": "Synthetic module",
            "subjectName": "Synthetic subject", "units": "5", "status": "L", "show": true,
            "termCode": "Synthetic term",
        }]),
        "/api/GetAssignments/Groups/7/StudentProjects" => json!({
            "assignments": [{
                "id": 5, "name": "Synthetic assessment", "type": 0, "weight": 20, "returnDate": 1,
                "assignDate": 1, "handedIn": true,
            }],
            "categories": [],
        }),
        "/api/Attendance/GetAttendance" => json!({
            "absencesTotal": [{"code": "F", "name": "Synthetic", "number": "1"}],
            "leaveOfAbsencesTotal": [],
            "attendanceTerm": {"realAttendance": "90", "attendance": "95"},
            "dateFrom": "01.01.2040", "dateTo": "01.05.2040", "termName": "Synthetic term",
            "nrClassesTotal": 10, "absencePointsTotal": "1",
            "modules": [{
                "moduleName": "Synthetic module", "show": 1, "studentRecordId": "3",
                "attendance": {"realAttendance": "90", "attendance": "95"}, "absences": [],
                "leaveOfAbsence": [], "absencePoints": {"nrClasses": "10"},
            }],
        }),
        "/api/Attachment/GetModuleFiles" => json!([{
            "fileGroupId": "9", "groupId": "7", "fileGroup": "Synthetic group",
            "files": [{
                "name": "Synthetic file", "fileId": "10", "closed": true,
                "link": "https://example.invalid/do-not-fetch",
            }],
        }]),
        "/api/Messages/GetReceivedMessages" => json!({
            "count": 1,
            "messages": [{
                "messagesId": "11", "table": "A", "sender": "Synthetic sender", "date": DATES[0],
                "status": "S",
            }],
        }),
        "/api/Messages/GetMessageDetails" => json!({
            "title": "Synthetic message", "message": "<p>Hello &amp; welcome</p>",
            "dateCreated": "02.01.2040", "dateSent": "02.01.2040", "sentTo": "Synthetic recipient",
            "type": "A", "attachmentList": [],
        }),
        "/api/RegisterAbsence/GetRegisterAbsences" => json!({
            "todayAllowed": true, "tomorrowAllowed": false, "today": false, "tomorrow": false,
            "doctorsNote": "", "comment": "",
        }),
        "/api/RegisterAbsence/GetLeaves" => json!([{
            "id": 1, "dateFrom": DATES[0], "dateTo": DATES[1], "leaveType": "Synthetic",
            "status": "Synthetic", "statusCode": 1, "reasonForLeave": HTML, "createdBy": "Synthetic",
            "created": DATES[2],
            "classes": [{"date": DATES[0], "timeFrom": "08:00", "timeTo": "09:00", "class": "Synthetic"}],
        }]),
        "/api/RegisterAbsence/GetStudentRegisteredAbsences" => json!([{
            "id": 1, "date": DATES[0], "statusCode": 0, "allDay": "1", "classes": [],
        }]),
        _ => return None,
    })
}

fn send(status: u16, body: &str, extra: &[(&str, &str)]) -> Reply {
    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];

    for (name, value) in extra {
        match headers.iter_mut().find(|(known, _)| known == name) {
            Some(header) => header.1 = (*value).to_owned(),
            None => headers.push(((*name).to_owned(), (*value).to_owned())),
        }
    }
    Reply {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

impl Fake {
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.0.0.lock().unwrap()
    }

    pub fn upstream(&self) -> Upstream {
        let fake = self.clone();
        Upstream::new(move |request| fake.answer(request))
    }

    /// Waits until `ready` holds of the state, at most `patience`; whether it did.
    pub fn until(&self, patience: Duration, mut ready: impl FnMut(&State) -> bool) -> bool {
        let (lock, changed) = &*self.0;
        let state = lock.lock().unwrap();
        let (_state, timeout) = changed
            .wait_timeout_while(state, patience, |state| !ready(state))
            .unwrap();
        !timeout.timed_out()
    }

    /// `until`, failing the test when the state never gets there.
    #[track_caller]
    pub fn wait(&self, what: &str, ready: impl FnMut(&State) -> bool) {
        assert!(self.until(PATIENCE, ready), "the fake never saw {what}");
    }

    fn answer(&self, request: &Request) -> Reply {
        let (lock, changed) = &*self.0;
        let url = Url::parse(&request.url).unwrap();
        let path = url.path().to_owned();
        let (index, delay) = {
            let mut state = lock.lock().unwrap();
            state.seen.push(Seen {
                request: request.clone(),
                path: path.clone(),
                at: Instant::now(),
                status: None,
            });
            changed.notify_all();
            (state.seen.len() - 1, state.delays.get(&path).copied())
        };
        // The fake's own delay before it answers, as `await Bun.sleep(delay)`.
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let mut state = lock.lock().unwrap();
        let reply = respond(&mut state, &url, request);
        state.seen[index].status = Some(reply.status);
        changed.notify_all();
        reply
    }
}

fn respond(state: &mut State, url: &Url, request: &Request) -> Reply {
    let path = url.path();

    if url.host_str() != Some("nam.inna.is") {
        return send(404, "", &[]);
    }
    if let Some(planted) = state.planted.get(path) {
        return send(planted.status, &planted.body, &[]);
    }
    if let Some(count) = state.once401.get_mut(path).filter(|count| **count > 0) {
        *count -= 1;
        return send(401, "", &[]);
    }
    if let (Some(ttl), Some(renewed)) = (state.ttl, state.renewed_at)
        && renewed.elapsed() > ttl
    {
        return send(401, "", &[]);
    }
    if path == "/auth/system" {
        let index = url
            .query_pairs()
            .find(|(key, _)| key == "i")
            .and_then(|(_, value)| value.parse::<usize>().ok());

        if let Some(key) = index.and_then(|index| ORDER.get(index))
            && !state.ignore_switch
        {
            state.selected = (*key).to_owned();
        }
        let cookie = format!(
            "SESSION=synthetic-switched-{}; Path=/; Secure; HttpOnly",
            state.seen.len()
        );
        let location = state.switch_location.clone();
        return send(
            303,
            "<html>Synthetic redirect</html>",
            &[
                ("location", &location),
                ("set-cookie", &cookie),
                ("content-type", "text/html"),
            ],
        );
    }
    if path == "/Components/Students/Students.html" {
        return send(
            200,
            "<html>Synthetic application</html>",
            &[("content-type", "text/html")],
        );
    }
    // Inna answers its API only with a session cookie.
    let cookie = request.headers.get("cookie").and_then(Value::as_str);

    if path.starts_with("/api/") && !cookie.is_some_and(|cookie| cookie.contains("SESSION=")) {
        return send(401, "", &[]);
    }
    if path == "/api/UserData/GetLoggedInUser" {
        state.rotations += 1;

        if let (Some(renewed), Some(gaps)) = (state.renewed_at, state.renewal_gaps.as_mut()) {
            gaps.push(renewed.elapsed());
        }
        state.renewed_at = Some(Instant::now());

        if let Some(mut hook) = state.after_user.take() {
            hook(state);
            state.after_user.get_or_insert(hook);
        }
        let mut user = context(&state.selected);
        let object = user.as_object_mut().unwrap();
        object.insert("access".into(), access(state));
        object.insert("studentIdNumber".into(), json!("DO-NOT-RETURN"));
        let rotated = format!(
            "SESSION=synthetic-rotated-{}; Path=/; Secure; HttpOnly",
            state.rotations
        );
        let mut reply = send(200, &user.to_string(), &[]);
        reply.headers.push(("set-cookie".into(), rotated));
        reply
            .headers
            .push(("set-cookie".into(), "other=x; Path=/".into()));
        return reply;
    }
    let Some(mut body) = feed(path) else {
        return send(404, "", &[]);
    };
    // Inna pages messages by row, from 1.
    if path == "/api/Messages/GetReceivedMessages" {
        let number = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or(0)
        };
        let (from, to) = (number("rowFrom"), number("rowTo"));
        let messages = body["messages"].as_array().unwrap();
        let page: Vec<Value> = messages
            .iter()
            .skip(from.saturating_sub(1))
            .take(to.saturating_sub(from.saturating_sub(1)))
            .cloned()
            .collect();
        body["messages"] = json!(page);
    }
    send(200, &body.to_string(), &[])
}
