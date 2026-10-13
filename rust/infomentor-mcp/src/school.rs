//! Runtime discovery and strictly allowlisted, read-only school content.
use serde_json::{Map, Value, json};
use url::Url;
use crate::error::{Code, Fail, Result};
use crate::http::{Http, Parent};
use crate::session::{PARENT_URL, origin, trusted_url};
use crate::signal::Signal;
use crate::{html, js};

pub const APPS: [&str; 16] = ["home", "assessmentv2", "calendar", "calendarv2", "homework", "task", "message", "communication", "grouptimeline", "documentation", "portfolio", "timetable", "attendance", "uol", "uolv2", "notificationapp"];
const FORBIDDEN: [&str; 13] = ["send", "save", "update", "delete", "mark", "set", "post", "upload", "reply", "create", "remove", "submit", "clear"];
const READ_KEYS: [&str; 25] = ["getNavigationData", "getGridItems", "getTaskItems", "getUolItems", "getLearningStepItems", "getSelfAssessmentItems", "itemUrl", "getEntries", "getEntriesUrl", "getEntry", "getAttachments", "getMessages", "getMessage", "getMoreItems", "getTasks", "getTask", "getUols", "getUol", "getCalendarData", "allTimelineEntries", "getById", "list", "getNewsList", "getDocumentsList", "getLinksList"];

#[derive(Clone)]
pub struct Request {
    pub operation: &'static str,
    pub child: Option<String>,
    pub app: Option<String>,
    pub key: Option<String>,
    pub id: Option<String>,
    pub body: Value,
    pub max_chars: usize,
    pub page: i64,
    pub from: Option<String>,
    pub to: Option<String>,
}

pub fn input(operation: &'static str, args: &Map<String, Value>) -> std::result::Result<Request, String> {
    let allowed = ["app", "url_key", "id", "body", "childId", "pupil_id", "max_chars", "page", "from_date", "to_date"];
    if args.keys().any(|key| !allowed.contains(&key.as_str())) { return Err("Unrecognized school-read input field.".to_owned()); }
    let string = |name: &str| -> std::result::Result<Option<String>, String> {
        match args.get(name) {
            None => Ok(None), Some(Value::String(s)) if !s.is_empty() && s.len() <= 1024 => Ok(Some(s.clone())),
            _ => Err(format!("{name}: expected a nonempty string.")),
        }
    };
    let app = string("app")?;
    if app.as_deref().is_some_and(|app| !APPS[..15].contains(&app)) { return Err("app: unsupported school app.".to_owned()); }
    let key = string("url_key")?;
    if key.as_deref().is_some_and(|key| !allowed_key(key)) { return Err("url_key: only allowlisted read actions are permitted.".to_owned()); }
    if matches!(operation, "infomentor_app_data" | "infomentor_app_get") && app.is_none() { return Err("app: required.".to_owned()); }
    if operation == "infomentor_app_get" && key.is_none() { return Err("url_key: required.".to_owned()); }
    let id = match args.get("id") {
        None => None,
        Some(Value::String(id)) if crate::assessment::valid_id(id) => Some(id.clone()),
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n <= js::SAFE as u64) => Some(n.to_string()),
        _ => return Err("id: use an ID from a returned list.".to_owned()),
    };
    if matches!(operation, "infomentor_get_calendar_entry" | "infomentor_get_homework_item" | "infomentor_get_assignment") && id.is_none() {
        return Err("id: required.".to_owned());
    }
    let integer = |name: &str, default: i64, min: i64, max: i64| -> std::result::Result<i64, String> {
        match args.get(name) { None => Ok(default), Some(value) if value.as_i64().is_some_and(|v| (min..=max).contains(&v)) => Ok(value.as_i64().unwrap_or(default)), _ => Err(format!("{name}: integer outside supported range.")) }
    };
    let from = string("from_date")?; let to = string("to_date")?;
    if operation == "infomentor_get_calendar" {
        let valid = |date: Option<&str>| date.is_some_and(|date| date.len() == 10 && js::iso_datetime(&format!("{date}T00:00:00Z")).is_some());
        if !valid(from.as_deref()) || !valid(to.as_deref()) || from > to { return Err("Use from_date and to_date as an ordered ISO YYYY-MM-DD range.".to_owned()); }
    }
    let body = args.get("body").cloned().unwrap_or(Value::Null);
    if !(body.is_null() || body.is_object()) || !safe_body(&body, 0) || body.to_string().len() > 60_000 { return Err("body: only bounded, read-only JSON filter fields are permitted.".to_owned()); }
    Ok(Request { operation, child: string("childId")?.or(string("pupil_id")?), app, key, id, body,
        max_chars: integer("max_chars", 60_000, 1000, 200_000)? as usize, page: integer("page", 1, 1, 100_000)?, from, to })
}

