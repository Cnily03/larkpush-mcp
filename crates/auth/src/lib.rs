mod refresh;
mod registry;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use larkpush_config::{LarkConfig, ServerConfig};
use rand::{RngCore, rngs::OsRng};
use refresh::RefreshRegistry;
use registry::ClientRegistry;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tracing::{info, warn};
use url::Url;

const LOGIN_TTL: Duration = Duration::from_secs(300);
const TOKEN_TTL: Duration = Duration::from_secs(3600);
const LARK_TIMEOUT: Duration = Duration::from_secs(15);
const LARK_CALLBACK_PATH: &str = "/oauth/feishu/callback";
const MCP_SCOPE: &str = "mcp:tools";

#[derive(Clone)]
pub struct ServerAuth {
    lark: LarkConfig,
    server: ServerConfig,
    http: reqwest::Client,
    state: Arc<Mutex<AuthStore>>,
}

struct AuthStore {
    clients: ClientRegistry,
    refresh: RefreshRegistry,
    pending: HashMap<String, Pending>,
    codes: HashMap<String, Code>,
}

struct Pending {
    client_id: String,
    redirect_uri: String,
    client_state: String,
    challenge: String,
    expires_at: Instant,
}

struct Code {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    open_id: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct Registration {
    redirect_uris: Vec<String>,
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    client_id: String,
    redirect_uri: String,
    response_type: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: String,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct LarkCallback {
    code: Option<String>,
    state: String,
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    resource: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn oauth_error(status: StatusCode, message: &str) -> Response {
    oauth_error_with_code(status, "invalid_request", message)
}

fn oauth_error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    if status != StatusCode::UNAUTHORIZED {
        warn!(status = %status, reason = message, "OAuth request rejected");
    }
    (
        status,
        Json(json!({"error":code,"error_description":message})),
    )
        .into_response()
}

impl ServerAuth {
    pub fn new(lark: LarkConfig, server: ServerConfig, registry_path: PathBuf) -> Result<Self> {
        let issuer = server.public_base_url.trim_end_matches('/').to_owned();
        let refresh_path = registry_path.with_file_name("oauth-refresh-tokens.json");
        let clients = ClientRegistry::load(registry_path, issuer.clone())?;
        let refresh = RefreshRegistry::load(refresh_path, issuer)?;
        info!(clients = clients.len(), "OAuth client registry loaded");
        Ok(Self::with_registries(lark, server, clients, refresh))
    }

    #[cfg(test)]
    fn with_clients(lark: LarkConfig, server: ServerConfig, clients: ClientRegistry) -> Self {
        let issuer = server.public_base_url.trim_end_matches('/').to_owned();
        Self::with_registries(lark, server, clients, RefreshRegistry::in_memory(issuer))
    }

    fn with_registries(
        lark: LarkConfig,
        server: ServerConfig,
        clients: ClientRegistry,
        refresh: RefreshRegistry,
    ) -> Self {
        Self {
            lark,
            server,
            http: reqwest::Client::builder()
                .timeout(LARK_TIMEOUT)
                .build()
                .expect("valid Lark HTTP client configuration"),
            state: Arc::new(Mutex::new(AuthStore {
                clients,
                refresh,
                pending: HashMap::new(),
                codes: HashMap::new(),
            })),
        }
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.server.public_base_url.trim_end_matches('/'))
    }

    fn metadata(&self) -> Value {
        let origin = self.server.public_base_url.trim_end_matches('/');
        json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "registration_endpoint": format!("{origin}/register"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": [MCP_SCOPE]
        })
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(token) = value.strip_prefix("Bearer ") else {
            return false;
        };
        let state = self.state.lock().await;
        state
            .refresh
            .access_open_id(token)
            .is_some_and(|open_id| !open_id.is_empty())
    }

    pub fn challenge_response(&self) -> Response {
        let metadata = format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            self.server.public_base_url.trim_end_matches('/')
        );
        let mut response = oauth_error(StatusCode::UNAUTHORIZED, "Authorization required");
        response.headers_mut().insert(
            "www-authenticate",
            format!("Bearer resource_metadata=\"{metadata}\", scope=\"{MCP_SCOPE}\"")
                .parse()
                .expect("valid challenge header"),
        );
        response
    }
}

