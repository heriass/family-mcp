//! Fixed, explicitly scoped Abler writes. Inputs become GraphQL variables; callers cannot supply
//! an operation, field name, selection set or arbitrary GraphQL document.
use serde_json::{Map, Value, json};

use crate::error::{Fail, Result};

const MUTATIONS: [&str; 6] = [
    "MarkAsRead",
    "MarkAllAsRead",
    "MessageReply",
    "setPlayerAttendanceV4",
    "UserJoinEvent",
    "UserLeaveEvent",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    MarkConversationRead { conversation_id: String },
    MarkAllRead,
    Reply { conversation_id: String, message_text: String, confirm: bool },
    Attendance { event_id: String, child_id: String, status: String, confirm: bool },
    Join { child_id: String, event_id: String, age_group_id: String, confirm: bool },
    Leave { child_id: String, event_id: String, age_group_id: String, confirm: bool },
}

impl Action {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::MarkConversationRead { .. } => "MarkAsRead",
            Self::MarkAllRead => "MarkAllAsRead",
            Self::Reply { .. } => "MessageReply",
            Self::Attendance { .. } => "setPlayerAttendanceV4",
            Self::Join { .. } => "UserJoinEvent",
            Self::Leave { .. } => "UserLeaveEvent",
        }
    }

    pub fn requires_confirmation(&self) -> bool {
        matches!(self, Self::Reply { .. } | Self::Attendance { .. } | Self::Join { .. } | Self::Leave { .. })
    }

    pub fn confirmed(&self) -> bool {
        match self {
            Self::Reply { confirm, .. } | Self::Attendance { confirm, .. }
            | Self::Join { confirm, .. } | Self::Leave { confirm, .. } => *confirm,
            Self::MarkConversationRead { .. } | Self::MarkAllRead => true,
        }
    }

    pub fn child_id(&self) -> Option<&str> {
        match self {
            Self::Attendance { child_id, .. } | Self::Join { child_id, .. } | Self::Leave { child_id, .. } => Some(child_id),
            _ => None,
        }
    }

    pub fn attendance_status(&self) -> Option<&str> {
        match self { Self::Attendance { status, .. } => Some(status), _ => None }
    }

    pub fn document(&self) -> &'static str {
        match self {
            Self::MarkConversationRead { .. } => "mutation MarkAsRead($conversationId: String!) { MarkAsRead(conversationId: $conversationId) { __typename } }",
            Self::MarkAllRead => "mutation MarkAllAsRead($userId: String!) { MarkAllAsRead(userId: $userId) }",
            Self::Reply { .. } => "mutation MessageReply($conversationId: String!, $messageText: String!) { MessageReply(conversationId: $conversationId, messageText: $messageText) { __typename } }",
            Self::Attendance { .. } => "mutation SetPlayerAttendanceV4($eventId: ID!, $playerId: ID!, $status: PLAYER_ATTENDANCE_STATUS!) { setPlayerAttendanceV4(eventId: $eventId, playerId: $playerId, status: $status) { __typename } }",
            Self::Join { .. } => "mutation UserJoinEvent($playerId: String!, $toEventId: String!, $ageGroupId: String!) { UserJoinEvent(playerId: $playerId, toEventId: $toEventId, ageGroupId: $ageGroupId) { __typename } }",
            Self::Leave { .. } => "mutation UserLeaveEvent($playerId: String!, $eventId: String!, $ageGroupId: String!) { UserLeaveEvent(playerId: $playerId, eventId: $eventId, ageGroupId: $ageGroupId) { __typename } }",
        }
    }

    pub fn variables(&self, account_id: Option<&str>, player_id: Option<&str>) -> Value {
        match self {
            Self::MarkConversationRead { conversation_id } => json!({"conversationId":conversation_id}),
            Self::MarkAllRead => json!({"userId":account_id.unwrap_or_default()}),
            Self::Reply { conversation_id, message_text, .. } => json!({"conversationId":conversation_id,"messageText":message_text}),
            Self::Attendance { event_id, status, .. } => json!({"eventId":event_id,"playerId":player_id.unwrap_or_default(),"status":status}),
            Self::Join { event_id, age_group_id, .. } => json!({"playerId":player_id.unwrap_or_default(),"toEventId":event_id,"ageGroupId":age_group_id}),
            Self::Leave { event_id, age_group_id, .. } => json!({"playerId":player_id.unwrap_or_default(),"eventId":event_id,"ageGroupId":age_group_id}),
        }
    }
}

fn string(args: &Map<String, Value>, name: &str, maximum: usize) -> std::result::Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.is_empty() && value.chars().count() <= maximum => Ok(value.clone()),
        _ => Err(format!("{name}: expected a nonempty string of at most {maximum} characters.")),
    }
}

fn confirm(args: &Map<String, Value>) -> std::result::Result<bool, String> {
    match args.get("confirm") {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err("confirm: expected a boolean; use true only after approval.".to_owned()),
    }
}