pub fn forbidden(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    FORBIDDEN.iter().any(|word| lower.contains(word))
}

pub fn allowed_key(key: &str) -> bool {
    !forbidden(key) && (READ_KEYS.iter().any(|read| key.eq_ignore_ascii_case(read)) || key.eq_ignore_ascii_case("notificationAppData"))
}

fn changed() -> Fail {
    Fail::new(Code::UnexpectedPage, "InfoMentor did not provide a supported read-only school data endpoint.")
}

pub fn resolve_url(urls: &Value, key: &str, id: Option<&str>) -> Result<String> {
    if !allowed_key(key) { return Err(changed()); }
    let object = urls.as_object().ok_or_else(changed)?;
    let link = object.get(key).and_then(Value::as_str).ok_or_else(changed)?;
    let url = Url::parse(PARENT_URL).and_then(|base| base.join(link)).map_err(|_| changed())?;
    let url = trusted_url(url.as_str()).map_err(|_| changed())?;
    let parts: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    let communication_list = parts.len() == 3 && parts[0].eq_ignore_ascii_case("communication")
        && ["getNewsList", "getDocumentsList", "getLinksList"].iter().any(|key| parts[2].eq_ignore_ascii_case(key));
    if origin(&url) != "https://minn.infomentor.is" || url.query().is_some() || url.fragment().is_some()
        || parts.len() != 3 || parts.iter().any(|part| forbidden(part) || part.contains('%'))
        || !APPS.iter().any(|app| parts[0].eq_ignore_ascii_case(app))
        || (!parts[0].eq_ignore_ascii_case(parts[1]) && !communication_list)
        || !(["appdata", "notificationappdata", "item"].contains(&parts[2].to_ascii_lowercase().as_str())
            || READ_KEYS.iter().any(|read| parts[2].eq_ignore_ascii_case(read) || parts[2].eq_ignore_ascii_case(read.trim_start_matches("get").trim_end_matches("Url"))))
    { return Err(changed()); }
    let mut path = url.path().trim_matches('/').to_owned();
    if let Some(id) = id {
        if !crate::assessment::valid_id(id) { return Err(changed()); }
        path.push('/'); path.push_str(id);
    }
    Ok(path)
}

pub fn safe_body(body: &Value, depth: usize) -> bool {
    depth <= 12 && match body {
        Value::Object(object) => object.iter().all(|(key, value)| (!forbidden(key) || key == "offset") && !secret_key(key) && safe_body(value, depth + 1)),
        Value::Array(items) => items.iter().all(|value| safe_body(value, depth + 1)),
        _ => true,
    }
}

fn secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["token", "password", "cookie", "secret", "credential", "authorization", "csrf", "viewstate", "eventvalidation", "username", "kennitala", "ssn", "nationalid", "personalidentity"]
        .iter().any(|part| lower.contains(part)) || lower.contains("header") || lower == "account"
}

pub fn redact_identity(text: &str) -> String {
    let mut out = String::new();
    let mut digits = String::new();
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() { digits.push(c); continue; }
        if digits.len() == 10 { out.push_str("[redacted]"); } else { out.push_str(&digits); }
        digits.clear();
        if c != '\0' { out.push(c); }
    }
    let chars: Vec<char> = out.chars().collect();
    let mut safe = String::new(); let mut at = 0;
    while at < chars.len() {
        if at + 11 <= chars.len() && chars[at + 6] == '-'
            && chars[at..at + 6].iter().all(char::is_ascii_digit)
            && chars[at + 7..at + 11].iter().all(char::is_ascii_digit) {
            safe.push_str("[redacted]"); at += 11;
        } else { safe.push(chars[at]); at += 1; }
    }
    safe
}

