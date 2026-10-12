//! Read-only assessmentv2 data and one level of linked assessment content.
use std::collections::HashSet;
use serde_json::{Value, json};
use crate::error::{Code, Fail, Result};
use crate::http::Http;
use crate::input::{AssessmentItemRequest, AssessmentsRequest};
use crate::signal::Signal;
use crate::school;

pub const KINDS: [&str; 5] = ["grid", "task", "uol", "learning_step", "self_assessment"];

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn response(value: &Value) -> Option<Value> {
    (value.is_object() || value.is_array()).then(|| value.clone())
}

fn known_overview(value: &Value) -> bool {
    ["gridItemsList", "uolItemsList", "selfAssessmentItemsList"].iter().all(|key| {
        value[*key].as_array().is_some_and(|items| items.iter().all(Value::is_object))
    })
}

fn action(kind: &str) -> Result<&'static str> {
    match kind {
        "grid" => Ok("getGridItems"), "task" => Ok("getTaskItems"), "uol" => Ok("getUolItems"),
        "learning_step" => Ok("getLearningStepItems"), "self_assessment" => Ok("getSelfAssessmentItems"),
        _ => Err(Fail::new(Code::UnexpectedPage, "Unsupported assessment item kind.")),
    }
}

fn unavailable() -> Value {
    json!({"available": false, "message": "Assessments are not available for this pupil/school."})
}

pub fn overview(http: &mut Http, signal: &Signal, input: &AssessmentsRequest) -> Result<Value> {
    let parent = school::selected(http, signal, input.child_id.as_deref())?;
    if !parent.apps.iter().any(|app| app == "assessmentv2") { return Ok(unavailable()); }
    let child = parent.pupils.iter().find(|pupil| pupil.selected).ok_or(Fail::Unknown)?;
    let app = http.read_app_data("assessmentv2/assessmentv2/appData", &[], signal, response)?;
    let data = if let Some(year) = &input.academic_year_id {
        let key = school::find_key(&app, &["getNavigationData"]).ok_or(Fail::new(Code::UnexpectedPage, "Assessment year navigation is unavailable."))?;
        school::fetch(http, signal, &app, key, None, &json!({"academicYearId": year}))?
    } else { app };
    let mut result = school::bounded(&data, input.max_chars);
    result["available"] = json!(true);
    result["childId"] = json!(school::redact_identity(&child.id));
    result["academicYearId"] = json!(input.academic_year_id);
    result["schemaKnown"] = json!(known_overview(&data));
    Ok(result)
}

fn linked(value: &Value, depth: usize, found: &mut Vec<(&'static str, String)>) {
    if depth > 8 || found.len() >= 100 { return; }
    match value {
        Value::Array(items) => for item in items { linked(item, depth + 1, found); },
        Value::Object(object) => for (key, value) in object {
            let kind = match key.as_str() {
                "linkedGrids" => Some("grid"), "linkedTasks" => Some("task"),
                "linkedUols" => Some("uol"), "linkedLearningSteps" => Some("learning_step"),
                "linkedSelfAssessments" => Some("self_assessment"), _ => None,
            };
            if let Some(kind) = kind {
                if let Some(items) = value.as_array() {
                    for item in items {
                        let id = school::item_id(item).or_else(|| {
                            let id = match item { Value::String(id) => id.clone(), Value::Number(n) => n.to_string(), _ => return None };
                            valid_id(&id).then_some(id)
                        });
                        if let Some(id) = id && found.len() < 100 { found.push((kind, id)); }
                    }
                }
            } else if key != "urls" { linked(value, depth + 1, found); }
        },
        _ => {}
    }
}

pub fn item(http: &mut Http, signal: &Signal, input: &AssessmentItemRequest) -> Result<Value> {
    let parent = school::selected(http, signal, input.child_id.as_deref())?;
    if !parent.apps.iter().any(|app| app == "assessmentv2") { return Ok(unavailable()); }
    let child = parent.pupils.iter().find(|pupil| pupil.selected).ok_or(Fail::Unknown)?;
    let app = http.read_app_data("assessmentv2/assessmentv2/appData", &[], signal, response)?;
    let requested = action(input.kind)?;
    let key = school::find_key(&app, &[requested]).ok_or(Fail::new(Code::UnexpectedPage, "Assessment item details are unavailable."))?;
    let detail = school::fetch(http, signal, &app, key, Some(&input.id), &Value::Null)?;
    let mut found = Vec::new(); linked(&detail, 0, &mut found);
    let incomplete = found.len() >= 100;
    let mut visited = HashSet::new(); visited.insert((input.kind, input.id.clone()));
    let mut linked_items = Vec::new();
    for (kind, id) in found {
        if !visited.insert((kind, id.clone())) { continue; }
        let requested = action(kind)?;
        let Some(key) = school::find_key(&app, &[requested]) else {
            linked_items.push(json!({"kind": kind, "id": id, "available": false})); continue;
        };
        linked_items.push(json!({"kind": kind, "id": id, "detail": school::fetch(http, signal, &app, key, Some(&id), &Value::Null)?}));
    }
    let data = json!({"item": detail, "linkedItems": linked_items, "linkedItemsIncomplete": incomplete});
    let mut result = school::bounded(&data, input.max_chars);
    result["available"] = json!(true); result["childId"] = json!(school::redact_identity(&child.id));
    result["kind"] = json!(input.kind); result["id"] = json!(school::redact_identity(&input.id));
    result["schemaKnown"] = json!(false);
    if incomplete { result["truncated"] = json!(true); result["next"] = json!("Read further linked item IDs individually; only one level and at most 100 references are resolved."); }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_safe_identifiers_and_known_read_kinds_are_allowed() {
        for id in ["../save", "1?x", "1/2", "", "%2f", "https://evil.test"] { assert!(!valid_id(id)); }
        for kind in KINDS { assert!(school::allowed_key(action(kind).unwrap())); }
        assert!(action("save").is_err());
    }
    #[test]
    fn shapes_and_linked_assessment_kinds_are_checked_without_guessing() {
        assert!(known_overview(&json!({"gridItemsList": [], "uolItemsList": [], "selfAssessmentItemsList": []})));
        assert!(!known_overview(&json!({"gridItemsList": "unexpected"})));
        assert!(response(&json!(true)).is_none());
        let mut found = Vec::new();
        linked(&json!({"linkedGrids": [{"id": 4}], "linkedTasks": [5], "linkedUols": ["uol-1"], "linkedLearningSteps": [{"id": "step-1"}], "linkedSelfAssessments": [6]}), 0, &mut found);
        assert_eq!(found, [("grid", "4".to_owned()), ("task", "5".to_owned()), ("uol", "uol-1".to_owned()), ("learning_step", "step-1".to_owned()), ("self_assessment", "6".to_owned())]);
    }
}
