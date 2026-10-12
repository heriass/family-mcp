//! Schema metadata only. No account fields, mutation arguments, defaults or headers are returned.
use std::collections::{BTreeMap, BTreeSet};
use serde_json::{Map, Value, json};
use crate::error::{Fail, Result};

pub const PROBES: [&str; 28] = ["SetAttendance", "UpdateAttendance", "RegisterAttendance", "AttendEvent", "RespondToEvent", "AcceptEvent", "DeclineEvent", "JoinEvent", "LeaveEvent", "SetParticipation", "UpdateParticipation", "Rsvp", "ReportAbsence", "RegisterAbsence", "SignUp", "Register", "Enroll", "AnswerSurvey", "SubmitSurvey", "SubmitForm", "SendMessage", "CreateConversation", "StartConversation", "ReactToMessage", "CreatePost", "CommentPost", "LikePost", "UpdateNotificationSettings"];
pub const QUERY_PROBES: [&str; 17] = ["feed", "posts", "news", "announcements", "notifications", "surveys", "forms", "attendance", "statistics", "documents", "files", "payments", "invoices", "registrations", "products", "events", "eventDetails"];

#[derive(Clone, Copy)]
pub enum Root { Mutation, Query }
impl Root {
    pub fn label(self) -> &'static str { match self { Self::Mutation => "Mutation", Self::Query => "Query" } }
    pub fn operation(self) -> &'static str { match self { Self::Mutation => "mutation", Self::Query => "query" } }
}

#[derive(Clone)]
pub struct Request { pub filter: String, pub max_chars: usize, pub root: Root, pub probes: Vec<String>, pub describe_fields: Vec<String>, pub explicit_probes: bool }

pub fn input(args: &Map<String, Value>) -> std::result::Result<Request, String> {
    if args.keys().any(|key| !["filter", "max_chars", "root", "probes", "describe_fields"].contains(&key.as_str())) { return Err("Unrecognized discovery input field.".to_owned()); }
    let filter = match args.get("filter") {
        None => String::new(), Some(Value::String(filter)) if filter.chars().count() <= 256 => filter.to_lowercase(),
        _ => return Err("filter: expected a string of at most 256 characters.".to_owned()),
    };
    let max_chars = match args.get("max_chars") {
        None => 60_000,
        Some(value) if value.as_f64().is_some_and(|n| n.fract() == 0.0 && (1000.0..=200_000.0).contains(&n)) => value.as_f64().unwrap_or(60_000.0) as usize,
        _ => return Err("max_chars: expected an integer from 1000 to 200000.".to_owned()),
    };
    let root = match args.get("root") {
        None => Root::Mutation, Some(Value::String(value)) if value == "Mutation" => Root::Mutation,
        Some(Value::String(value)) if value == "Query" => Root::Query,
        _ => return Err("root: expected Mutation or Query.".to_owned()),
    };
    let names = |key: &str, maximum: usize| -> std::result::Result<Option<Vec<String>>, String> {
        match args.get(key) {
            None => Ok(None),
            Some(Value::Array(values)) if values.len() <= maximum => values.iter().map(|value| value.as_str().filter(|value| probe_name(value)).map(str::to_owned).ok_or_else(|| format!("{key}: names must match ^[A-Za-z_][A-Za-z0-9_]{{0,63}}$."))).collect::<std::result::Result<Vec<_>, _>>().map(Some),
            _ => Err(format!("{key}: expected at most {maximum} field names.")),
        }
    };
    let probes = names("probes", 60)?.unwrap_or_else(|| match root { Root::Mutation => PROBES.to_vec(), Root::Query => QUERY_PROBES.to_vec() }.into_iter().map(str::to_owned).collect());
    Ok(Request { filter, max_chars, root, probes, describe_fields: names("describe_fields", 20)?.unwrap_or_default(), explicit_probes: args.contains_key("probes") })
}

