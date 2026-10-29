//! Axum HTTP adapter for the shared OAuth workflow.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use larkpush_auth::{
    AuthError, AuthFlow, AuthorizeRequest, LarkCallback, Registration, TokenRequest,
    authorization_metadata, bearer_challenge, lark_access_token, lark_open_id, lark_token_request,
    resource, resource_metadata,
};
use larkpush_config::{LarkConfig, ServerConfig};
use larkpush_dbapi::DbApi;
use rand::{RngCore, rngs::OsRng};
use serde_json::{Value, json};
use tracing::{info, warn};
use url::Url;

const LARK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
/// Native OAuth adapter with Lark HTTP access and a shared database API.
pub struct ServerAuth {
    lark: LarkConfig,
    server: ServerConfig,
    http: reqwest::Client,
    dbapi: Arc<dyn DbApi<Error = anyhow::Error>>,
}

impl ServerAuth {
    /// Create the native adapter and its bounded-time Lark HTTP client.
    pub fn new(
        lark: LarkConfig,
        server: ServerConfig,
        dbapi: Arc<dyn DbApi<Error = anyhow::Error>>,
    ) -> Self {
        Self {
            lark,
            server,
            http: reqwest::Client::builder()
                .timeout(LARK_TIMEOUT)
                .build()
                .expect("valid Lark HTTP client configuration"),
            dbapi,
        }
    }

    /// Bind the shared OAuth workflow to this server and database adapter.
    fn flow(&self) -> AuthFlow<'_, dyn DbApi<Error = anyhow::Error>> {
        AuthFlow::new(self.dbapi.as_ref(), &self.server.public_base_url)
    }

    /// Return the resource identifier advertised by this MCP server.
    pub fn resource(&self) -> String {
        resource(&self.server.public_base_url)
    }

    /// Validate a Bearer header; log storage failures and deny the request.
    pub async fn authenticate(&self, headers: &HeaderMap) -> bool {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        match self.flow().authenticate(authorization).await {
            Ok(authenticated) => authenticated,
            Err(error) => {
                warn!(detail = ?error.detail, "OAuth access token lookup failed");
                false
            }
        }
    }

    /// Return a 401 response pointing clients to protected-resource discovery.
    pub fn challenge_response(&self) -> Response {
        let mut response = oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "Authorization required",
        );
        response.headers_mut().insert(
            "www-authenticate",
            bearer_challenge(&self.server.public_base_url)
                .parse()
                .expect("valid challenge header"),
        );
        response
    }
}

/// Register the native OAuth discovery, registration, login, and token routes.
pub fn router(auth: ServerAuth) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route(larkpush_auth::CALLBACK_PATH, get(lark_callback))
        .route("/token", post(token))
        .with_state(auth)
}

/// Serve the MCP resource metadata used during OAuth discovery.
async fn protected_resource_metadata(State(auth): State<ServerAuth>) -> Json<Value> {
    Json(resource_metadata(&auth.server.public_base_url))
}

/// Serve the OAuth authorization-server metadata.
async fn metadata(State(auth): State<ServerAuth>) -> Json<Value> {
    Json(authorization_metadata(&auth.server.public_base_url))
}

/// Register a client after the shared workflow validates its redirect URIs.
async fn register(State(auth): State<ServerAuth>, Json(request): Json<Registration>) -> Response {
    match auth.flow().register(request, random_secret()).await {
        Ok(client) => {
            info!("OAuth client registered");
            (
                StatusCode::CREATED,
                Json(json!({
                    "client_id": client.client_id,
                    "redirect_uris": client.redirect_uris,
                    "token_endpoint_auth_method": "none"
                })),
            )
                .into_response()
        }
        Err(error) => auth_error(error),
    }
}

/// Keep callback paths and query parameters out of login logs.
fn redirect_origin(redirect_uri: &str) -> String {
    Url::parse(redirect_uri)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default()
}