fn safe_link(text: &str) -> Option<String> {
    let mut url = Url::parse(PARENT_URL).ok()?.join(text).ok()?;
    trusted_url(url.as_str()).ok()?;
    url.set_query(None); url.set_fragment(None);
    Some(redact_identity(url.path()))
}

fn iso_date(text: &str) -> Option<String> {
    let text = text.trim();
    if text.len() == 10 && js::iso_datetime(&format!("{text}T00:00:00Z")).is_some() { return Some(text.to_owned()); }
    if let Some(ms) = js::iso_datetime(text) { return Some(js::iso_string(ms)); }
    let words: Vec<&str> = text.split_whitespace().collect();
    if let Some(date) = words.first() {
        let parts: Vec<&str> = date.split('.').collect();
        if let [day, month, year] = parts.as_slice()
            && day.len() == 2 && month.len() == 2 && year.len() == 4 {
                let time = words.get(1).copied().unwrap_or("00:00");
                let time = if time.len() == 5 { format!("{time}:00") } else { time.to_owned() };
                let candidate = format!("{year}-{month}-{day}T{time}Z");
                return js::iso_datetime(&candidate).map(js::iso_string);
        }
    }
    if text.len() == 16 || text.len() == 19 {
        let candidate = if text.len() == 16 { format!("{text}:00Z") } else { format!("{text}Z") };
        return js::iso_datetime(&candidate).map(js::iso_string);
    }
    if text.len() >= 25 {
        let offset_at = text.len() - 6;
        if let (Some(base), Some(offset)) = (text.get(..offset_at), text.get(offset_at..)) {
            let bytes = offset.as_bytes();
            if b"+-".contains(&bytes[0]) && bytes[3] == b':' {
                let hour = offset[1..3].parse::<u32>().ok()?;
                let minute = offset[4..6].parse::<u32>().ok()?;
                if hour > 23 || minute > 59 { return None; }
                let ms = js::iso_datetime(&format!("{base}Z"))?;
                let shift = f64::from(hour * 60 + minute) * 60_000.0;
                return Some(js::iso_string(if bytes[0] == b'+' { ms - shift } else { ms + shift }));
            }
        }
    }
    None
}

