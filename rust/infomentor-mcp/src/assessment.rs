//! Read-only assessmentv2 endpoints. Upstream URLs are never treated as arbitrary actions.

use serde_json::{Map, Value, json};
use url::Url;

use crate::error::{Code, Fail, Result};
use crate::http::{Http, Parent};
use crate::input::{AssessmentItemRequest, AssessmentsRequest};
use crate::js;
use crate::session::{PARENT_URL, origin, trusted_url};
use crate::signal::Signal;

const MAX_UNITS: usize = 40_000;
const APP_DATA: &str = "assessmentv2/assessmentv2/appData";
const LISTS: [&str; 4] = [
    "gridItemsList", "uolItemsList", "selfAssessmentItemsList", "taskItemsList",
];

pub const KINDS: [&str; 5] = ["grid", "task", "uol", "learning_step", "self_assessment"];

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn unavailable() -> Fail {
    Fail::new(Code::UnexpectedPage, "InfoMentor assessment data is unavailable or its format has changed.")
}

/// Validate an advertised read URL against the exact controller and action, before any POST.
fn read_path(value: &Value, action: &str, id: Option<&str>) -> Result<String> {
    let link = value.as_str().ok_or_else(unavailable)?;
    let url = Url::parse(PARENT_URL).and_then(|base| base.join(link)).map_err(|_| unavailable())?;
    let url = trusted_url(url.as_str()).map_err(|_| unavailable())?;
    let expected = format!("/assessmentv2/assessmentv2/{action}");
    if origin(&url) != "https://minn.infomentor.is"
        || !url.path().trim_end_matches('/').eq_ignore_ascii_case(&expected)
        || url.query().is_some() || url.fragment().is_some()
    {
        return Err(unavailable());
    }
    let mut path = url.path().trim_matches('/').to_owned();
    if let Some(id) = id {
        if !valid_id(id) { return Err(unavailable()); }
        path.push('/');
        path.push_str(id);
    }
    Ok(path)
}

/// An unknown object/array is retained as sanitized JSON, never guessed to be a known shape.
fn response(value: &Value) -> Option<Value> {
    matches!(value, Value::Object(_) | Value::Array(_)).then(|| value.clone())
}

fn known_overview(value: &Value) -> bool {
    let Some(object) = value.as_object() else { return false; };
    let required = ["gridItemsList", "uolItemsList", "selfAssessmentItemsList"];
    required.iter().all(|key| object.get(*key).is_some_and(|list| {
        list.as_array().is_some_and(|items| items.iter().all(Value::is_object))
    })) && LISTS.iter().all(|key| object.get(*key).is_none_or(|list| {
        list.as_array().is_some_and(|items| items.iter().all(Value::is_object))
    })) && object.get("urls").is_none_or(Value::is_object)
}

fn sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["token", "password", "cookie", "secret", "credential", "authorization", "csrf", "viewstate", "eventvalidation", "username", "login"]
        .iter().any(|part| lower.contains(part))
        || lower == "urls" || lower.ends_with("url") || lower == "account"
}

/// Strip HTML attributes and URL-bearing text. Output remains untrusted school content.
fn clean_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if ["oauth_token", "access_token", "password=", "authorization:", "__viewstate"]
        .iter().any(|marker| lower.contains(marker))
    { return "[redacted]".to_owned(); }
    let mut output = String::new();
    let mut tag = false;
    for c in text.chars() {
        match c {
            '<' => { tag = true; output.push(' '); }
            '>' if tag => tag = false,
            _ if !tag => output.push(c),
            _ => {}
        }
    }
    output.split_whitespace().map(|word| {
        if word.contains("https://") || word.contains("http://") || word.contains("?token=") {
            "[link omitted]"
        } else { word }
    }).collect::<Vec<_>>().join(" ")
}

fn sanitize(value: &Value, depth: usize, nodes: &mut usize, truncated: &mut bool) -> Value {
    if depth > 24 || *nodes >= 5000 {
        *truncated = true;
        return Value::Null;
    }
    *nodes += 1;
    match value {
        Value::Object(object) => {
            let mut safe = Map::new();
            for (key, value) in object {
                if sensitive_key(key) { continue; }
                if *nodes >= 5000 { *truncated = true; break; }
                safe.insert(clean_text(js::slice_units(key, 128)), sanitize(value, depth + 1, nodes, truncated));
            }
            Value::Object(safe)
        }
        Value::Array(items) => {
            let mut safe = Vec::new();
            for item in items {
                if *nodes >= 5000 { *truncated = true; break; }
                safe.push(sanitize(item, depth + 1, nodes, truncated));
            }
            Value::Array(safe)
        }
        Value::String(text) => {
            if js::units(text) > MAX_UNITS { *truncated = true; }
            Value::String(clean_text(js::slice_units(text, MAX_UNITS)))
        }
        _ => value.clone(),
    }
}

