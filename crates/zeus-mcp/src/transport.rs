//! The real rmcp stdio binding. Human approvals are never exposed through MCP.

use crate::catalogue::{SERVER_INSTRUCTIONS, catalogue};
use crate::resources::{self, PROMPT_RESCUE, ResourceUri};
use crate::{ToolOutcome, ZeusTools};
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use std::sync::Arc;
use zeus_api::{ApiError, ApiResult, Client};

struct McpServer<C: Client> {
    client: Arc<C>,
}

fn tool_result(outcome: ToolOutcome) -> CallToolResult {
    let mut result = if outcome.is_error() {
        CallToolResult::error(vec![ContentBlock::text(outcome.text())])
    } else {
        CallToolResult::success(vec![ContentBlock::text(outcome.text())])
    };
    result.structured_content = Some(outcome.structured());
    result
}

fn no_cursor(request: Option<PaginatedRequestParams>) -> Result<(), ErrorData> {
    if request.is_some_and(|r| r.cursor.is_some()) {
        Err(ErrorData::invalid_params(
            "This catalogue has no continuation cursor",
            None,
        ))
    } else {
        Ok(())
    }
}

impl<C: Client + 'static> ServerHandler for McpServer<C> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("zeus", env!("CARGO_PKG_VERSION")))
        .with_instructions(SERVER_INSTRUCTIONS)
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        no_cursor(request)?;
        Ok(ListToolsResult::with_all_items(
            catalogue().iter().map(|t| t.to_rmcp()).collect(),
        ))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        let outcome = ZeusTools::new(Arc::clone(&self.client))
            .call(&request.name, args)
            .await
            .map_err(|_| ErrorData::invalid_params("Unknown Zeus tool", None))?;
        Ok(tool_result(outcome).into())
    }
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        no_cursor(request)?;
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("zeus://audit/today", "audit-today")
                .with_description("Recent redacted audit records visible to this client.")
                .with_mime_type("application/json"),
        ]))
    }
    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        no_cursor(request)?;
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("zeus://jobs/{id}", "job")
                .with_description("Download state and progress; URLs are redacted.")
                .with_mime_type("application/json"),
            ResourceTemplate::new("zeus://runs/{id}/manifest", "run-manifest")
                .with_description("Bounded redacted provenance records of an owned workflow run.")
                .with_mime_type("application/json"),
        ]))
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = ResourceUri::parse(&request.uri)
            .ok_or_else(|| ErrorData::invalid_params("Unknown Zeus resource", None))?;
        let contents = resources::read_resource(&self.client, &uri)
            .await
            .map_err(|e| {
                ErrorData::internal_error(
                    e.envelope().message.clone(),
                    Some(serde_json::json!({"code":e.envelope().code})),
                )
            })?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(contents, request.uri).with_mime_type("application/json"),
        ])
        .into())
    }
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        no_cursor(request)?;
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            PROMPT_RESCUE,
            Some("Review a failed download and choose a safe recovery step."),
            Some(vec![PromptArgument::new("job_id").with_required(true)]),
        )]))
    }
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if request.name != PROMPT_RESCUE {
            return Err(ErrorData::invalid_params("Unknown Zeus prompt", None));
        }
        let job = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("job_id"))
            .and_then(|v| v.as_str())
            .and_then(|id| id.parse::<zeus_types::JobId>().ok())
            .ok_or_else(|| {
                ErrorData::invalid_params("job_id must be a Zeus job identifier", None)
            })?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            resources::rescue_prompt(&job.to_string()),
        )])
        .into())
    }
}

/// Serve MCP over stdin/stdout until the client disconnects. Diagnostics stay on stderr.
pub async fn serve_stdio<C: Client + 'static>(client: Arc<C>) -> ApiResult<()> {
    let service = McpServer { client }
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|_| ApiError::internal("MCP initialization failed"))?;
    service
        .waiting()
        .await
        .map_err(|_| ApiError::internal("MCP service stopped unexpectedly"))?;
    Ok(())
}

/// Optional HTTP bridge remains unavailable until its token and Origin checks are implemented.
#[cfg(feature = "http")]
pub async fn serve_http<C: Client>(
    _client: Arc<C>,
    _port: u16,
    _bearer_token_hash: [u8; 32],
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "MCP HTTP bridge (bearer authentication and Origin validation)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_remain_structured_tool_errors() {
        let error =
            zeus_types::ErrorEnvelope::from_code(zeus_api::error::codes::INVALID_PARAMS, "Bad URL");
        let result = tool_result(ToolOutcome::Error {
            next: crate::output::next_step_for(error.class),
            envelope: error,
        });
        assert_eq!(result.is_error, Some(true));
        assert!(result.structured_content.unwrap().get("next").is_some());
        assert_eq!(result.content.len(), 1);
    }
    #[test]
    fn success_has_machine_and_text_representations() {
        let result = tool_result(ToolOutcome::Ok(serde_json::json!({"jobs":[]})));
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content.unwrap(),
            serde_json::json!({"jobs":[]})
        );
    }
    struct FakeClient {
        session: zeus_api::methods::daemon::AuthenticateResult,
        notifications: tokio::sync::broadcast::Sender<zeus_api::jsonrpc::Notification>,
    }
    #[async_trait::async_trait]
    impl Client for FakeClient {
        async fn call_raw(&self, _: &str, _: serde_json::Value) -> ApiResult<serde_json::Value> {
            Err(ApiError::invalid_params(
                "scripted daemon rejected the request",
            ))
        }
        fn notifications(
            &self,
        ) -> tokio::sync::broadcast::Receiver<zeus_api::jsonrpc::Notification> {
            self.notifications.subscribe()
        }
        fn session(&self) -> &zeus_api::methods::daemon::AuthenticateResult {
            &self.session
        }
    }

    #[tokio::test]
    async fn stdio_protocol_lists_only_agent_tools_and_returns_structured_errors() {
        use serde_json::{Value, json};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (notifications, _) = tokio::sync::broadcast::channel(8);
        let client = Arc::new(FakeClient {
            session: serde_json::from_value(json!({
                "client_id":"cli_test", "kind":"agent", "credential_kind":"agent", "downgraded":false, "role":"mcp", "permissions":[],
                "limits":zeus_api::methods::daemon::ConnectionLimits::STANDARD,
                "connection_id":zeus_types::IdGenerator::new().connection_id()
            }))
            .unwrap(),
            notifications,
        });
        let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            let service = McpServer { client }.serve(server_stream).await.unwrap();
            service.waiting().await.unwrap();
        });
        let (reader, mut writer) = tokio::io::split(client_stream);
        let mut reader = BufReader::new(reader);
        async fn exchange(
            reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
            writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
            value: Value,
        ) -> Value {
            writer
                .write_all(format!("{value}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                reader.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        let initialized = exchange(&mut reader, &mut writer, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
        assert!(
            initialized["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("only the user can approve")
        );
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let catalogue = exchange(
            &mut reader,
            &mut writer,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;
        let tools = catalogue["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
        assert!(
            !tools
                .iter()
                .any(|tool| tool["name"].as_str().unwrap().contains("approve"))
        );
        let failure = exchange(&mut reader, &mut writer, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"zeus_probe","arguments":{"url":"not a URL"}}})).await;
        assert_eq!(failure["result"]["isError"], true);
        assert!(failure["result"]["structuredContent"]["code"].is_string());
        drop(writer);
        drop(reader);
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
}