pub fn sanitize(value: &Value, depth: usize, nodes: &mut usize, truncated: &mut bool) -> Value {
    if depth > 32 || *nodes >= 20_000 { *truncated = true; return Value::Null; }
    *nodes += 1;
    match value {
        Value::Object(object) => {
            let mut safe = Map::new();
            for (key, value) in object {
                if secret_key(key) || key == "urls" { continue; }
                if *nodes >= 20_000 { *truncated = true; break; }
                let lower = key.to_ascii_lowercase();
                let cleaned = if lower.ends_with("url") || lower == "href" || lower == "downloadpath" {
                    value.as_str().and_then(safe_link).map(Value::String).unwrap_or(Value::Null)
                } else if (lower.contains("date") || lower.ends_with("time") || ["start", "end", "timesent", "datesent"].contains(&lower.as_str())) && value.is_string() {
                    match value.as_str().and_then(iso_date) {
                        Some(date) => json!(date), None => sanitize(value, depth + 1, nodes, truncated),
                    }
                } else { sanitize(value, depth + 1, nodes, truncated) };
                safe.insert(redact_identity(js::slice_units(key, 128)), cleaned);
            }
            Value::Object(safe)
        }
        Value::Array(items) => {
            let mut safe = Vec::new();
            for item in items {
                if *nodes >= 20_000 { *truncated = true; break; }
                safe.push(sanitize(item, depth + 1, nodes, truncated));
            }
            Value::Array(safe)
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if ["oauth_token", "access_token", "password=", "authorization:", "__viewstate"].iter().any(|marker| lower.contains(marker)) {
                return json!("[redacted]");
            }
            if js::units(text) > 200_000 { *truncated = true; }
            let text = js::slice_units(text, 200_000);
            let text = if text.contains('<') { html::plain_text(text) } else { text.to_owned() };
            // Arbitrary strings may embed URLs. Strip their queries before returning them.
            let cleaned = text.lines().map(|line| line.split_whitespace().map(|word| {
                if let Some(at) = word.find("https://").or_else(|| word.find("http://")) {
                    let link = word[at..].trim_end_matches([')', ']', '}', ',', ';', '"']);
                    match Url::parse(link) {
                        Ok(mut url) if url.username().is_empty() && url.password().is_none() => {
                            url.set_query(None); url.set_fragment(None);
                            format!("{}{}{}", &word[..at], url, &word[at + link.len()..])
                        },
                        _ => "[link omitted]".to_owned(),
                    }
                } else { word.to_owned() }
            }).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n");
            if ["token=", "cookie=", "password=", "authorization:"].iter().any(|marker| cleaned.to_ascii_lowercase().contains(marker)) {
                json!("[redacted]")
            } else { json!(redact_identity(&cleaned)) }
        }
        Value::Number(n) if n.as_u64().is_some_and(|n| (1_000_000_000..=9_999_999_999).contains(&n)) => json!("[redacted]"),
        _ => value.clone(),
    }
}

pub fn bounded(data: &Value, max_chars: usize) -> Value {
    let mut truncated = false;
    let safe = sanitize(data, 0, &mut 0, &mut truncated);
    let text = safe.to_string();
    let oversized = js::units(&text) > max_chars;
    let mut paging = Map::new();
    let source = safe.get("initial").unwrap_or(&safe);
    for key in ["page", "pageSize", "more", "hasMore", "nextPage", "next", "total", "totalPages"] {
        if let Some(value) = source.get(key) { paging.insert(key.to_owned(), value.clone()); }
    }
    json!({
        "data": if oversized { Value::Null } else { safe },
        "json": if oversized { Some(js::slice_units(&text, max_chars)) } else { None },
        "truncated": truncated || oversized,
        "paging": paging,
        "next": if truncated || oversized { Some("Use returned paging or item IDs for further reads, or increase max_chars up to 200000. The JSON preview may end mid-value.") } else { None },
        "max_chars": max_chars,
        "timezone": "Atlantic/Reykjavik",
        "retrievedAt": js::iso_string(js::now_ms()),
    })
}

pub fn legacy_result(value: &Value, max_chars: usize) -> Value {
    let mut bounded = bounded(value, max_chars);
    if let Value::Object(mut data) = bounded["data"].take() {
        for key in ["truncated", "paging", "next", "timezone", "max_chars"] { data.insert(key.to_owned(), bounded[key].take()); }
        Value::Object(data)
    } else { bounded }
}

pub fn full_message(http: &mut Http, signal: &Signal, id: i64) -> Result<Value> {
    let value = http.read_form_data("Message/message/GetMessage", &[("id", id.to_string())], signal)?;
    let message = crate::shapes::message_detail(&value).ok_or(Fail::new(Code::UnexpectedPage, "InfoMentor school data is unavailable or its format has changed."))?;
    Ok(json!({"message": message, "fullContent": value}))
}

pub fn messages_page(http: &mut Http, signal: &Signal, folder: &str, page: i64, page_size: i64, search: &str) -> Result<crate::shapes::MessagesPage> {
    let value = http.read_form_data("Message/message/GetMessages", &[("page", page.to_string()), ("pageSize", page_size.to_string()), ("messageText", search.to_owned()), ("inbox", (folder == "inbox").to_string()), ("sentItems", (folder == "sent").to_string())], signal)?;
    crate::shapes::messages_response(&value).ok_or(Fail::new(Code::UnexpectedPage, "InfoMentor school data is unavailable or its format has changed."))
}

pub fn full_notifications(http: &mut Http, signal: &Signal) -> Result<Value> {
    let home = match load(http, signal, "home") {
        Err(fail) if fail.is(Code::UnexpectedPage) => return Ok(json!({"available": false, "message": "Full home notifications are not available for this pupil/school."})),
        outcome => outcome?,
    };
    let Some(key) = find_key(&home, &["notificationAppData"]) else {
        return Ok(json!({"available": false, "message": "Full home notifications are not available for this pupil/school."}));
    };
    fetch(http, signal, &home, key, None, &Value::Null)
}

pub fn selected(http: &mut Http, signal: &Signal, child: Option<&str>) -> Result<Parent> {
    let parent = match (child, &http.parent) { (None, Some(parent)) => parent.clone(), _ => http.read_parent(signal, child)? };
    if parent.pupils.iter().filter(|pupil| pupil.selected).count() != 1 { return Err(changed()); }
    Ok(parent)
}

fn enabled(parent: &Parent, app: &str) -> bool {
    app == "home" || parent.apps.iter().any(|code| code.eq_ignore_ascii_case(app))
}

pub fn load(http: &mut Http, signal: &Signal, app: &str) -> Result<Value> {
    if !APPS.contains(&app) { return Err(changed()); }
    let value = http.read_form_data(&format!("{app}/{app}/appData"), &[], signal)?;
    if !value.is_object() { return Err(changed()); }
    if app == "home" && value["homeData"].is_object() { Ok(value["homeData"].clone()) }
    else { Ok(value) }
}

pub fn fetch(http: &mut Http, signal: &Signal, app: &Value, key: &str, id: Option<&str>, body: &Value) -> Result<Value> {
    if !safe_body(body, 0) || body.to_string().len() > 60_000 { return Err(changed()); }
    let path = resolve_url(&app["urls"], key, id)?;
    http.read_json_data(&path, body, signal)
}

pub fn find_key<'a>(app: &'a Value, candidates: &[&str]) -> Option<&'a str> {
    let urls = app["urls"].as_object()?;
    candidates.iter().find_map(|candidate| urls.keys().find(|key| key.eq_ignore_ascii_case(candidate) && resolve_url(&app["urls"], key, None).is_ok()).map(String::as_str))
}