pub fn probe_name(text: &str) -> bool { text.len() <= 64 && name(text) }

pub fn name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256
        && text.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn introspection_query() -> String {
    let mut reference = "kind name".to_owned();
    for _ in 0..16 { reference = format!("kind name ofType {{ {reference} }}"); }
    format!("query DescribeApi {{ __schema {{ queryType {{ name }} mutationType {{ name }} types {{ kind name fields(includeDeprecated: true) {{ name description args {{ name type {{ ...TypeRef }} }} type {{ ...TypeRef }} }} inputFields {{ name type {{ ...TypeRef }} }} enumValues(includeDeprecated: true) {{ name }} }} }} }} fragment TypeRef on __Type {{ {reference} }}")
}

/// Both guards matter: reserved __ names cannot be custom schema fields, so validation rejects
/// the whole document before execution. @skip(true) additionally prevents a candidate resolver
/// from running even if the candidate exists. No user input is interpolated in these probes.
/// https://spec.graphql.org/October2021/#sec-Validating-Requests
#[cfg(test)]
pub fn probe_query(operation: &str, field: &str) -> String {
    document(operation, Root::Mutation, field, false)
}

pub fn document(operation: &str, root: Root, field: &str, empty_input: bool) -> String {
    let args = if empty_input { "(input: {})" } else { "" };
    format!("{} {operation} {{ __describe_api_invalid {field}{args} @skip(if: true) }}", root.operation())
}

pub fn cookie_values(serialized: &Value) -> Vec<String> {
    serialized["cookies"].as_array().into_iter().flatten()
        .filter_map(|cookie| cookie["value"].as_str().filter(|value| !value.is_empty()).map(str::to_owned)).collect()
}

pub fn safe_text(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) { text = text.replace(secret, "[redacted]"); }
    let lower = text.to_ascii_lowercase();
    if ["authorization:", "cookie:", "bearer ", "password=", "access_token=", "refresh_token="].iter().any(|marker| lower.contains(marker)) {
        return "[private diagnostic omitted]".to_owned();
    }
    let mut out = String::new();
    let mut digits = String::new();
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() { digits.push(c); continue; }
        if digits.len() == 10 { out.push_str("[redacted]"); } else { out.push_str(&digits); }
        digits.clear(); if c != '\0' { out.push(c); }
    }
    out.split_inclusive(char::is_whitespace).map(|word| {
        if word.contains('@') && word.contains('.') { "[private contact omitted] ".to_owned() }
        else if word.contains("https://") || word.contains("http://") { "[link omitted] ".to_owned() }
        else { word.to_owned() }
    }).collect()
}

fn quoted_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[at]; rest = &rest[at + 1..];
        let Some(end) = rest.find(char::from(quote)) else { break; };
        if name(&rest[..end]) { names.push(rest[..end].to_owned()); }
        rest = &rest[end + 1..];
    }
    names
}

pub fn suggestions(message: &str) -> Vec<String> {
    let lower = message.to_ascii_lowercase();
    lower.find("did you mean").map(|at| quoted_names(&message[at..])).unwrap_or_default()
}

pub fn validation_root(messages: &[String]) -> Option<String> {
    messages.iter().find_map(|message| {
        if !message.to_ascii_lowercase().starts_with("cannot query field") { return None; }
        quoted_names(message).get(1).filter(|root| name(root)).cloned()
    })
}

fn quoted_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new(); let mut rest = text;
    while let Some(at) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[at]; rest = &rest[at + 1..];
        let Some(end) = rest.find(char::from(quote)) else { break; };
        if end <= 256 { tokens.push(rest[..end].to_owned()); }
        rest = &rest[end + 1..];
    }
    tokens
}

