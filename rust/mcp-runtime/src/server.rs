//! The MCP surface: tools/list replays the TypeScript server's advertised tools (a JSON file the
//! server compiles in, checked by its tests), so names, descriptions, schemas and annotations
//! match exactly; calls get the TypeScript SDK's error texts and `toolResult`'s results.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;

use crate::fail::{Failure, tool_text};

/// A TypeScript server's instructions and advertised tools.
pub struct Surface {
    pub instructions: String,
    pub tools: Vec<Tool>,
}

impl Surface {
    /// `{ "instructions": ..., "tools": [...] }`, as the server's `include_str!` supplies it.
    /// Panics on anything else: the file is compiled in and checked by the server's tests.
    pub fn parse(json: &str) -> Self {
        let mut surface: Value = serde_json::from_str(json).expect("the surface is JSON");
        Surface {
            instructions: surface["instructions"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            tools: serde_json::from_value(surface["tools"].take()).expect("the surface has tools"),
        }
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name == name)
    }
}

/// `toolResult`: the output as JSON text and structured content, or a fixed error text.
pub fn tool_result<F: Failure>(output: Result<Value, F>) -> CallToolResult {
    match output {
        Ok(output) => {
            let mut result = CallToolResult::structured(output);
            result.is_error = None;
            result
        }
        Err(fail) => CallToolResult::error(vec![ContentBlock::text(tool_text(fail))]),
    }
}

/// The SDK's error for a tool the surface does not list.
pub fn unknown_tool(name: &str) -> ErrorData {
    ErrorData::invalid_params(format!("Tool {name} not found"), None)
}

/// The SDK's result for arguments the tool's schema refuses, with zod's issue text.
pub fn invalid_arguments(name: &str, issues: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "Input validation error: Invalid arguments for tool {name}: {issues}"
    ))])
}

/// rmcp's per-request token, as the TypeScript SDK's `ctx.mcpReq.signal`: it completes when the
/// host cancels the call (`notifications/cancelled`) and when serving stops, which on SIGINT,
/// SIGTERM or a broken connection is at once, but at stdin's end only after rmcp has waited up to
/// 5 s for calls in flight to answer (tests/signals.rs, tests/connection.rs). It also completes
/// once the call has answered.
pub type Cancelled = Pin<Box<dyn Future<Output = ()> + Send>>;

/// One MCP server: its identity, surface and tools, and its stdio lifetime hooks.
pub trait Server: Send + Sync + 'static {
    type Fail: Failure + Send;

    const NAME: &'static str;

    const VERSION: &'static str;

    fn surface(&self) -> &Surface;

    /// Run a listed tool: the input validation issues (`Err`), or the tool's outcome. `cancelled`
    /// completes when the call is cancelled.
    fn call(
        &self,
        name: &str,
        arguments: &JsonObject,
        cancelled: Cancelled,
    ) -> impl Future<Output = Result<Result<Value, Self::Fail>, String>> + Send;

    /// Standard input ended. Called on every read at the end, so it must be idempotent.
    fn stdin_ended(&self) {}

    /// Cancel and wait for operations in flight, before the process exits.
    fn close(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// rmcp's handler for a [`Server`].
pub struct Handler<S>(Arc<S>);

impl<S> Handler<S> {
    pub fn new(server: Arc<S>) -> Self {
        Self(server)
    }
}

impl<S: Server> ServerHandler for Handler<S> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(S::NAME, S::VERSION))
        .with_instructions(self.0.surface().instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.0.surface().tools.clone(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.as_ref();

        if !self.0.surface().has_tool(name) {
            return Err(unknown_tool(name));
        }
        let arguments = request.arguments.unwrap_or_default();

        let cancelled = Box::pin(context.ct.clone().cancelled_owned());

        Ok(match self.0.call(name, &arguments, cancelled).await {
            Ok(output) => tool_result(output),
            Err(issues) => invalid_arguments(name, &issues),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fail::{Fail, UNKNOWN_ERROR, ZOD_ERROR};

    const SURFACE: &str = r#"{
        "instructions": "Read only.",
        "tools": [{ "name": "get_item", "inputSchema": { "type": "object" } }]
    }"#;

    #[test]
    fn results_match_tool_result() {
        let ok = serde_json::to_value(tool_result::<Fail>(Ok(json!({ "b": 1, "a": [] })))).unwrap();
        assert_eq!(
            ok,
            json!({
                "resultType": ok["resultType"],
                "content": [{ "type": "text", "text": r#"{"b":1,"a":[]}"# }],
                "structuredContent": { "b": 1, "a": [] },
            })
        );

        for (fail, text) in [
            (Fail::Unknown, UNKNOWN_ERROR),
            (Fail::Invalid, ZOD_ERROR),
            (Fail::Safe("Reviewed."), "Reviewed."),
        ] {
            let error = serde_json::to_value(tool_result(Err(fail))).unwrap();
            assert_eq!(error["isError"], true);
            assert_eq!(error["content"], json!([{ "type": "text", "text": text }]));
            assert_eq!(error.get("structuredContent"), None);
        }
    }

    #[test]
    fn call_errors_match_the_sdk_texts() {
        let unknown = unknown_tool("nope");
        assert_eq!(unknown.code, ErrorData::invalid_params("", None).code);
        assert_eq!(unknown.message, "Tool nope not found");

        let invalid =
            serde_json::to_value(invalid_arguments("get_item", "Unrecognized key: \"a\"")).unwrap();
        assert_eq!(invalid["isError"], true);
        assert_eq!(
            invalid["content"][0]["text"],
            r#"Input validation error: Invalid arguments for tool get_item: Unrecognized key: "a""#
        );
    }

    #[test]
    fn the_surface_replays_its_json() {
        let surface = Surface::parse(SURFACE);
        assert_eq!(surface.instructions, "Read only.");
        assert_eq!(surface.tools.len(), 1);
        assert!(surface.has_tool("get_item"));
        assert!(!surface.has_tool("get_items"));
    }
}
