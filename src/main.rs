use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, uri::Authority},
    response::{IntoResponse, Response},
    routing::post,
};
use clap::{Args, Parser, Subcommand};
use larkpush::{auth, db::SqliteStore};
use larkpush_config::Config;
use larkpush_lark::LarkClient;
use larkpush_mcp::{McpServer, PROTOCOL_VERSION};
use serde_json::{Value, json};
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt::time::UtcTime};

#[derive(Parser)]
#[command(name = "larkpush", about = "OAuth-protected Lark MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the OAuth-protected MCP server.
    Mcp(McpArgs),
}

#[derive(Args)]
struct McpArgs {
    /// HTTP listen address; omit the value to use [server].listen.
    #[arg(short = 'L', long = "listen", num_args = 0..=1)]
    listen: Option<Option<String>>,

    /// Configuration file containing the required [server] section.
    #[arg(short = 'c', long = "config", default_value = "config.toml")]
    config: PathBuf,
}

#[derive(Clone)]
struct ServerState {
    mcp: McpServer,
    auth: auth::ServerAuth,
    address: SocketAddr,
}

/// Initialize logging, dispatch the CLI command, and return its exit status.
#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let result = match Cli::parse().command {
        Command::Mcp(args) => run_server(args).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(error = %format!("{error:#}"), "MCP server failed");
            ExitCode::FAILURE
        }
    }
}

/// Send timestamped, colored tracing output to stderr.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("larkpush=info,larkpush_mcp=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(UtcTime::rfc_3339())
        .with_target(true)
        .with_ansi(true)
        .with_writer(std::io::stderr)
        .init();
}

/// Load configuration, bind the listener, and serve MCP and OAuth routes.
async fn run_server(args: McpArgs) -> Result<()> {
    let Config { lark, server } = Config::load(&args.config)?;
    info!(config = %args.config.display(), "MCP configuration loaded");
    let address: SocketAddr = args
        .listen
        .flatten()
        .unwrap_or_else(|| server.listen.clone())
        .parse()
        .context("invalid server listen address")?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    let address = listener.local_addr()?;
    let database_path = database_path(&args.config, server.database_path.as_deref());
    let dbapi = Arc::new(SqliteStore::open(&database_path).await?);
    let auth = auth::ServerAuth::new(lark.clone(), server, dbapi);
    info!(path = %database_path.display(), "SQLite database initialized");
    let mcp = McpServer::new(LarkClient::new(lark)?);
    let mcp_routes = Router::new()
        .route("/mcp", post(mcp_request).get(mcp_get))
        .with_state(ServerState {
            mcp,
            auth: auth.clone(),
            address,
        });
    let auth_routes = auth::router(auth);
    info!(address = %address, "MCP server listening");
    axum::serve(listener, mcp_routes.merge(auth_routes))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Resolve a relative SQLite path beside the configuration file.
fn database_path(config_path: &Path, configured: Option<&Path>) -> PathBuf {
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let path = configured.unwrap_or_else(|| Path::new(".data/larkpush.sqlite"));
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

/// Report that this stateless MCP server has no standalone SSE stream.
async fn mcp_get() -> StatusCode {
    info!("MCP GET rejected because SSE is unavailable");
    StatusCode::METHOD_NOT_ALLOWED
}

/// Enforce host, origin, and OAuth checks before dispatching JSON-RPC.
async fn mcp_request(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    info!("MCP POST received");
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !allowed_host(state.address, host) {
        warn!(host, "MCP request rejected by host check");
        return StatusCode::FORBIDDEN.into_response();
    }
    if !same_origin(&headers, host) {
        warn!(host, "MCP request rejected by origin check");
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.auth.authenticate(&headers).await {
        info!("MCP request requires authentication");
        return state.auth.challenge_response();
    }
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            warn!(%error, "MCP request contains invalid JSON");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})),
            )
                .into_response();
        }
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid>")
        .to_owned();
    if method == "server/discover"
        || headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|version| version != PROTOCOL_VERSION)
    {
        // Modern clients recognize an ordinary HTTP 400 as a legacy-server signal.
        info!(method, "MCP version probe requires legacy initialization");
        return StatusCode::BAD_REQUEST.into_response();
    }
    match state.mcp.handle(request).await {
        Some(response) => {
            if let Some(error) = response.get("error") {
                warn!(method, code = ?error.get("code"), "MCP JSON-RPC request rejected");
            } else {
                info!(method, "MCP request handled");
            }
            (StatusCode::OK, Json(response)).into_response()
        }
        None => {
            info!(method, "MCP notification handled");
            StatusCode::ACCEPTED.into_response()
        }
    }
}