pub fn router(auth: ServerAuth) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_metadata),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route(LARK_CALLBACK_PATH, get(lark_callback))
        .route("/token", post(token))
        .with_state(auth)
}

async fn resource_metadata(State(auth): State<ServerAuth>) -> Json<Value> {
    Json(
        json!({"resource":auth.resource(),"authorization_servers":[auth.server.public_base_url],"scopes_supported":[MCP_SCOPE],"bearer_methods_supported":["header"]}),
    )
}

async fn authorization_metadata(State(auth): State<ServerAuth>) -> Json<Value> {
    Json(auth.metadata())
}

async fn register(State(auth): State<ServerAuth>, Json(request): Json<Registration>) -> Response {
    if request.redirect_uris.is_empty()
        || request.redirect_uris.len() > 10
        || !request
            .redirect_uris
            .iter()
            .all(|uri| valid_redirect_uri(uri))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "one to ten HTTPS or loopback redirect_uris are required",
        );
    }
    let client_id = random_secret();
    if let Err(error) = auth
        .state
        .lock()
        .await
        .clients
        .register(client_id.clone(), request.redirect_uris.clone())
        .await
    {
        warn!(%error, "OAuth client registration could not be persisted");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    info!("OAuth client registered");
    (StatusCode::CREATED, Json(json!({"client_id":client_id,"redirect_uris":request.redirect_uris,"token_endpoint_auth_method":"none"}))).into_response()
}

async fn authorize(
    State(auth): State<ServerAuth>,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    if query.response_type != "code"
        || query.code_challenge_method != "S256"
        || query.state.is_empty()
        || query.code_challenge.len() < 40
        || query.resource != auth.resource()
        || query
            .scope
            .as_deref()
            .is_some_and(|scope| scope != MCP_SCOPE)
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid authorization request");
    }
    let mut state = auth.state.lock().await;
    match state.clients.redirect_uris(&query.client_id) {
        None => return oauth_error(StatusCode::BAD_REQUEST, "unregistered client"),
        Some(uris) if !uris.contains(&query.redirect_uri) => {
            return oauth_error(StatusCode::BAD_REQUEST, "unregistered redirect_uri");
        }
        Some(_) => {}
    }
    state
        .pending
        .retain(|_, item| item.expires_at > Instant::now());
    let login_state = random_secret();
    state.pending.insert(
        login_state.clone(),
        Pending {
            client_id: query.client_id,
            redirect_uri: query.redirect_uri,
            client_state: query.state,
            challenge: query.code_challenge,
            expires_at: Instant::now() + LOGIN_TTL,
        },
    );
    drop(state);
    let Ok(mut url) = Url::parse(&auth.lark.lark_authorize_url) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    url.query_pairs_mut()
        .append_pair("client_id", &auth.lark.app_id)
        .append_pair("response_type", "code")
        .append_pair(
            "redirect_uri",
            &format!(
                "{}{LARK_CALLBACK_PATH}",
                auth.server.public_base_url.trim_end_matches('/')
            ),
        )
        .append_pair("state", &login_state);
    if !auth.lark.lark_scope.is_empty() {
        url.query_pairs_mut()
            .append_pair("scope", &auth.lark.lark_scope);
    }
    info!("Lark login initiated");
    Redirect::to(url.as_str()).into_response()
}