pub fn input(tool: &str, args: &Map<String, Value>) -> std::result::Result<Action, String> {
    let (allowed, action) = match tool {
        "mark_conversation_read" => (&["conversationId"][..], Action::MarkConversationRead { conversation_id: string(args,"conversationId",256)? }),
        "mark_all_read" => (&[][..], Action::MarkAllRead),
        "reply_to_conversation" => (&["conversationId","messageText","confirm"][..], Action::Reply {
            conversation_id: string(args,"conversationId",256)?,
            message_text: string(args,"messageText",10_000)?, confirm: confirm(args)?,
        }),
        "set_child_attendance" => (&["eventId","childId","status","confirm"][..], Action::Attendance {
            event_id: string(args,"eventId",256)?, child_id: string(args,"childId",256)?,
            status: string(args,"status",64)?, confirm: confirm(args)?,
        }),
        "join_event" => (&["childId","eventId","ageGroupId","confirm"][..], Action::Join {
            child_id: string(args,"childId",256)?, event_id: string(args,"eventId",256)?,
            age_group_id: string(args,"ageGroupId",256)?, confirm: confirm(args)?,
        }),
        "leave_event" => (&["childId","eventId","ageGroupId","confirm"][..], Action::Leave {
            child_id: string(args,"childId",256)?, event_id: string(args,"eventId",256)?,
            age_group_id: string(args,"ageGroupId",256)?, confirm: confirm(args)?,
        }),
        _ => return Err("Unknown Abler action.".to_owned()),
    };
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Unrecognized action input field.".to_owned());
    }
    Ok(action)
}

pub fn preview(action: &Action, variables: Value) -> Value {
    json!({
        "preview": true,
        "operationName": action.operation(),
        "query": action.document(),
        "variables": variables,
        "message": "No change was sent. Show this exact preview and ask the user to approve it, then call again with confirm: true.",
    })
}

pub fn enum_values(messages: &[String]) -> Vec<String> {
    crate::describe::attendance_enum_values(messages)
}

fn invalid_enum() -> Fail {
    Fail::Safe("Unknown attendance status. Call describe_api for discovered enum values, then retry.")
}

pub fn validate_attendance_status(status: &str, allowed: &[String]) -> Result<()> {
    if allowed.iter().any(|value| value == status) { Ok(()) } else { Err(invalid_enum()) }
}

pub fn linked_child(child_id: &str, profile: &Value) -> bool {
    profile["children"].as_array().is_some_and(|children| children.iter().any(|child| child["id"] == child_id))
}

pub fn allowed_mutation(operation: &str) -> bool { MUTATIONS.contains(&operation) }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: Value) -> Map<String, Value> { value.as_object().cloned().unwrap() }

    #[test]
    fn confirmation_is_required_except_for_the_two_idempotent_mark_read_actions() {
        let examples = [
            ("mark_conversation_read", json!({"conversationId":"c1"}), true),
            ("mark_all_read", json!({}), true),
            ("reply_to_conversation", json!({"conversationId":"c1","messageText":"hello"}), false),
            ("set_child_attendance", json!({"eventId":"e1","childId":"p1","status":"PRESENT"}), false),
            ("join_event", json!({"childId":"p1","eventId":"e1","ageGroupId":"g1"}), false),
            ("leave_event", json!({"childId":"p1","eventId":"e1","ageGroupId":"g1"}), false),
        ];
        for (tool, value, expected) in examples {
            let action = input(tool, &args(value)).unwrap();
            assert_eq!(action.confirmed(), expected, "{tool}");
            if action.requires_confirmation() {
                assert!(preview(&action, action.variables(None, Some("p1"))).to_string().contains("No change was sent"));
                let mut confirmed = match action {
                    Action::Reply { conversation_id, message_text, .. } => json!({"conversationId":conversation_id,"messageText":message_text,"confirm":true}),
                    Action::Attendance { event_id, child_id, status, .. } => json!({"eventId":event_id,"childId":child_id,"status":status,"confirm":true}),
                    Action::Join { child_id, event_id, age_group_id, .. } | Action::Leave { child_id, event_id, age_group_id, .. } => json!({"childId":child_id,"eventId":event_id,"ageGroupId":age_group_id,"confirm":true}),
                    _ => unreachable!(),
                };
                assert!(input(tool, confirmed.as_object().unwrap()).unwrap().confirmed());
                confirmed["confirm"] = Value::Bool(false);
                assert!(!input(tool, confirmed.as_object().unwrap()).unwrap().confirmed());
            }
        }
        let approved = input("reply_to_conversation", &args(json!({"conversationId":"c1","messageText":"hello","confirm":true}))).unwrap();
        assert!(approved.confirmed());
    }

    #[test]
    fn attendance_requires_a_discovered_enum_value_and_child_operations_are_fixed() {
        let values = enum_values(&["Value \"__INVALID_ENUM__\" does not exist in \"PLAYER_ATTENDANCE_STATUS\" enum. Did you mean \"PRESENT\", \"ABSENT\", or \"LATE\"?".to_owned()]);
        assert_eq!(values, ["ABSENT", "LATE", "PRESENT"]);
        validate_attendance_status("PRESENT", &values).unwrap();
        assert!(validate_attendance_status("NOT_A_STATUS", &values).is_err());
        assert_eq!(input("join_event", &args(json!({"childId":"p1","eventId":"e1","ageGroupId":"g1","confirm":true}))).unwrap().operation(), "UserJoinEvent");
        assert_eq!(input("leave_event", &args(json!({"childId":"p1","eventId":"e1","ageGroupId":"g1","confirm":true}))).unwrap().operation(), "UserLeaveEvent");
        assert_eq!(MUTATIONS.len(), 6);
        assert!(MUTATIONS.iter().all(|name| allowed_mutation(name)));
        assert!(!["DeleteConversationMessage", "SetConversationName", "markInAppMessagesAsSeen", "sendMessage"].iter().any(|name| allowed_mutation(name)));
        let profile = json!({"children":[{"id":"child-1"}]});
        assert!(linked_child("child-1", &profile));
        assert!(!linked_child("child-2", &profile));
        assert!(!linked_child("child-1", &json!({"children":[]})));
    }
}