fn output(value: &Value, child: &str, known: bool) -> Value {
    let mut truncated = false;
    let safe = sanitize(value, 0, &mut 0, &mut truncated);
    let serialized = safe.to_string();
    let oversized = js::units(&serialized) > MAX_UNITS;
    json!({
        "childId": child,
        "schemaKnown": known,
        "data": if oversized { Value::Null } else { safe },
        "json": if oversized { Some(js::slice_units(&serialized, MAX_UNITS)) } else { None },
        "truncated": truncated || oversized,
        "retrievedAt": js::iso_string(js::now_ms()),
    })
}

fn selected(http: &mut Http, signal: &Signal, child: Option<&str>) -> Result<Parent> {
    let parent = match (child, &http.parent) {
        (None, Some(parent)) => parent.clone(),
        _ => http.read_parent(signal, child)?,
    };
    if !parent.apps.iter().any(|app| app == "assessmentv2") {
        return Err(Fail::new(Code::UnexpectedPage, "Assessment is not available for the selected child."));
    }
    if parent.pupils.iter().filter(|pupil| pupil.selected).count() != 1 {
        return Err(unavailable());
    }
    Ok(parent)
}

pub fn overview(http: &mut Http, signal: &Signal, input: &AssessmentsRequest) -> Result<Value> {
    let parent = selected(http, signal, input.child_id.as_deref())?;
    let child = parent.pupils.iter().find(|pupil| pupil.selected).ok_or_else(unavailable)?;
    let app = http.read_app_data(APP_DATA, &[], signal, response)?;
    let data = if let Some(year) = &input.academic_year_id {
        let path = read_path(&app["urls"]["getNavigationData"], "getNavigationData", None)?;
        http.read_app_data(&path, &[("academicYearId", year.clone())], signal, response)?
    } else { app };
    let mut result = output(&data, &child.id, known_overview(&data));
    result["academicYearId"] = json!(input.academic_year_id);
    Ok(result)
}

pub fn item(http: &mut Http, signal: &Signal, input: &AssessmentItemRequest) -> Result<Value> {
    let parent = selected(http, signal, input.child_id.as_deref())?;
    let child = parent.pupils.iter().find(|pupil| pupil.selected).ok_or_else(unavailable)?;
    let app = http.read_app_data(APP_DATA, &[], signal, response)?;
    let action = match input.kind {
        "grid" => "getGridItems", "task" => "getTaskItems", "uol" => "getUolItems",
        "learning_step" => "getLearningStepItems", "self_assessment" => "getSelfAssessmentItems",
        _ => return Err(unavailable()),
    };
    let path = read_path(&app["urls"][action], action, Some(&input.id))?;
    let data = http.read_app_data(&path, &[], signal, response)?;
    // The detailed model is not documented. Preserve it without inventing grade semantics.
    let mut result = output(&data, &child.id, false);
    result["kind"] = json!(input.kind);
    result["id"] = json!(input.id);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_read_actions_and_safe_ids_can_be_posted() {
        assert_eq!(read_path(&json!("/assessmentv2/assessmentv2/getGridItems/"), "getGridItems", Some("12")), Ok("assessmentv2/assessmentv2/getGridItems/12".to_owned()));
        for link in ["https://evil.test/assessmentv2/assessmentv2/getGridItems", "https://im1.infomentor.is/assessmentv2/assessmentv2/getGridItems", "/assessmentv2/assessmentv2/save", "/assessmentv2/assessmentv2/getGridItems?token=private", "/assessmentv2/assessmentv2/getGridItems#private", "https://user:secret@minn.infomentor.is/assessmentv2/assessmentv2/getGridItems"] {
            assert!(read_path(&json!(link), "getGridItems", Some("12")).is_err());
        }
        for id in ["../save", "1?x", "1/2", "", "%2f", "https://evil.test"] { assert!(!valid_id(id)); }
        for id in ["42", "a_b-c", "d68004b8-45d5-4d0e-bd72-eabeb37a238c"] { assert!(valid_id(id)); }
    }

    #[test]
    fn shapes_are_checked_and_unknown_json_is_sanitized_and_bounded() {
        assert!(known_overview(&json!({"gridItemsList": [], "uolItemsList": [], "selfAssessmentItemsList": []})));
        assert!(!known_overview(&json!({"gridItemsList": "unexpected"})));
        assert!(response(&json!(true)).is_none());
        let out = output(&json!({"unfamiliar": {"teacherComment": "<p token='private'>Good progress</p>", "oauth_token": "private", "cookies": ["private"], "itemUrl": "https://minn.infomentor.is/?secret=private"}, "linkedGrids": [{"id": 5}]}), "child", false);
        assert!(!out.to_string().contains("private"));
        assert_eq!(out["data"]["unfamiliar"]["teacherComment"], "Good progress");
        assert_eq!(out["data"]["linkedGrids"][0]["id"], 5);
        let out = output(&json!({"comment": "é".repeat(100_000)}), "child", false);
        assert_eq!(out["truncated"], true);
        assert!(js::units(out["json"].as_str().unwrap()) <= MAX_UNITS);
        let out = output(&json!((0..10_000).collect::<Vec<_>>()), "child", false);
        assert_eq!(out["truncated"], true);
    }
}