async fn lark_callback(
    State(auth): State<ServerAuth>,
    Query(query): Query<LarkCallback>,
) -> Response {
    let pending = auth.state.lock().await.pending.remove(&query.state);
    let Some(pending) = pending.filter(|item| item.expires_at > Instant::now()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid or expired login state");
    };
    let Some(code) = query.code else {
        return oauth_error(StatusCode::BAD_REQUEST, "Lark login was denied");
    };
    let redirect_uri = format!(
        "{}{LARK_CALLBACK_PATH}",
        auth.server.public_base_url.trim_end_matches('/')
    );
    let response = auth
        .http
        .post(&auth.lark.lark_token_url)
        .json(&json!({
            "grant_type":"authorization_code", "client_id":auth.lark.app_id,
            "client_secret":auth.lark.app_secret, "code":code, "redirect_uri":redirect_uri
        }))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "Lark token request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let token_data = match response.json::<Value>().await {
        Ok(data) => data,
        Err(error) => {
            warn!(%error, "Lark token response was invalid");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    if token_data["code"].as_i64().is_some_and(|value| value != 0) {
        return oauth_error(StatusCode::BAD_GATEWAY, "Lark token exchange failed");
    }
    let Some(user_token) = token_data["access_token"]
        .as_str()
        .or_else(|| token_data["data"]["access_token"].as_str())
    else {
        return oauth_error(
            StatusCode::BAD_GATEWAY,
            "Lark did not return an access token",
        );
    };
    let response = auth
        .http
        .get(&auth.lark.lark_user_info_url)
        .bearer_auth(user_token)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "Lark user lookup request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let user = match response.json::<Value>().await {
        Ok(user) => user,
        Err(error) => {
            warn!(%error, "Lark user lookup response was invalid");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    if user["code"].as_i64() != Some(0) {
        return oauth_error(StatusCode::BAD_GATEWAY, "Lark user lookup failed");
    }
    let Some(open_id) = user["data"]["open_id"].as_str() else {
        return oauth_error(StatusCode::BAD_GATEWAY, "Lark did not return an open_id");
    };
    let code = random_secret();
    let mut state = auth.state.lock().await;
    state
        .codes
        .retain(|_, item| item.expires_at > Instant::now());
    state.codes.insert(
        code.clone(),
        Code {
            client_id: pending.client_id,
            redirect_uri: pending.redirect_uri.clone(),
            challenge: pending.challenge,
            open_id: open_id.to_owned(),
            expires_at: Instant::now() + LOGIN_TTL,
        },
    );
    let Ok(mut destination) = Url::parse(&pending.redirect_uri) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    destination
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &pending.client_state);
    info!("Lark login completed");
    Redirect::to(destination.as_str()).into_response()
}

async fn token(State(auth): State<ServerAuth>, Form(request): Form<TokenRequest>) -> Response {
    match request.grant_type.as_str() {
        "authorization_code" => exchange_code(&auth, request).await,
        "refresh_token" => refresh_token(&auth, request).await,
        _ => oauth_error_with_code(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "unsupported grant type",
        ),
    }
}

async fn exchange_code(auth: &ServerAuth, request: TokenRequest) -> Response {
    let Some(verifier) = request.code_verifier.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "missing PKCE verifier");
    };
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        || request.resource.as_deref() != Some(auth.resource().as_str())
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid verifier or resource");
    }
    let Some(request_code) = request.code.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "missing authorization code");
    };
    let mut state = auth.state.lock().await;
    let Some(code) = state
        .codes
        .remove(request_code)
        .filter(|item| item.expires_at > Instant::now())
    else {
        return oauth_error_with_code(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid or expired code",
        );
    };
    if code.client_id != request.client_id
        || Some(code.redirect_uri.as_str()) != request.redirect_uri.as_deref()
        || challenge(verifier)
            .as_bytes()
            .ct_eq(code.challenge.as_bytes())
            .unwrap_u8()
            != 1
    {
        return oauth_error_with_code(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid client, redirect_uri, or PKCE verifier",
        );
    }
    let (access, refresh) = match state
        .refresh
        .issue(code.client_id, code.open_id.clone())
        .await
    {
        Ok(token) => token,
        Err(error) => {
            warn!(%error, "OAuth refresh token could not be persisted");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    token_response(access, refresh)
}

async fn refresh_token(auth: &ServerAuth, request: TokenRequest) -> Response {
    if request
        .resource
        .as_deref()
        .is_some_and(|resource| resource != auth.resource())
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid resource");
    }
    let Some(previous) = request.refresh_token.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "missing refresh token");
    };
    let mut state = auth.state.lock().await;
    let Some(grant) = state.refresh.get(previous) else {
        return oauth_error_with_code(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid or expired refresh token",
        );
    };
    if grant.client_id != request.client_id
        || state.clients.redirect_uris(&request.client_id).is_none()
    {
        return oauth_error_with_code(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token is not valid for this client or user",
        );
    }
    let (access, refresh) = match state.refresh.rotate(previous).await {
        Ok(Some(result)) => result,
        Ok(None) => {
            return oauth_error_with_code(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "invalid or expired refresh token",
            );
        }
        Err(error) => {
            warn!(%error, "OAuth refresh token rotation could not be persisted");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    token_response(access, refresh)
}

fn token_response(access_token: String, refresh_token: String) -> Response {
    info!("MCP bearer token issued");
    (
        StatusCode::OK,
        Json(json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": TOKEN_TTL.as_secs(),
            "refresh_token": refresh_token,
            "scope": MCP_SCOPE,
        })),
    )
        .into_response()
}