/// Restrict the Host header to the bound address when listening on loopback.
fn allowed_host(address: SocketAddr, host: &str) -> bool {
    if !address.ip().is_loopback() {
        return true;
    }
    let port = address.port();
    let ip_host = match address.ip() {
        std::net::IpAddr::V4(ip) => format!("{ip}:{port}"),
        std::net::IpAddr::V6(ip) => format!("[{ip}]:{port}"),
    };
    host == ip_host || host == format!("localhost:{port}")
}

/// Reject an Origin header whose normalized host and port differ from Host.
fn same_origin(headers: &HeaderMap, host: &str) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let (Ok(origin), Ok(host)) = (url::Url::parse(origin), host.parse::<Authority>()) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https")
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        return false;
    }
    origin.host_str().is_some_and(|name| {
        name.eq_ignore_ascii_case(host.host())
            && origin.port_or_known_default()
                == host.port_u16().or_else(|| match origin.scheme() {
                    "http" => Some(80),
                    "https" => Some(443),
                    _ => None,
                })
    })
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, ServerState, allowed_host, database_path, mcp_request, same_origin};
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, HeaderValue, Request, StatusCode},
        routing::post,
    };
    use clap::Parser;
    use larkpush::{auth, db::SqliteStore};
    use larkpush_config::{LarkConfig, ServerConfig};
    use larkpush_dbapi::{DbApi, TokenGrant};
    use larkpush_lark::LarkClient;
    use larkpush_mcp::McpServer;
    use serde_json::{Value, json};
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tower::ServiceExt;

    #[test]
    fn rejects_unexpected_host_on_loopback() {
        let address = "127.0.0.1:3000".parse().unwrap();
        assert!(allowed_host(address, "localhost:3000"));
        assert!(!allowed_host(address, "attacker.example:3000"));
    }

    #[test]
    fn accepts_matching_origin_with_implicit_default_port() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", HeaderValue::from_static("https://example.com"));
        assert!(same_origin(&headers, "example.com"));
        assert!(same_origin(&headers, "example.com:443"));
        assert!(!same_origin(&headers, "example.com:8443"));
        assert!(!same_origin(&headers, "other.example.com"));
    }

    #[tokio::test]
    async fn discovers_tools_after_authentication_and_rejects_modern_probe() {
        let directory = tempfile::tempdir().unwrap();
        let dbapi = Arc::new(
            SqliteStore::open(&directory.path().join("larkpush.sqlite"))
                .await
                .unwrap(),
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        dbapi
            .insert_token_pair(
                "test-access",
                "test-refresh",
                TokenGrant {
                    client_id: "client".into(),
                    open_id: "ou_test".into(),
                    resource: "http://127.0.0.1:3000/mcp".into(),
                },
                now + 3600,
                now + 7200,
            )
            .await
            .unwrap();
        let lark = LarkConfig {
            app_id: "test".into(),
            app_secret: "test".into(),
            base_url: "http://127.0.0.1:3000".into(),
            lark_authorize_url: String::new(),
            lark_token_url: String::new(),
            lark_user_info_url: String::new(),
            lark_scope: String::new(),
        };
        let auth = auth::ServerAuth::new(
            lark.clone(),
            ServerConfig {
                listen: "127.0.0.1:3000".into(),
                public_base_url: "http://127.0.0.1:3000".into(),
                database_path: None,
            },
            dbapi,
        );
        let router = Router::new()
            .route("/mcp", post(mcp_request))
            .with_state(ServerState {
                mcp: McpServer::new(LarkClient::new(lark).unwrap()),
                auth,
                address: "127.0.0.1:3000".parse().unwrap(),
            });
        let probe = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "127.0.0.1:3000")
            .header("authorization", "Bearer test-access")
            .header("mcp-protocol-version", "2026-07-28")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":1,"method":"server/discover"}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(probe).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );

        // Some clients omit Content-Type; this still reaches the JSON-RPC handler.
        let list = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "127.0.0.1:3000")
            .header("origin", "http://127.0.0.1:3000")
            .header("authorization", "Bearer test-access")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
            ))
            .unwrap();
        let response = router.oneshot(list).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn accepts_server_command_and_rejects_old_client_invocation() {
        let cli = Cli::try_parse_from(["larkpush", "mcp", "-L", "-c", "custom.toml"])
            .expect("server command should parse");
        let Command::Mcp(args) = cli.command;
        assert!(args.listen.is_some());
        assert_eq!(args.listen.flatten(), None);
        assert_eq!(args.config.to_string_lossy(), "custom.toml");

        assert!(Cli::try_parse_from(["larkpush"]).is_err());
        assert!(Cli::try_parse_from(["larkpush", "-L"]).is_err());
        assert!(Cli::try_parse_from(["larkpush", "serve"]).is_err());
    }

    #[test]
    fn default_database_path_is_relative_to_config_directory() {
        let directory = std::env::temp_dir().join(format!(
            "larkpush-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = database_path(&directory.join("config.toml"), None);
        assert_eq!(path, directory.join(".data/larkpush.sqlite"));
    }
}
