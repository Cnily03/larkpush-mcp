use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use clap::{Args, Parser, Subcommand};
use larkpush_auth as auth;
use larkpush_config::Config;
use larkpush_lark::LarkClient;
use larkpush_mcp::McpServer;
use serde_json::Value;
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

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("larkpush=info,larkpush_auth=info,larkpush_mcp=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(UtcTime::rfc_3339())
        .with_target(true)
        .with_ansi(true)
        .with_writer(std::io::stderr)
        .init();
}

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
    let registry_path = client_registry_path(&args.config, server.client_registry_path.as_deref())?;
    let auth = auth::ServerAuth::new(lark.clone(), server, registry_path)?;
    let mcp = McpServer::new(LarkClient::new(lark)?);
    let mcp_routes = Router::new()
        .route("/mcp", post(mcp_request))
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

fn client_registry_path(config_path: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let path = configured.unwrap_or_else(|| Path::new(".data/oauth-clients.json"));
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    };

    if configured.is_none() {
        let legacy = config_dir.join("oauth-clients.json");
        if !resolved.exists() && legacy.is_file() {
            std::fs::create_dir_all(resolved.parent().context("invalid registry path")?)
                .with_context(|| format!("failed to create {}", resolved.display()))?;
            std::fs::rename(&legacy, &resolved).with_context(|| {
                format!(
                    "failed to move OAuth client registry from {} to {}",
                    legacy.display(),
                    resolved.display()
                )
            })?;
            info!(path = %resolved.display(), "OAuth client registry moved into .data");
        }
    }

    Ok(resolved)
}

async fn mcp_request(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid>")
        .to_owned();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !allowed_host(state.address, host) || !same_origin(&headers, host) {
        warn!(method, "MCP request rejected by host or origin check");
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.auth.authenticate(&headers).await {
        info!(method, "MCP request requires authentication");
        return state.auth.challenge_response();
    }
    match state.mcp.handle(request).await {
        Some(response) => {
            info!(method, "MCP request handled");
            (StatusCode::OK, Json(response)).into_response()
        }
        None => {
            info!(method, "MCP notification handled");
            StatusCode::ACCEPTED.into_response()
        }
    }
}

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

fn same_origin(headers: &HeaderMap, host: &str) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    url::Url::parse(origin).ok().is_some_and(|url| {
        url.host_str().is_some_and(|name| {
            format!("{}:{}", name, url.port_or_known_default().unwrap_or(0)) == host
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, allowed_host, client_registry_path};
    use clap::Parser;

    #[test]
    fn rejects_unexpected_host_on_loopback() {
        let address = "127.0.0.1:3000".parse().unwrap();
        assert!(allowed_host(address, "localhost:3000"));
        assert!(!allowed_host(address, "attacker.example:3000"));
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
    fn default_registry_path_migrates_existing_file_into_data_directory() {
        let directory = std::env::temp_dir().join(format!(
            "larkpush-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let legacy = directory.join("oauth-clients.json");
        std::fs::write(&legacy, b"existing registrations").unwrap();

        let path = client_registry_path(&directory.join("config.toml"), None).unwrap();
        assert_eq!(path, directory.join(".data/oauth-clients.json"));
        assert_eq!(std::fs::read(&path).unwrap(), b"existing registrations");
        assert!(!legacy.exists());

        std::fs::remove_dir_all(directory).unwrap();
    }
}
