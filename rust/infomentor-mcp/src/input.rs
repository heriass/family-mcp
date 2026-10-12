//! Tool input validation with the TypeScript server's results: the same accepted values,
//! defaults, and zod 4 issue messages, order and abort rules, so an invalid call gets the same
//! text. The advertised JSON Schemas are the TypeScript server's own (`surface.json`).

pub use mcp_runtime::input::empty;
use mcp_runtime::input::{Parse, object, optional};
use serde_json::{Map, Value};

use crate::js::{SAFE, is_uuid};

/// `messagesRequestSchema`, defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub struct MessagesRequest {
    pub folder: &'static str,
    pub search: String,
    pub page: i64,
    pub page_size: i64,
}

/// `notificationsRequestSchema`, defaults applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotificationsRequest {
    pub selected_child_only: bool,
    pub include_cleared: bool,
}

/// `collectRequestSchema`, defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectRequest {
    pub cursor: Option<String>,
    pub include_existing: bool,
    pub max_message_pages: i64,
}

/// `loginRequestSchema`, defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginRequest {
    pub import_file: Option<String>,
    pub credentials_file: Option<String>,
    pub allow_account_change: Option<bool>,
    pub timeout_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssessmentsRequest {
    pub child_id: Option<String>,
    pub academic_year_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssessmentItemRequest {
    pub child_id: Option<String>,
    pub kind: &'static str,
    pub id: String,
}

fn assessment_id(parse: &mut Parse, value: Option<&Value>) -> Option<String> {
    let id = match value {
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n <= SAFE as u64) => Some(n.to_string()),
        _ => parse.string(value, (1, 128)),
    };
    if id.as_deref().is_some_and(|id| !crate::assessment::valid_id(id)) {
        parse.issue("Use an assessment ID from the returned lists or linked items.".to_owned(), true);
    }
    id
}

pub fn assessments(arguments: &Map<String, Value>) -> Result<AssessmentsRequest, String> {
    object(arguments, &["childId", "academicYearId"], |parse, args| AssessmentsRequest {
        child_id: optional(parse, args, "childId", |p, v| p.string(v, (1, 1024))),
        academic_year_id: optional(parse, args, "academicYearId", assessment_id),
    }, |_| None)
}

pub fn assessment_item(arguments: &Map<String, Value>) -> Result<AssessmentItemRequest, String> {
    object(arguments, &["childId", "kind", "id"], |parse, args| AssessmentItemRequest {
        child_id: optional(parse, args, "childId", |p, v| p.string(v, (1, 1024))),
        kind: parse.at("kind").option(args.get("kind"), &crate::assessment::KINDS).unwrap_or("grid"),
        id: assessment_id(&mut parse.at("id"), args.get("id")).unwrap_or_default(),
    }, |_| None)
}

/// The lower bound of a number: `.min(n)` or `.positive()`.
#[derive(Clone, Copy)]
enum Min {
    AtLeast(i64),
    Positive,
}

/// InfoMentor's fields, on top of the runtime's strings.
trait Fields {
    fn int(&mut self, value: Option<&Value>, min: Min, max: Option<i64>) -> Option<i64>;

    fn int_or(&mut self, value: Option<&Value>, bounds: (i64, i64), default: i64) -> i64;

    fn boolean_or(&mut self, value: Option<&Value>, default: bool) -> bool;

    fn option(&mut self, value: Option<&Value>, options: &[&'static str]) -> Option<&'static str>;
}

impl Fields for Parse<'_> {
    /// `z.number().int()` with a lower and an optional upper bound.
    fn int(&mut self, value: Option<&Value>, min: Min, max: Option<i64>) -> Option<i64> {
        let Some(number) = value.and_then(Value::as_f64) else {
            self.wrong_type("number", value);
            return None;
        };

        if number.fract() != 0.0 {
            self.wrong_type("int", value);
            return None;
        }

        if number > SAFE {
            self.issue(format!("Too big: expected int to be <={SAFE}"), true);
        } else if number < -SAFE {
            self.issue(format!("Too small: expected int to be >=-{SAFE}"), true);
        }

        match min {
            Min::AtLeast(min) if number < min as f64 => {
                self.issue(format!("Too small: expected number to be >={min}"), true);
            }
            Min::Positive if number <= 0.0 => {
                self.issue("Too small: expected number to be >0".to_owned(), true);
            }
            _ => {}
        }

        if let Some(max) = max.filter(|max| number > *max as f64) {
            self.issue(format!("Too big: expected number to be <={max}"), true);
        }
        Some(number as i64)
    }

    /// `z.number().int().min(min).max(max).default(default)`.
    fn int_or(&mut self, value: Option<&Value>, (min, max): (i64, i64), default: i64) -> i64 {
        match value {
            None => default,
            Some(_) => self
                .int(value, Min::AtLeast(min), Some(max))
                .unwrap_or(default),
        }
    }

    /// `z.boolean().default(default)`.
    fn boolean_or(&mut self, value: Option<&Value>, default: bool) -> bool {
        match value {
            None => default,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                self.wrong_type("boolean", value);
                default
            }
        }
    }