fn validation_type(text: &str) -> bool {
    fn parse(bytes: &[u8], at: &mut usize, depth: usize) -> bool {
        if depth > 16 { return false; }
        if bytes.get(*at) == Some(&b'[') {
            *at += 1;
            if !parse(bytes, at, depth + 1) || bytes.get(*at) != Some(&b']') { return false; }
            *at += 1;
        } else {
            let start = *at;
            while bytes.get(*at).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') { *at += 1; }
            if start == *at || !(bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') { return false; }
        }
        if bytes.get(*at) == Some(&b'!') { *at += 1; }
        true
    }
    let mut at = 0;
    text.len() <= 128 && parse(text.as_bytes(), &mut at, 0) && at == text.len()
}

/// Reconstruct only documented GraphQL validation metadata; never echo the server body.
pub fn argument_message(message: &str) -> Option<Value> {
    let lower = message.to_ascii_lowercase(); let tokens = quoted_tokens(message);
    let field = tokens.first()?;
    if !field.split('.').all(probe_name) { return None; }
    if lower.starts_with("field ") && lower.contains(" argument ") && lower.contains("is required") && tokens.len() >= 3 {
        if !probe_name(&tokens[1]) || !validation_type(&tokens[2]) { return None; }
        return Some(json!({"kind":"argument_required", "field":field, "name":tokens[1], "type":tokens[2]}));
    }
    if lower.starts_with("field ") && lower.contains("of required type") && tokens.len() >= 2 && validation_type(&tokens[1]) {
        return Some(json!({"kind":"input_field_required", "field":field, "name":field.rsplit('.').next(), "type":tokens[1]}));
    }
    if lower.starts_with("field ") && (lower.contains("must have a selection") || lower.contains("must not have a selection")) {
        return Some(json!({"kind":if lower.contains("must not") {"selection_forbidden"} else {"selection_required"}, "field":field}));
    }
    None
}

pub fn reported_input(messages: &[String]) -> bool {
    messages.iter().filter_map(|message| argument_message(message)).any(|row| row["kind"] == "argument_required" && row["name"] == "input")
}

pub fn described(field: &str, root: Root, stages: Vec<Value>, messages: &[String], secrets: &[String]) -> Value {
    let parsed = messages.iter().filter_map(|message| argument_message(message)).collect::<Vec<_>>();
    let mut arguments = Vec::new(); let mut inputs = Vec::new(); let mut selection = None;
    for row in parsed {
        match row["kind"].as_str() {
            Some("argument_required") => if !arguments.contains(&row) { arguments.push(row); },
            Some("input_field_required") => if !inputs.contains(&row) { inputs.push(row); },
            Some("selection_required") => selection = Some(true), Some("selection_forbidden") => selection = Some(false), _ => {},
        }
    }
    let mut value = json!({"field":field, "root":root.label(), "stages":stages, "requiredArguments":arguments, "requiredInputFields":inputs, "selectionSetRequired":selection});
    scrub(&mut value, secrets); value
}

pub fn safe_error(message: &str, secrets: &[String]) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("introspection") { return "GraphQL introspection is disabled or not permitted.".to_owned(); }
    if let Some(mut row) = argument_message(message) { scrub(&mut row, secrets); return format!("GraphQL validation: {row}"); }
    if lower.starts_with("cannot query field") {
        let fields = quoted_names(message);
        let field = fields.first().map(|name| safe_text(name, secrets)).unwrap_or_else(|| "[unknown field]".to_owned());
        let root = fields.get(1).map(|name| safe_text(name, secrets)).unwrap_or_else(|| "[unknown type]".to_owned());
        let proposed = suggestions(message).iter().map(|name| safe_text(name, secrets)).collect::<Vec<_>>();
        return format!("Cannot query field {field:?} on type {root:?}.{}", if proposed.is_empty() { String::new() } else { format!(" Did you mean {}?", proposed.join(", ")) });
    }
    "GraphQL rejected schema discovery; private server diagnostics were omitted.".to_owned()
}

fn invalid() -> Fail { Fail::Safe("Abler returned an unsupported schema discovery response.") }

