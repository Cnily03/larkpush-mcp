use larkpush_lark::LarkClient;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::tools;

/// Legacy MCP revision supported by the JSON-RPC handler.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone)]
pub struct McpServer {
    client: LarkClient,
}

impl McpServer {
    /// Attach a Lark client to the JSON-RPC protocol handler.
    pub fn new(client: LarkClient) -> Self {
        Self { client }
    }

    /// Returns None for JSON-RPC notifications, which have no response body.
    pub async fn handle(&self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str);
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
            return Some(error(id.unwrap_or(Value::Null), -32600, "Invalid Request"));
        }
        let id = id?;
        if !id.is_string() && !id.is_number() {
            return Some(error(Value::Null, -32600, "Invalid id"));
        }
        let result = match method.unwrap() {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "larkpush", "version": env!("CARGO_PKG_VERSION")}
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools::list()}),
            "tools/call" => {
                let params = request.get("params").unwrap_or(&Value::Null);
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error(id, -32602, "Missing tool name"));
                };
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    return Some(error(id, -32602, "Tool arguments must be an object"));
                }
                match tools::call(&self.client, name, args).await {
                    Ok(data) => {
                        info!(tool = name, "MCP tool completed");
                        json!({"content": [{"type":"text","text":data.to_string()}], "structuredContent": data})
                    }
                    Err(err) => {
                        warn!(tool = name, error = %err, "MCP tool failed");
                        json!({"content": [{"type":"text","text":err.to_string()}], "isError": true})
                    }
                }
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

/// Build a JSON-RPC error response with the original request ID.
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use larkpush_config::LarkConfig;
    use larkpush_lark::LarkClient;
    use serde_json::json;

    use super::McpServer;

    fn server() -> McpServer {
        McpServer::new(
            LarkClient::new(LarkConfig {
                app_id: "test".into(),
                app_secret: "test".into(),
                base_url: "http://localhost".into(),
                lark_authorize_url: String::new(),
                lark_token_url: String::new(),
                lark_user_info_url: String::new(),
                lark_scope: String::new(),
            })
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn lists_expected_tools() {
        let response = server()
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let names: Vec<_> = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"send_as_app"));
        assert!(names.contains(&"send_multi_users"));
        assert!(!names.contains(&"send_as_user"));
        assert!(names.contains(&"upload_image"));

        let tools = response["result"]["tools"].as_array().unwrap();
        let list_users = tools
            .iter()
            .find(|tool| tool["name"] == "list_users")
            .unwrap();
        let search_users = tools
            .iter()
            .find(|tool| tool["name"] == "search_users")
            .unwrap();
        assert_eq!(list_users["inputSchema"]["required"], json!([]));
        assert_eq!(search_users["inputSchema"]["required"], json!(["query"]));
        assert!(
            list_users["inputSchema"]["properties"]
                .get("department_id")
                .is_none()
        );
        assert!(
            search_users["inputSchema"]["properties"]
                .get("department_id")
                .is_none()
        );
    }

    #[tokio::test]
    async fn notifications_do_not_receive_responses() {
        assert!(
            server()
                .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .is_none()
        );
    }
}