    /// `z.enum(options)`.
    fn option(&mut self, value: Option<&Value>, options: &[&'static str]) -> Option<&'static str> {
        let found = value
            .and_then(Value::as_str)
            .and_then(|text| options.iter().find(|option| **option == text).copied());

        if found.is_none() {
            let listed: Vec<String> = options
                .iter()
                .map(|option| format!("\"{option}\""))
                .collect();
            self.issue(
                format!("Invalid option: expected one of {}", listed.join("|")),
                false,
            );
        }
        found
    }
}

/// `selectChildRequestSchema`: the child's id.
pub fn select_child(arguments: &Map<String, Value>) -> Result<String, String> {
    let child = object(
        arguments,
        &["childId"],
        |parse, args| parse.at("childId").string(args.get("childId"), (1, 1024)),
        |_| None,
    )?;
    Ok(child.unwrap_or_default())
}

/// `messagesRequestSchema`.
pub fn messages(arguments: &Map<String, Value>) -> Result<MessagesRequest, String> {
    object(
        arguments,
        &["folder", "search", "page", "pageSize"],
        |parse, args| MessagesRequest {
            folder: optional(parse, args, "folder", |p, v| {
                p.option(v, &["inbox", "sent"])
            })
            .unwrap_or("inbox"),
            search: optional(parse, args, "search", |p, v| p.string(v, (0, 500)))
                .unwrap_or_default(),
            page: parse.at("page").int_or(args.get("page"), (1, 100_000), 1),
            page_size: parse
                .at("pageSize")
                .int_or(args.get("pageSize"), (1, 100), 20),
        },
        |_| None,
    )
}

/// `messageRequestSchema`: the message id.
pub fn message(arguments: &Map<String, Value>) -> Result<i64, String> {
    let id = object(
        arguments,
        &["id"],
        |parse, args| parse.at("id").int(args.get("id"), Min::Positive, None),
        |_| None,
    )?;
    Ok(id.unwrap_or_default())
}

/// `notificationsRequestSchema`.
pub fn notifications(arguments: &Map<String, Value>) -> Result<NotificationsRequest, String> {
    object(
        arguments,
        &["selectedChildOnly", "includeCleared"],
        |parse, args| NotificationsRequest {
            selected_child_only: parse
                .at("selectedChildOnly")
                .boolean_or(args.get("selectedChildOnly"), false),
            include_cleared: parse
                .at("includeCleared")
                .boolean_or(args.get("includeCleared"), false),
        },
        |_| None,
    )
}

/// `collectRequestSchema`.
pub fn collect(arguments: &Map<String, Value>) -> Result<CollectRequest, String> {
    object(
        arguments,
        &["cursor", "includeExisting", "maxMessagePages"],
        |parse, args| CollectRequest {
            cursor: optional(parse, args, "cursor", |p, v| {
                let text = p.string(v, (0, usize::MAX));

                if text.as_deref().is_some_and(|text| !is_uuid(text)) {
                    p.issue("Invalid UUID".to_owned(), true);
                }
                text
            }),
            include_existing: parse
                .at("includeExisting")
                .boolean_or(args.get("includeExisting"), false),
            max_message_pages: parse.at("maxMessagePages").int_or(
                args.get("maxMessagePages"),
                (1, 100),
                20,
            ),
        },
        |_| None,
    )
}

/// `z.string().refine(isAbsolute, ...)`: POSIX `path.isAbsolute`.
fn absolute(parse: &mut Parse, value: Option<&Value>) -> Option<String> {
    let text = parse.string(value, (0, usize::MAX));

    if text.as_deref().is_some_and(|text| !text.starts_with('/')) {
        parse.issue("Use an absolute path on the MCP host.".to_owned(), true);
    }
    text
}

/// `loginRequestSchema`.
pub fn login(arguments: &Map<String, Value>) -> Result<LoginRequest, String> {
    object(
        arguments,
        &[
            "importFile",
            "credentialsFile",
            "allowAccountChange",
            "timeoutSeconds",
        ],
        |parse, args| LoginRequest {
            import_file: optional(parse, args, "importFile", absolute),
            credentials_file: optional(parse, args, "credentialsFile", absolute),
            allow_account_change: optional(parse, args, "allowAccountChange", |p, v| match v {
                Some(Value::Bool(flag)) => Some(*flag),
                _ => {
                    p.wrong_type("boolean", v);
                    None
                }
            }),
            timeout_seconds: parse.at("timeoutSeconds").int_or(
                args.get("timeoutSeconds"),
                (1, 3600),
                300,
            ),
        },
        |_| None,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn invalid_inputs_get_the_zod_texts() {
        // Texts from zod 4's safeParse of the TypeScript schemas.
        assert_eq!(
            message(&args(json!({"id": 0}))).unwrap_err(),
            "id: Too small: expected number to be >0"
        );
        assert_eq!(
            message(&args(json!({"id": -1.5}))).unwrap_err(),
            "id: Invalid input: expected int, received number"
        );
        assert_eq!(
            message(&args(json!({}))).unwrap_err(),
            "id: Invalid input: expected number, received undefined"
        );
        assert_eq!(message(&args(json!({"id": 7}))), Ok(7));
        assert_eq!(
            collect(&args(json!({"cursor": "x", "maxMessagePages": 0}))).unwrap_err(),
            "cursor: Invalid UUID, maxMessagePages: Too small: expected number to be >=1"
        );
        assert_eq!(
            collect(&args(json!({"cursor": 5}))).unwrap_err(),
            "cursor: Invalid input: expected string, received number"
        );
        assert_eq!(
            messages(&args(json!({
                "folder": "x", "search": "a".repeat(501), "page": 0, "pageSize": 101, "z": 1
            })))
            .unwrap_err(),
            "folder: Invalid option: expected one of \"inbox\"|\"sent\", search: Too big: expected string to have <=500 characters, page: Too small: expected number to be >=1, pageSize: Too big: expected number to be <=100, Unrecognized key: \"z\""
        );
        assert_eq!(
            select_child(&args(json!({"childId": ""}))).unwrap_err(),
            "childId: Too small: expected string to have >=1 characters"
        );
    }

    #[test]
    fn login_requests_get_the_zod_texts() {
        // Texts from zod 4's safeParse of loginRequestSchema.
        for (input, text) in [
            (
                json!({"importFile": "rel"}),
                "importFile: Use an absolute path on the MCP host.",
            ),
            (
                json!({"importFile": ""}),
                "importFile: Use an absolute path on the MCP host.",
            ),
            (
                json!({"importFile": 5}),
                "importFile: Invalid input: expected string, received number",
            ),
            (
                json!({"credentialsFile": "x", "importFile": "y"}),
                "importFile: Use an absolute path on the MCP host., credentialsFile: Use an absolute path on the MCP host.",
            ),
            (
                json!({"allowAccountChange": "yes"}),
                "allowAccountChange: Invalid input: expected boolean, received string",
            ),
            (
                json!({"timeoutSeconds": 0}),
                "timeoutSeconds: Too small: expected number to be >=1",
            ),
            (
                json!({"timeoutSeconds": 1.5}),
                "timeoutSeconds: Invalid input: expected int, received number",
            ),
            (
                json!({"timeoutSeconds": 3601}),
                "timeoutSeconds: Too big: expected number to be <=3600",
            ),
            (
                json!({"z": 1, "importFile": "r", "timeoutSeconds": 0}),
                "importFile: Use an absolute path on the MCP host., timeoutSeconds: Too small: expected number to be >=1, Unrecognized key: \"z\"",
            ),
            (
                json!({"importFile": null}),
                "importFile: Invalid input: expected string, received null",
            ),
        ] {
            assert_eq!(login(&args(input)).unwrap_err(), text);
        }
        assert_eq!(
            login(&args(json!({"importFile": "/a", "credentialsFile": "/b"}))),
            Ok(LoginRequest {
                import_file: Some("/a".to_owned()),
                credentials_file: Some("/b".to_owned()),
                allow_account_change: None,
                timeout_seconds: 300,
            })
        );
    }

    #[test]
    fn defaults_fill_missing_fields() {
        assert_eq!(
            messages(&args(json!({"page": 2}))),
            Ok(MessagesRequest {
                folder: "inbox",
                search: String::new(),
                page: 2,
                page_size: 20,
            })
        );
        assert_eq!(
            notifications(&args(json!({"includeCleared": true}))),
            Ok(NotificationsRequest {
                selected_child_only: false,
                include_cleared: true,
            })
        );
        let cursor = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(
            collect(&args(json!({"cursor": cursor}))),
            Ok(CollectRequest {
                cursor: Some(cursor.to_owned()),
                include_existing: false,
                max_message_pages: 20,
            })
        );
        assert_eq!(
            select_child(&args(json!({"childId": "7"}))),
            Ok("7".to_owned())
        );
    }
}