/// Render nested NON_NULL/LIST wrappers, retaining the named terminal kind and name.
fn type_ref(value: &Value, depth: usize) -> Result<(String, String, String)> {
    if depth > 16 { return Err(invalid()); }
    let kind = value["kind"].as_str().ok_or_else(invalid)?;
    match kind {
        "NON_NULL" | "LIST" => {
            if kind == "NON_NULL" && value["ofType"]["kind"] == "NON_NULL" { return Err(invalid()); }
            let (inner, leaf_kind, leaf_name) = type_ref(&value["ofType"], depth + 1)?;
            Ok((if kind == "LIST" { format!("[{inner}]") } else { format!("{inner}!") }, leaf_kind, leaf_name))
        },
        "SCALAR" | "OBJECT" | "INTERFACE" | "UNION" | "ENUM" | "INPUT_OBJECT" => {
            let text = value["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
            Ok((text.to_owned(), kind.to_owned(), text.to_owned()))
        },
        _ => Err(invalid()),
    }
}

fn fields(value: &Value, filter: &str, secrets: &[String], argument_types: &mut Vec<(String, String)>) -> Result<Vec<Value>> {
    let fields = value["fields"].as_array().ok_or_else(invalid)?;
    let mut rows = Vec::new();
    for field in fields {
        let field_name = field["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
        if !field_name.to_lowercase().contains(filter) { continue; }
        let description = match field.get("description") { None | Some(Value::Null) => None, Some(Value::String(text)) => Some(safe_text(text, secrets)), _ => return Err(invalid()) };
        let mut args = Vec::new();
        for arg in field["args"].as_array().ok_or_else(invalid)? {
            let arg_name = arg["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
            let (rendered, kind, leaf) = type_ref(&arg["type"], 0)?;
            argument_types.push((kind, leaf));
            args.push(json!({"name": safe_text(arg_name, secrets), "type": safe_text(&rendered, secrets)}));
        }
        let (rendered, _, leaf) = type_ref(&field["type"], 0)?;
        rows.push(json!({"name": safe_text(field_name, secrets), "description": description, "arguments": args, "type": safe_text(&rendered, secrets), "returnType": safe_text(&leaf, secrets)}));
    }
    Ok(rows)
}

pub fn schema(data: &Value, request: &Request, secrets: &[String]) -> Result<Value> {
    let schema = data["__schema"].as_object().ok_or_else(invalid)?;
    let all = schema.get("types").and_then(Value::as_array).ok_or_else(invalid)?;
    let mut types = BTreeMap::new();
    for ty in all {
        let type_name = ty["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
        if types.insert(type_name, ty).is_some() { return Err(invalid()); }
    }
    let query_name = schema["queryType"]["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
    let query_type = types.get(query_name).copied().ok_or_else(invalid)?;
    if query_type["kind"] != "OBJECT" { return Err(invalid()); }
    let query = fields(query_type, &request.filter, secrets, &mut Vec::new())?;
    let mutation_name = match schema.get("mutationType") { None | Some(Value::Null) => None, Some(value) => Some(value["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?), };
    let mut used = Vec::new();
    let mutation = match mutation_name {
        Some(root) => { let ty = types.get(root).copied().ok_or_else(invalid)?; if ty["kind"] != "OBJECT" { return Err(invalid()); } fields(ty, &request.filter, secrets, &mut used)? },
        None => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut inputs = Map::new(); let mut enums = Map::new();
    while let Some((kind, type_name)) = used.pop() {
        if !["INPUT_OBJECT", "ENUM"].contains(&kind.as_str()) || !seen.insert(type_name.clone()) { continue; }
        let ty = types.get(type_name.as_str()).copied().ok_or_else(invalid)?;
        if ty["kind"] != kind { return Err(invalid()); }
        if kind == "ENUM" {
            let values = ty["enumValues"].as_array().ok_or_else(invalid)?.iter().map(|value| {
                let value = value["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
                Ok(json!(safe_text(value, secrets)))
            }).collect::<Result<Vec<_>>>()?;
            enums.insert(safe_text(&type_name, secrets), json!(values));
        } else {
            let mut rows = Vec::new();
            for field in ty["inputFields"].as_array().ok_or_else(invalid)? {
                let field_name = field["name"].as_str().filter(|text| name(text)).ok_or_else(invalid)?;
                let (rendered, kind, leaf) = type_ref(&field["type"], 0)?; used.push((kind, leaf));
                rows.push(json!({"name": safe_text(field_name, secrets), "type": safe_text(&rendered, secrets)}));
            }
            inputs.insert(safe_text(&type_name, secrets), json!(rows));
        }
    }
    let output = json!({"introspection": true, "queryType": safe_text(query_name, secrets), "mutationType": mutation_name.map(|name| safe_text(name, secrets)), "query": query, "mutation": mutation, "inputObjects": inputs, "enums": enums, "filter": request.filter, "truncated": false});
    Ok(limit(output, request.max_chars))
}

fn chars(value: &Value) -> usize { value.to_string().chars().count() }

pub fn limit(mut value: Value, maximum: usize) -> Value {
    if chars(&value) <= maximum { return value; }
    let total = chars(&value);
    value["truncated"] = json!(true);
    value["note"] = json!("Schema discovery output was truncated. Use a narrower filter or increase max_chars (up to 200000); this is not a complete API inventory.");
    value["full_chars"] = json!(total);
    // Retain whole records, not invalid sliced JSON. Drop trailing type definitions first, then
    // trailing fields/probe records until the complete serialized response fits the requested cap.
    for key in ["inputObjects", "enums"] {
        while chars(&value) > maximum {
            let Some(object) = value[key].as_object_mut() else { break; };
            let Some(last) = object.keys().next_back().cloned() else { break; };
            object.shift_remove(&last);
        }
    }
    for key in ["mutation", "query", "probes", "suggestions", "errors", "describedFields"] {
        while chars(&value) > maximum {
            let Some(array) = value[key].as_array_mut() else { break; };
            if array.pop().is_none() { break; }
        }
    }
    value
}

pub fn fallback(errors: Vec<String>, mutation_type: Option<String>, probes: Vec<Value>, suggestions: Vec<String>, request: &Request, secrets: &[String]) -> Result<Value> {
    let suggestions = suggestions.into_iter().filter(|text| text.to_lowercase().contains(&request.filter)).collect::<BTreeSet<_>>();
    let mut value = json!({"introspection": false, "errors": errors.iter().map(|error| safe_text(error, secrets)).collect::<Vec<_>>(), "mutationType": mutation_type, "probes": probes, "suggestions": suggestions, "complete": false, "note": "Only server validation suggestions are available; this is not a complete mutation inventory. No mutation resolver was invoked.", "truncated": false});
    // Only controlled names and safe diagnostics enter probes; scrub rotated cookie values again.
    scrub(&mut value, secrets);
    Ok(limit(value, request.max_chars))
}

pub fn scrub(value: &mut Value, secrets: &[String]) {
    match value { Value::String(text) => *text = safe_text(text, secrets), Value::Array(items) => for item in items { scrub(item, secrets); }, Value::Object(object) => for value in object.values_mut() { scrub(value, secrets); }, _ => {} }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_names_limits_and_both_root_guards_are_validated() {
        for field in ["MarkAsRead", "_field", "field1", &"x".repeat(64)] { assert!(probe_name(field)); }
        for field in ["", "1field", "x(input: {})", "x } mutation { write", "x-y", &"x".repeat(65)] { assert!(!probe_name(field)); }
        assert!(input(json!({"probes":vec!["field";61]}).as_object().unwrap()).is_err());
        assert!(input(json!({"describe_fields":vec!["field";21]}).as_object().unwrap()).is_err());
        assert!(input(json!({"root":"Other"}).as_object().unwrap()).is_err());
        let defaults = input(&Map::new()).unwrap(); assert_eq!(defaults.probes.len(),28);
        let query = input(json!({"root":"Query"}).as_object().unwrap()).unwrap(); assert_eq!(query.probes.len(),17);
        for root in [Root::Mutation, Root::Query] {
            for empty in [false, true] {
                for field in ["MarkAsRead", "MessageReply", "__typename"] {
                    let query = document("DescribeApiTest", root, field, empty);
                    assert!(query.starts_with(root.operation())); assert!(query.contains("__describe_api_invalid"));
                    assert!(query.contains("@skip(if: true)")); assert!(!query.contains("{ __typename "));
                    assert_eq!(query.contains("(input: {})"),empty);
                }
            }
        }
    }
    #[test]
    fn argument_validation_metadata_is_parsed_and_private_diagnostics_are_redacted() {
        let required = "Field \"MessageReply\" argument \"input\" of type \"ReplyInput!\" is required, but it was not provided.";
        let nested = "Field \"text\" of required type \"String!\" was not provided.";
        assert_eq!(argument_message(required).unwrap()["type"],"ReplyInput!");
        assert!(reported_input(&[required.to_owned()]));
        assert_eq!(argument_message(nested).unwrap()["kind"],"input_field_required");
        let selected = "Field \"MessageReply\" must have a selection of subfields. Did you mean \"MessageReply { ... }\"?";
        assert_eq!(argument_message(selected).unwrap()["kind"],"selection_required");
        assert!(validation_type("[ID!]!")); assert!(!validation_type("ID!!")); assert!(!validation_type("String! bearer token"));
        let out = described("MessageReply",Root::Mutation,vec![],&[required.to_owned(),nested.to_owned(),selected.to_owned()],&[]);
        assert_eq!(out["selectionSetRequired"],true); assert_eq!(out["requiredArguments"][0]["name"],"input");
        let secret = vec!["MessageReply".to_owned()]; assert!(!safe_error(required,&secret).contains("MessageReply"));
        assert!(!safe_text("person@example.test 0101991239 https://example.test/?secret=token",&[]).contains("example.test"));
    }
    #[test]
    fn wrapper_types_render_and_probes_cannot_invoke_candidate_mutations() {
        let id = json!({"kind":"SCALAR","name":"ID"});
        let nonnull = json!({"kind":"NON_NULL","ofType":id});
        assert_eq!(type_ref(&json!({"kind":"LIST","ofType":nonnull}),0).unwrap().0,"[ID!]");
        assert_eq!(type_ref(&json!({"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}}),0).unwrap().0,"String!");
        for (i, field) in PROBES.iter().enumerate() {
            let probe = probe_query(&format!("DescribeApiProbe{i}"), field);
            assert!(probe.contains("__describe_api_invalid")); assert!(probe.contains(&format!("{field} @skip(if: true)")));
        }
    }
    #[test]
    fn diagnostics_and_bounds_never_return_private_values_or_invalid_json() {
        let secrets = vec!["private-cookie".to_owned()];
        assert!(!safe_text("A private-cookie value 0101991239 person@example.test",&secrets).contains("private-cookie"));
        assert!(!safe_error("Internal failure private-cookie",&secrets).contains("private-cookie"));
        assert_eq!(suggestions("Cannot query field \"mark\" on type \"Mutation\". Did you mean \"markRead\" or \"markSeen\"?"),["markRead","markSeen"]);
        let out = limit(json!({"introspection":true,"query":(0..100).map(|i|json!({"name":format!("field{i}"),"description":"x".repeat(100)})).collect::<Vec<_>>(),"mutation":[],"inputObjects":{},"enums":{},"truncated":false}),1000);
        assert_eq!(out["truncated"],true); assert!(chars(&out)<=1000); assert!(out["note"].is_string());
    }
}