fn valid_redirect_uri(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let secure = url.scheme() == "https" && url.host_str().is_some();
    let loopback = url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        && url.port().is_some();
    (secure || loopback)
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

#[cfg(test)]
mod tests {
    use super::registry::ClientRegistry;
    use super::{
        AuthorizeQuery, Code, LARK_CALLBACK_PATH, Registration, ServerAuth, TokenRequest,
        authorize, challenge, register, router, token, valid_redirect_uri,
    };
    use axum::{
        Form, Json,
        body::{Body, to_bytes},
        extract::{Query, State},
        http::{HeaderMap, Request, StatusCode},
    };
    use larkpush_config::{LarkConfig, ServerConfig};
    use serde_json::Value;
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    fn auth() -> ServerAuth {
        let lark = LarkConfig {
            app_id: "app".into(),
            app_secret: "secret".into(),
            base_url: "https://open.feishu.cn/open-apis".into(),
            lark_authorize_url: "https://accounts.feishu.cn/open-apis/authen/v1/authorize".into(),
            lark_token_url: "https://accounts.feishu.cn/oauth/v3/token".into(),
            lark_user_info_url: "https://open.feishu.cn/open-apis/authen/v1/user_info".into(),
            lark_scope: String::new(),
        };
        let server = ServerConfig {
            listen: "127.0.0.1:3000".into(),
            public_base_url: "http://127.0.0.1:3000".into(),
            client_registry_path: None,
        };
        ServerAuth::with_clients(
            lark,
            server,
            ClientRegistry::in_memory("http://127.0.0.1:3000".into()),
        )
    }

    #[test]
    fn redirect_requires_https_or_loopback() {
        assert!(valid_redirect_uri("http://127.0.0.1:4567/callback"));
        assert!(valid_redirect_uri("http://[::1]:4567/callback"));
        assert!(valid_redirect_uri(
            "https://chatgpt.com/connector/oauth/callback-id"
        ));
        assert!(!valid_redirect_uri("http://example.com:4567/callback"));
        assert!(!valid_redirect_uri("https://example.com/callback#fragment"));
    }

    #[test]
    fn pkce_s256_matches_known_value() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[tokio::test]
    async fn router_exposes_metadata_and_registered_callback() {
        let metadata = router(auth())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-authorization-server")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metadata.status(), StatusCode::OK);
        let metadata_body = to_bytes(metadata.into_body(), 4096).await.unwrap();
        let metadata: Value = serde_json::from_slice(&metadata_body).unwrap();
        assert_eq!(metadata["issuer"], "http://127.0.0.1:3000");
        assert_eq!(
            metadata["authorization_endpoint"],
            "http://127.0.0.1:3000/authorize"
        );
        assert_eq!(metadata["token_endpoint"], "http://127.0.0.1:3000/token");
        assert_eq!(
            metadata["grant_types_supported"],
            serde_json::json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(
            metadata["registration_endpoint"],
            "http://127.0.0.1:3000/register"
        );
        assert_eq!(
            metadata["scopes_supported"],
            serde_json::json!(["mcp:tools"])
        );

        let resource = router(auth())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-protected-resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resource.status(), StatusCode::OK);
        let resource_body = to_bytes(resource.into_body(), 4096).await.unwrap();
        let resource: Value = serde_json::from_slice(&resource_body).unwrap();
        assert_eq!(resource["resource"], "http://127.0.0.1:3000/mcp");
        assert_eq!(
            resource["authorization_servers"],
            serde_json::json!(["http://127.0.0.1:3000"])
        );
        assert_eq!(
            resource["scopes_supported"],
            serde_json::json!(["mcp:tools"])
        );

        let scoped_resource = router(auth())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-protected-resource/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(scoped_resource.status(), StatusCode::OK);
        let scoped_body = to_bytes(scoped_resource.into_body(), 4096).await.unwrap();
        let scoped_resource: Value = serde_json::from_slice(&scoped_body).unwrap();
        assert_eq!(scoped_resource, resource);

        let callback = router(auth())
            .oneshot(
                Request::builder()
                    .uri(format!("{LARK_CALLBACK_PATH}?state=invalid"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(callback.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn bearer_challenge_advertises_discovery_and_scope() {
        let challenge = auth().challenge_response();
        assert_eq!(challenge.status(), StatusCode::UNAUTHORIZED);
        let header = challenge.headers()["www-authenticate"].to_str().unwrap();
        assert!(header.contains(
            "resource_metadata=\"http://127.0.0.1:3000/.well-known/oauth-protected-resource/mcp\""
        ));
        assert!(header.contains("scope=\"mcp:tools\""));
    }

    #[tokio::test]
    async fn registration_accepts_https_callbacks_and_binds_redirect_uris() {
        let auth = auth();
        let rejected = register(
            State(auth.clone()),
            Json(Registration {
                redirect_uris: vec!["http://example.com/callback".into()],
            }),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

        let redirect_uri = "https://chatgpt.com/connector/oauth/callback-id";
        let accepted = register(
            State(auth.clone()),
            Json(Registration {
                redirect_uris: vec![redirect_uri.into(), "http://127.0.0.1:4567/callback".into()],
            }),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::CREATED);
        let body = to_bytes(accepted.into_body(), 4096).await.unwrap();
        let registration: Value = serde_json::from_slice(&body).unwrap();
        let client_id = registration["client_id"].as_str().unwrap();

        let request = |redirect_uri: &str| AuthorizeQuery {
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            response_type: "code".into(),
            state: "client-state".into(),
            code_challenge: challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            code_challenge_method: "S256".into(),
            resource: auth.resource(),
            scope: Some("mcp:tools".into()),
        };
        let authorized = authorize(State(auth.clone()), Query(request(redirect_uri))).await;
        assert_eq!(authorized.status(), StatusCode::SEE_OTHER);

        let unregistered = authorize(
            State(auth.clone()),
            Query(request("https://other.example/callback")),
        )
        .await;
        assert_eq!(unregistered.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(unregistered.into_body(), 4096).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error_description"], "unregistered redirect_uri");

        let unknown_client = authorize(
            State(auth.clone()),
            Query(AuthorizeQuery {
                client_id: "unknown-client".into(),
                ..request(redirect_uri)
            }),
        )
        .await;
        assert_eq!(unknown_client.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(unknown_client.into_body(), 4096).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error_description"], "unregistered client");
    }

    #[tokio::test]
    async fn registered_clients_survive_server_restarts() {
        let template = auth();
        let directory =
            std::env::temp_dir().join(format!("larkpush-clients-{}", super::random_secret()));
        let path = directory.join(".data/oauth-clients.json");
        let first = ServerAuth::new(template.lark.clone(), template.server.clone(), path.clone())
            .expect("empty registry should load");
        let redirect_uri = "https://chatgpt.com/connector/oauth/callback-id";
        let registered = register(
            State(first.clone()),
            Json(Registration {
                redirect_uris: vec![redirect_uri.into()],
            }),
        )
        .await;
        assert_eq!(registered.status(), StatusCode::CREATED);
        let body = to_bytes(registered.into_body(), 4096).await.unwrap();
        let registration: Value = serde_json::from_slice(&body).unwrap();
        let client_id = registration["client_id"].as_str().unwrap();
        let (access, refresh) = first
            .state
            .lock()
            .await
            .refresh
            .issue(client_id.into(), "ou_test".into())
            .await
            .unwrap();
        let stored =
            std::fs::read_to_string(directory.join(".data/oauth-refresh-tokens.json")).unwrap();
        assert!(!stored.contains(&refresh));
        assert!(!stored.contains(&access));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.join(".data/oauth-refresh-tokens.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let restarted = ServerAuth::new(template.lark, template.server, path.clone())
            .expect("persisted registry should load");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {access}").parse().unwrap());
        assert!(restarted.authenticate(&headers).await);
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", client_id)
            .append_pair("refresh_token", &refresh)
            .append_pair("resource", &restarted.resource())
            .finish();
        let refreshed = router(restarted.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refreshed.status(), StatusCode::OK);
        let body = to_bytes(refreshed.into_body(), 4096).await.unwrap();
        let token_response: Value = serde_json::from_slice(&body).unwrap();
        assert!(token_response["refresh_token"].as_str().is_some());
        let response = authorize(
            State(restarted.clone()),
            Query(AuthorizeQuery {
                client_id: client_id.into(),
                redirect_uri: redirect_uri.into(),
                response_type: "code".into(),
                state: "client-state".into(),
                code_challenge: challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
                code_challenge_method: "S256".into(),
                resource: restarted.resource(),
                scope: Some("mcp:tools".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn exchanges_single_use_pkce_code_for_bearer_token() {
        let auth = auth();
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        {
            let mut state = auth.state.lock().await;
            state
                .clients
                .register(
                    "registered-client".into(),
                    vec!["http://127.0.0.1:4567/callback".into()],
                )
                .await
                .unwrap();
            state.codes.insert(
                "one-time-code".into(),
                Code {
                    client_id: "registered-client".into(),
                    redirect_uri: "http://127.0.0.1:4567/callback".into(),
                    challenge: challenge(verifier),
                    open_id: "ou_test".into(),
                    expires_at: Instant::now() + Duration::from_secs(60),
                },
            );
        }
        let request = || TokenRequest {
            grant_type: "authorization_code".into(),
            code: Some("one-time-code".into()),
            client_id: "registered-client".into(),
            redirect_uri: Some("http://127.0.0.1:4567/callback".into()),
            code_verifier: Some(verifier.into()),
            resource: Some(auth.resource()),
            refresh_token: None,
        };
        let response = token(State(auth.clone()), Form(request())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let access_token = body["access_token"].as_str().unwrap();
        let original_refresh = body["refresh_token"].as_str().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {access_token}").parse().unwrap(),
        );
        assert!(auth.authenticate(&headers).await);

        let replay = token(State(auth.clone()), Form(request())).await;
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

        let refresh_request = |client_id: &str, refresh_token: &str| TokenRequest {
            grant_type: "refresh_token".into(),
            client_id: client_id.into(),
            resource: Some(auth.resource()),
            code: None,
            redirect_uri: None,
            code_verifier: None,
            refresh_token: Some(refresh_token.into()),
        };
        let wrong_client = token(
            State(auth.clone()),
            Form(refresh_request("another-client", original_refresh)),
        )
        .await;
        assert_eq!(wrong_client.status(), StatusCode::BAD_REQUEST);

        let refreshed = token(
            State(auth.clone()),
            Form(refresh_request("registered-client", original_refresh)),
        )
        .await;
        assert_eq!(refreshed.status(), StatusCode::OK);
        let body = to_bytes(refreshed.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let new_refresh = body["refresh_token"].as_str().unwrap();
        assert_ne!(new_refresh, original_refresh);
        assert_ne!(body["access_token"], access_token);

        let reused = token(
            State(auth.clone()),
            Form(refresh_request("registered-client", original_refresh)),
        )
        .await;
        assert_eq!(reused.status(), StatusCode::BAD_REQUEST);
    }
}