pub fn item_id(value: &Value) -> Option<String> {
    ["id", "Id", "itemId", "entryId", "taskId", "messageId"].iter().find_map(|key| {
        let id = match &value[*key] { Value::String(id) => id.clone(), Value::Number(n) => n.to_string(), _ => return None };
        crate::assessment::valid_id(&id).then_some(id)
    })
}

fn ids(value: &Value, depth: usize, found: &mut Vec<String>) {
    if depth > 4 || found.len() >= 100 { return; }
    match value {
        Value::Array(items) => for item in items {
            if let Some(id) = item_id(item) { if !found.contains(&id) { found.push(id); } }
            else { ids(item, depth + 1, found); }
        },
        Value::Object(object) => for (key, value) in object {
            if key != "urls" && !key.starts_with("linked") { ids(value, depth + 1, found); }
        },
        _ => {}
    }
}

fn not_available(app: &str, child: &str) -> Value {
    json!({"available": false, "app": app, "childId": redact_identity(child), "message": "Not available for this pupil/school: the app or requested read endpoint is not enabled."})
}

fn objective_links(value: &Value, depth: usize, found: &mut Vec<String>) {
    if depth > 8 || found.len() >= 100 { return; }
    match value {
        Value::Object(object) => for (key, value) in object {
            if ["linkedUols", "linkedLearningObjectives", "linkedObjectives"].contains(&key.as_str()) {
                if let Some(items) = value.as_array() {
                    for item in items.iter().take(100) {
                        let id = item_id(item).or_else(|| match item { Value::String(id) if crate::assessment::valid_id(id) => Some(id.clone()), Value::Number(n) => Some(n.to_string()), _ => None });
                        if let Some(id) = id && !found.contains(&id) && found.len() < 100 { found.push(id); }
                    }
                }
            } else if key != "urls" { objective_links(value, depth + 1, found); }
        },
        Value::Array(items) => for item in items { objective_links(item, depth + 1, found); },
        _ => {}
    }
}

fn full_objectives(http: &mut Http, signal: &Signal, app: &Value, value: &Value, budget: &mut usize) -> Result<Vec<Value>> {
    let mut ids = Vec::new(); objective_links(value, 0, &mut ids);
    let mut details = Vec::new();
    for id in ids {
        if *budget == 0 { break; } *budget -= 1;
        if let Some(key) = find_key(app, &["getUol"]) {
            details.push(json!({"id": id, "detail": fetch(http, signal, app, key, Some(&id), &Value::Null)?}));
        } else { details.push(json!({"id": id, "available": false})); }
    }
    Ok(details)
}

