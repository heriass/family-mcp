//! The MCP surface of packages/infomentor-mcp/src/server.ts. tools/list replays the TypeScript
//! server's advertised tools (src/surface.json, generated with the setup tools and checked by a
//! test), so names, descriptions, schemas and annotations match exactly.

use std::sync::Arc;

use mcp_runtime::{Cancelled, Server, Surface};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use crate::client::Client;
use crate::error::{Fail, Result};
use crate::input;
use crate::signal::Signal;

/// The tools registered only with `--allow-setup-tools`: a prompt-injected agent must not be able
/// to log the parent out or replace the account.
const SETUP_TOOLS: [&str; 4] = [
    "infomentor_login",
    "infomentor_setup_status",
    "infomentor_cancel_setup",
    "infomentor_logout",
];

/// The TypeScript surface, without the setup tools unless they are allowed.
fn surface(allow_setup_tools: bool) -> Surface {
    let mut surface = Surface::parse(include_str!("surface.json"));
    let assessments = Surface::parse(include_str!("assessment-surface.json"));
    let at = surface.tools.iter().position(|tool| SETUP_TOOLS.contains(&tool.name.as_ref())).unwrap_or(surface.tools.len());
    surface.tools.splice(at..at, assessments.tools);

    if !allow_setup_tools {
        surface
            .tools
            .retain(|tool| !SETUP_TOOLS.contains(&tool.name.as_ref()));
    }
    surface
}

pub struct InfoMentor {
    surface: Surface,
    client: Arc<Client>,
}

impl InfoMentor {
    pub fn new(allow_setup_tools: bool, client: Client) -> Self {
        Self {
            surface: surface(allow_setup_tools),
            client: Arc::new(client),
        }
    }
}

impl Server for InfoMentor {
    type Fail = Fail;

    const NAME: &'static str = "infomentor-mcp";

    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn surface(&self) -> &Surface {
        &self.surface
    }

    // A host's cancel is not passed on, though the TypeScript tools abort on theirs: a call ends
    // with the client's lifetime.
    async fn call(
        &self,
        name: &str,
        arguments: &JsonObject,
        _cancelled: Cancelled,
    ) -> std::result::Result<Result<Value>, String> {
        let client = &self.client;
        let signal = Signal::default();

        Ok(match name {
            "infomentor_session_status" => {
                input::empty(arguments)?;
                client.session_status(signal).await
            }
            "infomentor_get_overview" => {
                input::empty(arguments)?;
                client.overview(signal).await
            }
            "infomentor_select_child" => {
                let child_id = input::select_child(arguments)?;
                client.select_child(child_id, signal).await
            }
            "infomentor_get_messages" => {
                let request = input::messages(arguments)?;
                client.messages(request, signal).await
            }
            "infomentor_get_message" => {
                let id = input::message(arguments)?;
                client.message(id, signal).await
            }
            "infomentor_get_notifications" => {
                let request = input::notifications(arguments)?;
                client.notifications(request, signal).await
            }
            "infomentor_collect_updates" => {
                let request = input::collect(arguments)?;
                client.collect(request, signal).await
            }
            "infomentor_get_assessments" => {
                let request = input::assessments(arguments)?;
                client.assessments(request, signal).await
            }
            "infomentor_get_assessment_item" => {
                let request = input::assessment_item(arguments)?;
                client.assessment_item(request, signal).await
            }
            "infomentor_login" => {
                let request = input::login(arguments)?;
                client.start_login(request)
            }
            "infomentor_setup_status" => {
                input::empty(arguments)?;
                Ok(client.setup_status())
            }
            "infomentor_cancel_setup" => {
                input::empty(arguments)?;
                Ok(client.cancel_setup().await)
            }
            "infomentor_logout" => {
                input::empty(arguments)?;
                client.logout().await.map(|()| {
                    json!({"authenticated": false, "nextStep": "Call infomentor_login to sign in again."})
                })
            }
            // The runtime lists and accepts only the surface's tools.
            _ => Err(Fail::Unknown),
        })
    }

    fn stdin_ended(&self) {
        self.client.abort();
    }

    async fn close(&self) {
        self.client.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_tools_are_listed_only_when_allowed() {
        let names = |allowed| -> Vec<String> {
            surface(allowed)
                .tools
                .iter()
                .map(|tool| tool.name.to_string())
                .collect()
        };
        assert_eq!(
            names(false),
            [
                "infomentor_session_status",
                "infomentor_get_overview",
                "infomentor_select_child",
                "infomentor_get_messages",
                "infomentor_get_message",
                "infomentor_get_notifications",
                "infomentor_collect_updates",
                "infomentor_get_assessments",
                "infomentor_get_assessment_item",
            ]
        );
        assert_eq!(names(true).len(), 13);
        assert!(names(true).ends_with(&SETUP_TOOLS.map(String::from)));
    }
}