/// Start a login and redirect the browser to Lark.
async fn authorize(
    State(auth): State<ServerAuth>,
    Query(request): Query<AuthorizeRequest>,
) -> Response {
    let client_id = request.client_id.clone();
    let redirect_origin = redirect_origin(&request.redirect_uri);
    match auth
        .flow()
        .begin_authorize(request, &auth.lark, random_secret(), now_seconds())
        .await
    {
        Ok(destination) => {
            info!(%client_id, %redirect_origin, "Lark login initiated");
            Redirect::to(destination.as_str()).into_response()
        }
        Err(error) => auth_error(error),
    }
}

/// Exchange Lark credentials and redirect back with a one-time OAuth code.
async fn lark_callback(
    State(auth): State<ServerAuth>,
    Query(query): Query<LarkCallback>,
) -> Response {
    let pending = match auth.flow().take_pending(&query.state).await {
        Ok(pending) => pending,
        Err(error) => return auth_error(error),
    };
    let Some(code) = query.code else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "access_denied",
            "Lark login was denied",
        );
    };
    let response = auth
        .http
        .post(&auth.lark.lark_token_url)
        .json(&lark_token_request(
            &auth.lark,
            &auth.server.public_base_url,
            &code,
        ))
        .send()
        .await;
    let token_data: Value = match response {
        Ok(response) => match response.json().await {
            Ok(data) => data,
            Err(error) => {
                warn!(%error, "Lark token response was invalid");
                return StatusCode::BAD_GATEWAY.into_response();
            }
        },
        Err(error) => {
            warn!(%error, "Lark token request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let user_token = match lark_access_token(&token_data) {
        Ok(token) => token,
        Err(error) => return auth_error(error),
    };
    let response = auth
        .http
        .get(&auth.lark.lark_user_info_url)
        .bearer_auth(user_token)
        .send()
        .await;
    let user: Value = match response {
        Ok(response) => match response.json().await {
            Ok(data) => data,
            Err(error) => {
                warn!(%error, "Lark user lookup response was invalid");
                return StatusCode::BAD_GATEWAY.into_response();
            }
        },
        Err(error) => {
            warn!(%error, "Lark user lookup request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let open_id = match lark_open_id(&user) {
        Ok(open_id) => open_id,
        Err(error) => return auth_error(error),
    };
    let client_id = pending.client_id.clone();
    let redirect_origin = redirect_origin(&pending.redirect_uri);
    match auth
        .flow()
        .finish_login(pending, open_id, random_secret(), now_seconds())
        .await
    {
        Ok(destination) => {
            info!(%client_id, %redirect_origin, %open_id, "Lark login completed");
            Redirect::to(destination.as_str()).into_response()
        }
        Err(error) => auth_error(error),
    }
}

/// Exchange a code or rotate a refresh token and disable response caching.
async fn token(State(auth): State<ServerAuth>, Form(request): Form<TokenRequest>) -> Response {
    let flow = auth.flow();
    let access = random_secret();
    let refresh = random_secret();
    let now = now_seconds();
    let grant_type = request.grant_type.clone();
    let result = match request.grant_type.as_str() {
        "authorization_code" => flow.exchange_code(request, access, refresh, now).await,
        "refresh_token" => flow.refresh_token(request, access, refresh, now).await,
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "unsupported grant type",
            );
        }
    };
    match result {
        Ok(value) => {
            info!(grant_type, "OAuth token issued");
            let mut response = Json(value).into_response();
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
        Err(error) => auth_error(error),
    }
}

/// Return Unix time in seconds for OAuth expiration calculations.
fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Generate a cryptographically random, URL-safe OAuth credential.
fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Log internal details and convert a shared OAuth error to an HTTP response.
fn auth_error(error: AuthError) -> Response {
    if error.status != 401 {
        warn!(status = error.status, reason = error.description, detail = ?error.detail, "OAuth request rejected");
    }
    oauth_error(
        StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        error.code,
        error.description,
    )
}

/// Serialize an OAuth error without exposing internal diagnostics.
fn oauth_error(status: StatusCode, code: &str, description: &str) -> Response {
    (
        status,
        Json(json!({"error": code, "error_description": description})),
    )
        .into_response()
}
