use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{StatusCode, header},
    response::Response,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use tower_http::validate_request::ValidateRequestHeaderLayer;

use crate::mcp::task_server::TaskServer;

const MCP_HTTP_AUTH_TOKEN_ENV: &str = "VK_MCP_HTTP_AUTH_TOKEN";

pub fn router(base_url: &str) -> Router {
    router_with_token(
        base_url,
        std::env::var(MCP_HTTP_AUTH_TOKEN_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty()),
    )
}

pub fn http_auth_token_enabled() -> bool {
    std::env::var(MCP_HTTP_AUTH_TOKEN_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn router_with_token(base_url: &str, auth_token: Option<String>) -> Router {
    let Some(auth_token) = auth_token else {
        return Router::new();
    };

    let base_url = base_url.to_string();
    let service = StreamableHttpService::new(
        move || Ok(TaskServer::new(&base_url).without_context()),
        std::sync::Arc::new(LocalSessionManager::default()),
        Default::default(),
    );

    Router::new()
        .nest_service("/mcp", service)
        .layer(ValidateRequestHeaderLayer::custom(
            move |request: &mut Request<_>| validate_bearer_token(request, &auth_token),
        ))
}

fn validate_bearer_token<B>(
    request: &mut Request<B>,
    auth_token: &str,
) -> Result<(), Response<Body>> {
    let expected = format!("Bearer {auth_token}");
    let actual = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    if actual == Some(expected.as_str()) {
        return Ok(());
    }

    Err(Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::from("Unauthorized"))
        .unwrap_or_else(|_| Response::new(Body::from("Unauthorized"))))
}

#[cfg(test)]
mod tests {
    use axum::{Json, routing::get};
    use serde_json::{Value, json};

    use super::*;

    #[tokio::test]
    async fn http_mcp_smoke_test_supports_initialize_and_tool_calls() -> anyhow::Result<()> {
        let token = "mattermost-smoke-token";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{addr}");

        let app = Router::new()
            .merge(router_with_token(&base_url, Some(token.to_string())))
            .route(
                "/api/organizations",
                get(|| async {
                    Json(json!({
                        "success": true,
                        "data": {
                            "organizations": [
                                {
                                    "id": "53ee9868-7403-47cb-91e2-fd7b09deed35",
                                    "name": "Ops",
                                    "slug": "ops",
                                    "is_personal": false
                                }
                            ]
                        }
                    }))
                }),
            );

        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let client = reqwest::Client::new();
        let init = post_mcp(
            &client,
            &base_url,
            token,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "mattermost-smoke-test",
                        "version": "1.0.0"
                    }
                }
            }),
        )
        .await?;

        assert_eq!(init.status, StatusCode::OK);
        let session_id = init
            .session_id
            .clone()
            .expect("initialize should return an MCP session id");
        let init_message = init
            .json_messages
            .last()
            .expect("initialize result missing");
        assert_eq!(init_message["id"], 1);
        assert_eq!(init_message["result"]["serverInfo"]["name"], "vibe-kanban");
        assert_eq!(init_message["result"]["serverInfo"]["version"], "1.0.0");

        let initialized = post_mcp(
            &client,
            &base_url,
            token,
            Some(&session_id),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        )
        .await?;
        assert_eq!(initialized.status, StatusCode::ACCEPTED);

        let tools = post_mcp(
            &client,
            &base_url,
            token,
            Some(&session_id),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
        )
        .await?;

        assert_eq!(tools.status, StatusCode::OK);
        let tools_message = tools
            .json_messages
            .last()
            .expect("tools/list result missing");
        let tool_names: Vec<&str> = tools_message["result"]["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(tool_names.contains(&"list_organizations"));
        assert!(tool_names.contains(&"list_projects"));
        assert!(tool_names.contains(&"list_issues"));
        assert!(tool_names.contains(&"create_issue"));
        assert!(!tool_names.contains(&"get_context"));

        let list_organizations = post_mcp(
            &client,
            &base_url,
            token,
            Some(&session_id),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "list_organizations",
                    "arguments": {}
                }
            }),
        )
        .await?;

        assert_eq!(list_organizations.status, StatusCode::OK);
        let tool_message = list_organizations
            .json_messages
            .last()
            .expect("tools/call result missing");
        let result_text = tool_message["result"]["content"][0]["text"]
            .as_str()
            .expect("tool result text missing");
        let result_json: Value = serde_json::from_str(result_text)?;
        assert_eq!(result_json["count"], 1);
        assert_eq!(result_json["organizations"][0]["name"], "Ops");

        let close_response = client
            .delete(format!("{base_url}/mcp"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("mcp-session-id", session_id)
            .send()
            .await?;
        assert_eq!(close_response.status(), StatusCode::ACCEPTED);

        server.abort();
        let _ = server.await;

        Ok(())
    }

    struct McpHttpResponse {
        status: StatusCode,
        session_id: Option<String>,
        json_messages: Vec<Value>,
    }

    async fn post_mcp(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        session_id: Option<&str>,
        payload: Value,
    ) -> anyhow::Result<McpHttpResponse> {
        let mut request = client
            .post(format!("{base_url}/mcp"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");

        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id);
        }

        let response = request.body(payload.to_string()).send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await?;

        Ok(McpHttpResponse {
            status,
            session_id: headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned),
            json_messages: parse_sse_json_messages(&body)?,
        })
    }

    fn parse_sse_json_messages(body: &str) -> anyhow::Result<Vec<Value>> {
        let mut messages = Vec::new();

        for event in body.split("\n\n").filter(|event| !event.trim().is_empty()) {
            let mut data_lines = Vec::new();
            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    data_lines.push(data.trim_start());
                }
            }

            let data = data_lines.join("\n");
            if data.is_empty() {
                continue;
            }

            messages.push(serde_json::from_str::<Value>(&data)?);
        }

        Ok(messages)
    }
}
