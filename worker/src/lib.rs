//! Cloudflare's fetch entry point for OAuth discovery and the shared MCP tools.

mod auth;
mod config;
mod db;
mod logging;

use larkpush_lark::LarkClient;
use larkpush_mcp::{McpServer, PROTOCOL_VERSION};
use serde_json::Value;
use worker::{Context, Env, Method, Request, Response, Result, event};

use crate::{config::Config, db::D1Store};

const MISSING_HEADER: &str = "<none>";

/// Routes that need OAuth storage or the Lark client.
enum DatabaseRoute {
    Register,
    Authorize,
    Callback,
    Token,
    Mcp,
}

/// Route each Worker request to OAuth discovery, authorization, or MCP.
#[event(fetch)]
async fn main(mut request: Request, env: Env, _context: Context) -> Result<Response> {
    logging::init();
    let path = request.url()?.path().to_owned();
    let method = request.method();
    log::info!("Worker request received method={method:?} path={path}");

    let result = route(&mut request, &env, &path).await;
    match &result {
        Ok(response) => {
            log::info!(
                "Worker request completed method={method:?} path={path} status={}",
                response.status_code()
            );
        }
        Err(error) => {
            log::error!("Worker request failed method={method:?} path={path} error={error}");
        }
    }
    result
}

/// Initialize database bindings and dispatch the request to its route.
async fn route(request: &mut Request, env: &Env, path: &str) -> Result<Response> {
    let config = Config::from_env(&env)?;
    let database_route = match (request.method(), path) {
        (Method::Get, "/.well-known/oauth-protected-resource")
        | (Method::Get, "/.well-known/oauth-protected-resource/mcp") => {
            return auth::protected_resource_metadata(&config);
        }
        (Method::Get, "/.well-known/oauth-authorization-server") => {
            return auth::metadata(&config);
        }
        (Method::Get, "/mcp") => return Response::error("SSE is not supported", 405),
        (Method::Post, "/register") => DatabaseRoute::Register,
        (Method::Get, "/authorize") => DatabaseRoute::Authorize,
        (Method::Get, "/oauth/feishu/callback") => DatabaseRoute::Callback,
        (Method::Post, "/token") => DatabaseRoute::Token,
        (Method::Post, "/mcp") => DatabaseRoute::Mcp,
        _ => return Response::error("Not found", 404),
    };
    let dbapi = D1Store::new(env.d1("LARKPUSH_DB")?, &config.redis_url).await?;
    match database_route {
        DatabaseRoute::Register => auth::register(request, &config, &dbapi).await,
        DatabaseRoute::Authorize => auth::authorize(request, &config, &dbapi).await,
        DatabaseRoute::Callback => auth::callback(request, &config, &dbapi).await,
        DatabaseRoute::Token => auth::token(request, &config, &dbapi).await,
        DatabaseRoute::Mcp => mcp(request, &config, &dbapi).await,
    }
}

/// Authenticate a request and dispatch its JSON-RPC payload to shared tools.
async fn mcp(request: &mut Request, config: &Config, dbapi: &D1Store) -> Result<Response> {
    let authorization = request.headers().get("authorization")?;
    let bearer_present = authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| !token.is_empty());
    let protocol_version = request.headers().get("mcp-protocol-version")?;
    let origin = request.headers().get("origin")?;
    log::info!(
        "MCP POST received origin={} protocol_version={} bearer_present={bearer_present}",
        origin.as_deref().unwrap_or(MISSING_HEADER),
        protocol_version.as_deref().unwrap_or(MISSING_HEADER)
    );
    if !auth::authenticate(request, config, dbapi).await? {
        log::info!("MCP request requires authentication bearer_present={bearer_present}");
        return auth::challenge(config);
    }
    let payload: Value = match request.json().await {
        Ok(value) => value,
        Err(_) => {
            log::warn!("MCP request contains invalid JSON");
            return Response::error("Invalid JSON", 400);
        }
    };
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<invalid>")
        .to_owned();
    let tool = payload
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(MISSING_HEADER)
        .to_owned();
    if method == "server/discover"
        || protocol_version
            .as_deref()
            .is_some_and(|version| version != PROTOCOL_VERSION)
    {
        // Signal legacy transport so modern clients can retry with initialize.
        log::info!("MCP version probe requires legacy initialization method={method}");
        return Ok(Response::empty()?.with_status(400));
    }
    let client = LarkClient::new(config.lark.clone())
        .map_err(|error| worker::Error::RustError(error.to_string()))?;
    let server = McpServer::new(client);
    match server.handle(payload).await {
        Some(response) => {
            if let Some(error) = response.get("error") {
                log::warn!(
                    "MCP JSON-RPC request rejected method={method} tool={tool} error={error}"
                );
            } else {
                log::info!("MCP request handled method={method} tool={tool}");
            }
            Response::from_json(&response)
        }
        None => {
            log::info!("MCP notification handled method={method}");
            Ok(Response::empty()?.with_status(202))
        }
    }
}
