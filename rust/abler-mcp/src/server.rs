//! The MCP surface of packages/abler-mcp/src/server.ts. tools/list replays the TypeScript server's
//! advertised tools (src/surface.json, checked by a test), so names, descriptions, schemas and
//! annotations match exactly; inputs are validated with zod's rules and messages (input.rs).

use std::sync::{Arc, OnceLock};

use mcp_runtime::{Cancelled, Server, Surface};
use rmcp::model::JsonObject;
use serde_json::Value;

use crate::api::{self, Client};
use crate::error::{Fail, Result};
use crate::input;

fn surface() -> &'static Surface {
    static SURFACE: OnceLock<Surface> = OnceLock::new();
    SURFACE.get_or_init(|| Surface::parse(include_str!("surface.json")))
}

/// `toolResult` with Abler's failures, so a result's type needs no annotation.
#[cfg(test)]
fn tool_result(output: Result<Value>) -> rmcp::model::CallToolResult {
    mcp_runtime::tool_result(output)
}

pub struct Abler {
    pub client: Arc<Client>,
}

impl Server for Abler {
    type Fail = Fail;

    const NAME: &'static str = "abler-mcp";

    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn surface(&self) -> &Surface {
        surface()
    }

    async fn call(
        &self,
        name: &str,
        arguments: &JsonObject,
        _cancelled: Cancelled,
    ) -> std::result::Result<Result<Value>, String> {
        let client = &self.client;

        Ok(match name {
            "auth_status" => {
                input::empty(arguments)?;
                client
                    .run(|client| client.status(api::status_forces_refresh()))
                    .await
            }
            "get_profile" => {
                input::empty(arguments)?;
                client.run(Client::profile).await
            }
            "list_groups" => {
                input::empty(arguments)?;
                client.run(Client::groups).await
            }
            "list_schedule" => {
                let filters = input::schedule(arguments)?;
                client.run(|client| client.schedule(filters)).await
            }
            "list_child_schedules" => {
                let filters = input::child_schedules(arguments)?;
                client.run(|client| client.child_schedules(filters)).await
            }
            "get_event" => {
                let event = input::event(arguments)?;
                client.run(|client| client.event(event)).await
            }
            "list_conversations" => {
                let page = input::conversations(arguments)?;
                client.run(|client| client.conversations(page)).await
            }
            "list_messages" => {
                let messages = input::messages(arguments)?;
                client.run(|client| client.messages(messages)).await
            }
            "describe_api" => {
                let request = crate::describe::input(arguments)?;
                client.run(|client| client.describe_api(request)).await
            }
            "mark_conversation_read" | "mark_all_read" | "reply_to_conversation"
            | "set_child_attendance" | "join_event" | "leave_event" => {
                let action = crate::mutations::input(name, arguments)?;
                client.run(|client| client.action(action)).await
            }
            _ => unreachable!("only listed tools are called"),
        })
    }

    /// Abort the client's requests where stdin ends, as the TypeScript server closes on `end`.
    fn stdin_ended(&self) {
        self.client.abort();
    }

    async fn close(&self) {
        self.client.close().await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn results_match_tool_result() {
        let ok = serde_json::to_value(tool_result(Ok(json!({ "b": 1, "a": [] })))).unwrap();
        assert_eq!(
            ok,
            json!({
                "resultType": ok["resultType"],
                "content": [{ "type": "text", "text": r#"{"b":1,"a":[]}"# }],
                "structuredContent": { "b": 1, "a": [] },
            })
        );
        let error = serde_json::to_value(tool_result(Err(crate::error::Fail::Unknown))).unwrap();
        assert_eq!(error["isError"], true);
        assert_eq!(
            error["content"][0]["text"],
            "The operation failed. Check the server logs for details."
        );
        assert_eq!(surface().tools.len(), 15);
        let describe = surface().tools.iter().find(|tool| tool.name == "describe_api").unwrap();
        assert_eq!(serde_json::to_value(describe).unwrap()["annotations"]["readOnlyHint"], true);
    }
}