pub fn read(http: &mut Http, signal: &Signal, request: &Request) -> Result<Value> {
    let parent = selected(http, signal, request.child.as_deref())?;
    let child = parent.pupils.iter().find(|pupil| pupil.selected).ok_or_else(changed)?;
    let choices: &[&str] = match request.operation {
        "infomentor_get_calendar" | "infomentor_get_calendar_entry" => &["calendarv2"],
        "infomentor_get_homework" | "infomentor_get_homework_item" => &["homework"],
        "infomentor_get_assignments" | "infomentor_get_assignment" => &["task"],
        "infomentor_get_news" => &["communication", "grouptimeline"],
        _ => &[],
    };
    let app_name = match request.app.as_deref().or_else(|| choices.iter().find(|app| enabled(&parent, app)).copied()) {
        Some(app) => app,
        None => return Ok(not_available(choices.first().copied().unwrap_or("school data"), &child.id)),
    };
    if !enabled(&parent, app_name) { return Ok(not_available(app_name, &child.id)); }
    let app = match load(http, signal, app_name) {
        outcome => outcome?,
    };
    let mut body = request.body.clone();
    let key = if request.operation == "infomentor_app_data" { None }
        else if let Some(key) = request.key.as_deref() { Some(key) }
        else {
            let candidates: &[&str] = match request.operation {
                "infomentor_get_calendar" => &["getEntries"],
                "infomentor_get_calendar_entry" => &["getEntry", "getById", "itemUrl"],
                "infomentor_get_homework" => &["getMessages", "getMoreItems", "list"],
                "infomentor_get_homework_item" => &["getMessage", "getById", "itemUrl"],
                "infomentor_get_assignments" => &["getTasks"],
                "infomentor_get_assignment" => &["getTask", "getById", "itemUrl"],
                "infomentor_get_news" => &["getNewsList", "list", "getMoreItems"],
                _ => &[],
            };
            find_key(&app, candidates)
        };
    if request.operation != "infomentor_app_data" && key.is_none() {
        if app_name == "communication" && enabled(&parent, "grouptimeline") {
            let mut fallback = request.clone(); fallback.app = Some("grouptimeline".to_owned()); return read(http, signal, &fallback);
        }
        return Ok(not_available(app_name, &child.id));
    }
    if request.operation == "infomentor_get_calendar" {
        body = json!({"startDate": request.from, "endDate": request.to});
    } else if request.operation == "infomentor_get_assignments" {
        body = json!({});
    } else if request.id.is_none() && !matches!(request.operation, "infomentor_app_data" | "infomentor_app_get") {
        body = json!({"page": request.page, "pageSize": 100});
    }
    let mut data = match key { Some(key) => fetch(http, signal, &app, key, request.id.as_deref(), &body)?, None => app.clone() };
    let mut full = data.clone();
    if !matches!(request.operation, "infomentor_app_data" | "infomentor_app_get") {
        let candidates: &[&str] = match app_name { "calendar" | "calendarv2" => &["getEntry", "getById", "itemUrl"], "homework" => &["getMessage", "getById", "itemUrl"], "task" => &["getTask", "getById", "itemUrl"], _ => &["getById", "itemUrl"] };
        let mut details = Vec::new();
        let mut incomplete = false;
        let mut link_budget = 100;
        if request.id.is_none() && let Some(detail_key) = find_key(&app, candidates) {
            let mut found = Vec::new(); ids(&data, 0, &mut found);
            incomplete = found.len() >= 100;
            for id in found {
                signal.check()?;
                let detail = fetch(http, signal, &app, detail_key, Some(&id), &Value::Null)?;
                let linked = full_objectives(http, signal, &app, &detail, &mut link_budget)?;
                details.push(json!({"id": id, "detail": detail, "linkedObjectives": linked}));
            }
        }
        let attachments = if request.id.is_some() && let Some(key) = find_key(&app, &["getAttachments"]) {
            Some(fetch(http, signal, &app, key, request.id.as_deref(), &Value::Null)?)
        } else { None };
        let linked = if request.id.is_some() { full_objectives(http, signal, &app, &data, &mut link_budget)? } else { Vec::new() };
        full = json!({"initial": data, "details": details, "linkedObjectives": linked, "attachments": attachments, "detailsIncomplete": incomplete || link_budget == 0});
    }
    let mut result = bounded(&full, request.max_chars);
    result["available"] = json!(true); result["app"] = json!(app_name); result["childId"] = json!(redact_identity(&child.id));
    result["request"] = json!({"url_key": key, "id": request.id.as_deref().map(redact_identity), "page": request.page});
    if full["detailsIncomplete"] == true {
        result["truncated"] = json!(true);
        result["next"] = json!("At most 100 item IDs were expanded. Read remaining IDs individually using the detail tool or app_get.");
    }
    if request.operation == "infomentor_app_data" {
        let mut paths = Map::new();
        let mut urls_truncated = false;
        if let Some(urls) = app["urls"].as_object() {
            for (key, value) in urls.iter().take(100) {
                paths.insert(js::slice_units(key, 128).to_owned(), json!({"path": value.as_str().and_then(safe_link), "readable": resolve_url(&app["urls"], key, None).is_ok()}));
                if js::units(&json!(paths).to_string()) > (request.max_chars / 3).min(15_000) {
                    paths.remove(js::slice_units(key, 128)); urls_truncated = true; break;
                }
            }
            urls_truncated |= urls.len() > 100;
        }
        let remaining = request.max_chars.saturating_sub(js::units(&json!(paths).to_string()) + 500).max(100);
        result = bounded(&full, remaining);
        result["available"] = json!(true); result["app"] = json!(app_name); result["childId"] = json!(redact_identity(&child.id));
        result["max_chars"] = json!(request.max_chars); result["urlsTruncated"] = json!(urls_truncated);
        if urls_truncated { result["truncated"] = json!(true); result["next"] = json!("Increase max_chars to see additional discovery paths; app_get accepts advertised read keys directly."); }
        let mut first = Map::new(); first.insert("urls".to_owned(), json!(paths));
        if let Value::Object(rest) = result { first.extend(rest); }
        result = Value::Object(first);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forbidden_keys_and_dynamic_urls_cannot_turn_reads_into_writes() {
        for key in ["sendMessage", "externalUpdateUrl", "getAndMarkRead", "save", "set", "post", "upload", "reply", "create", "remove", "delete"] { assert!(!allowed_key(key)); }
        let urls = json!({"getEntry": "/calendarv2/calendarv2/GetEntry", "getMessage": "https://evil.test/get", "itemUrl": "/task/task/Save"});
        assert_eq!(resolve_url(&urls, "getEntry", Some("12")).unwrap(), "calendarv2/calendarv2/GetEntry/12");
        for key in ["getMessage", "itemUrl", "missing", "sendMessage"] { assert!(resolve_url(&urls, key, None).is_err()); }
        assert!(!safe_body(&json!({"filter": {"markRead": true}}), 0));
    }
    #[test]
    fn communication_news_document_and_link_lists_are_allowlisted_read_paths() {
        let urls = json!({
            "getNewsList": "/Communication/News/GetNewsList",
            "getDocumentsList": "/Communication/Documents/GetDocumentsList",
            "getLinksList": "/Communication/Links/GetLinksList",
            "bad": "/Communication/News/SaveNewsList"
        });
        for key in ["getNewsList", "getDocumentsList", "getLinksList"] {
            assert!(resolve_url(&urls, key, None).is_ok(), "{key}");
        }
        assert!(resolve_url(&urls, "bad", None).is_err());
    }
    #[test]
    fn full_text_metadata_and_identity_are_sanitized_with_explicit_truncation() {
        assert_eq!(html::plain_text("<p>Hello &amp; world</p><ul><li>One</li><li><a href='/docs?a=secret'>Notes</a></li></ul><script>secret</script>"), "Hello & world\n- One\n- Notes (https://minn.infomentor.is/docs)");
        let out = bounded(&json!({"body": "<p>Full teacher comment</p>", "kennitala": "0101991239", "note": "Student 0101991239", "attachments": [{"name": "Notes.pdf", "size": 42, "downloadUrl": "/docs/42?token=secret"}], "page": 2, "nextPage": 3}), 60_000);
        assert!(!out.to_string().contains("secret")); assert!(!out.to_string().contains("0101991239"));
        assert_eq!(out["data"]["body"], "Full teacher comment");
        let out = bounded(&json!({"text": "x".repeat(80_000), "nextPage": 3}), 1000);
        assert_eq!(out["truncated"], true); assert_eq!(out["paging"]["nextPage"], 3);
        assert!(out["json"].as_str().unwrap().len() <= 1000); assert!(out["next"].is_string());
    }
}
