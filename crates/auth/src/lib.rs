//! Framework-independent OAuth rules and database workflow shared by both servers.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use larkpush_config::LarkConfig;
use larkpush_dbapi::{AuthorizationCode, Client, DbApi, PendingLogin, TokenGrant};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

/// Lifetime of login states and authorization codes, in seconds.
pub const LOGIN_TTL: i64 = 300;
/// Lifetime of access tokens, in seconds.
pub const ACCESS_TTL: i64 = 3600;
/// Lifetime of refresh tokens, in seconds.
pub const REFRESH_TTL: i64 = 90 * 24 * 60 * 60;
/// Scope advertised by the MCP OAuth server.
pub const SCOPE: &str = "mcp:tools";
/// Browser callback path registered with the Lark application.
pub const CALLBACK_PATH: &str = "/oauth/feishu/callback";

/// Redirect URIs submitted during dynamic client registration.
#[derive(Clone, Debug, Deserialize)]
pub struct Registration {
    pub redirect_uris: Vec<String>,
}

/// Parameters supplied to the OAuth authorization endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub response_type: String,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub resource: String,
    pub scope: Option<String>,
}

/// Query parameters returned to the server by Lark after login.
#[derive(Clone, Debug, Deserialize)]
pub struct LarkCallback {
    pub code: Option<String>,
    pub state: String,
}

/// Form fields used for code exchange or refresh-token rotation.
#[derive(Clone, Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    pub resource: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
}

/// OAuth response fields and optional internal detail for logging.
#[derive(Debug)]
pub struct AuthError {
    pub status: u16,
    pub code: &'static str,
    pub description: &'static str,
    pub detail: Option<String>,
}

impl AuthError {
    /// Report a malformed OAuth request without exposing internal details.
    fn request(description: &'static str) -> Self {
        Self {
            status: 400,
            code: "invalid_request",
            description,
            detail: None,
        }
    }

    /// Report a missing, expired, or mismatched one-time grant.
    fn grant(description: &'static str) -> Self {
        Self {
            status: 400,
            code: "invalid_grant",
            description,
            detail: None,
        }
    }

    /// Preserve a database error for server-side logging.
    fn storage(error: impl std::fmt::Display) -> Self {
        Self {
            status: 500,
            code: "server_error",
            description: "OAuth storage failed",
            detail: Some(error.to_string()),
        }
    }

    /// Preserve an invalid server URL for server-side logging.
    fn configuration(error: impl std::fmt::Display) -> Self {
        Self {
            status: 500,
            code: "server_error",
            description: "invalid OAuth configuration",
            detail: Some(error.to_string()),
        }
    }
}

/// Return the MCP resource identifier for a public server origin.
pub fn resource(origin: &str) -> String {
    format!("{}/mcp", origin.trim_end_matches('/'))
}

/// Build the authorization-server discovery document.
pub fn authorization_metadata(origin: &str) -> Value {
    let origin = origin.trim_end_matches('/');
    json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/authorize"),
        "token_endpoint": format!("{origin}/token"),
        "registration_endpoint": format!("{origin}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE]
    })
}

/// Build the protected-resource discovery document for the MCP endpoint.
pub fn resource_metadata(origin: &str) -> Value {
    json!({
        "resource": resource(origin),
        "authorization_servers": [origin.trim_end_matches('/')],
        "scopes_supported": [SCOPE],
        "bearer_methods_supported": ["header"]
    })
}

/// Build the `WWW-Authenticate` value that points clients to discovery.
pub fn bearer_challenge(origin: &str) -> String {
    format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"{SCOPE}\"",
        origin.trim_end_matches('/')
    )
}

/// Return the Lark login callback URL for a public server origin.
pub fn callback_url(origin: &str) -> String {
    format!("{}{CALLBACK_PATH}", origin.trim_end_matches('/'))
}

/// Build the Lark authorization-code exchange body, including app credentials.
pub fn lark_token_request(lark: &LarkConfig, origin: &str, code: &str) -> Value {
    json!({
        "grant_type": "authorization_code",
        "client_id": lark.app_id,
        "client_secret": lark.app_secret,
        "code": code,
        "redirect_uri": callback_url(origin)
    })
}

/// Extract a Lark user token from a successful token-exchange response.
pub fn lark_access_token(response: &Value) -> Result<&str, AuthError> {
    if response["code"].as_i64().is_some_and(|code| code != 0) {
        return Err(AuthError {
            status: 502,
            code: "server_error",
            description: "Lark token exchange failed",
            detail: None,
        });
    }
    response["access_token"]
        .as_str()
        .or_else(|| response["data"]["access_token"].as_str())
        .ok_or(AuthError {
            status: 502,
            code: "server_error",
            description: "Lark did not return an access token",
            detail: None,
        })
}

/// Extract the user's Open ID from a successful Lark profile response.
pub fn lark_open_id(response: &Value) -> Result<&str, AuthError> {
    if response["code"].as_i64() != Some(0) {
        return Err(AuthError {
            status: 502,
            code: "server_error",
            description: "Lark user lookup failed",
            detail: None,
        });
    }
    response["data"]["open_id"].as_str().ok_or(AuthError {
        status: 502,
        code: "server_error",
        description: "Lark did not return an open_id",
        detail: None,
    })
}

/// Build the OAuth token response returned after exchange or rotation.
pub fn token_response(access_token: String, refresh_token: String) -> Value {
    json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL,
        "scope": SCOPE
    })
}

/// OAuth workflow independent of HTTP transport and database implementation.
pub struct AuthFlow<'a, D: DbApi + ?Sized> {
    dbapi: &'a D,
    origin: &'a str,
}

impl<'a, D: DbApi + ?Sized> AuthFlow<'a, D>
where
    D::Error: std::fmt::Display,
{
    /// Bind a database adapter and the public OAuth server origin.
    pub fn new(dbapi: &'a D, origin: &'a str) -> Self {
        Self { dbapi, origin }
    }

    /// Validate redirect URIs and persist a generated client ID.
    pub async fn register(
        &self,
        request: Registration,
        client_id: String,
    ) -> Result<Client, AuthError> {
        if request.redirect_uris.is_empty()
            || request.redirect_uris.len() > 10
            || !request
                .redirect_uris
                .iter()
                .all(|uri| valid_redirect_uri(uri))
        {
            return Err(AuthError::request(
                "one to ten HTTPS or loopback redirect_uris are required",
            ));
        }
        let client = Client {
            client_id,
            redirect_uris: request.redirect_uris,
        };
        self.dbapi
            .insert_client(client.clone())
            .await
            .map_err(AuthError::storage)?;
        Ok(client)
    }

    /// Validate an authorization request, save its state, and return the Lark login URL.
    /// `login_state` is generated by the HTTP adapter; `now` is Unix time in seconds.
    pub async fn begin_authorize(
        &self,
        request: AuthorizeRequest,
        lark: &LarkConfig,
        login_state: String,
        now: i64,
    ) -> Result<Url, AuthError> {
        if request.response_type != "code"
            || request.code_challenge_method != "S256"
            || request.state.is_empty()
            || request.code_challenge.len() < 40
            || request.resource != resource(self.origin)
            || request.scope.as_deref().is_some_and(|scope| scope != SCOPE)
        {
            return Err(AuthError::request("invalid authorization request"));
        }
        let client = self
            .dbapi
            .get_client(&request.client_id)
            .await
            .map_err(AuthError::storage)?
            .ok_or(AuthError::request("unregistered client"))?;
        if !client.redirect_uris.contains(&request.redirect_uri) {
            return Err(AuthError::request("unregistered redirect_uri"));
        }
        let mut destination =
            Url::parse(&lark.lark_authorize_url).map_err(AuthError::configuration)?;
        destination
            .query_pairs_mut()
            .append_pair("client_id", &lark.app_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &callback_url(self.origin))
            .append_pair("state", &login_state);
        if !lark.lark_scope.is_empty() {
            destination
                .query_pairs_mut()
                .append_pair("scope", &lark.lark_scope);
        }
        self.dbapi
            .insert_pending(
                &login_state,
                PendingLogin {
                    client_id: request.client_id,
                    redirect_uri: request.redirect_uri,
                    client_state: request.state,
                    challenge: request.code_challenge,
                },
                now + LOGIN_TTL,
            )
            .await
            .map_err(AuthError::storage)?;
        Ok(destination)
    }

    /// Consume the state saved before Lark login; reject expired or reused states.
    pub async fn take_pending(&self, state: &str) -> Result<PendingLogin, AuthError> {
        self.dbapi
            .delete_pending(state)
            .await
            .map_err(AuthError::storage)?
            .ok_or(AuthError::grant("invalid or expired login state"))
    }

    /// Issue a one-time code after Lark confirms the user and build the client redirect.
    pub async fn finish_login(
        &self,
        pending: PendingLogin,
        open_id: &str,
        authorization_code: String,
        now: i64,
    ) -> Result<Url, AuthError> {
        let mut destination =
            Url::parse(&pending.redirect_uri).map_err(AuthError::configuration)?;
        destination
            .query_pairs_mut()
            .append_pair("code", &authorization_code)
            .append_pair("state", &pending.client_state);
        self.dbapi
            .insert_code(
                &authorization_code,
                AuthorizationCode {
                    client_id: pending.client_id,
                    redirect_uri: pending.redirect_uri,
                    challenge: pending.challenge,
                    open_id: open_id.to_owned(),
                },
                now + LOGIN_TTL,
            )
            .await
            .map_err(AuthError::storage)?;
        Ok(destination)
    }

    /// Verify PKCE and consume a code before persisting a new token pair.
    /// The adapter supplies fresh token strings and Unix time in seconds.
    pub async fn exchange_code(
        &self,
        request: TokenRequest,
        access: String,
        refresh: String,
        now: i64,
    ) -> Result<Value, AuthError> {
        if request.resource.as_deref() != Some(resource(self.origin).as_str()) {
            return Err(AuthError::request("invalid resource"));
        }
        let verifier = request
            .code_verifier
            .as_deref()
            .ok_or(AuthError::request("missing PKCE verifier"))?;
        if !valid_verifier(verifier) {
            return Err(AuthError::request("invalid PKCE verifier"));
        }
        let raw_code = request
            .code
            .as_deref()
            .ok_or(AuthError::request("missing authorization code"))?;
        let code = self
            .dbapi
            .get_code(raw_code)
            .await
            .map_err(AuthError::storage)?
            .ok_or(AuthError::grant("invalid or expired code"))?;
        if code.client_id != request.client_id
            || Some(code.redirect_uri.as_str()) != request.redirect_uri.as_deref()
            || pkce_challenge(verifier)
                .as_bytes()
                .ct_eq(code.challenge.as_bytes())
                .unwrap_u8()
                != 1
        {
            return Err(AuthError::grant(
                "invalid client, redirect_uri, or PKCE verifier",
            ));
        }
        let consumed = self
            .dbapi
            .delete_code(raw_code)
            .await
            .map_err(AuthError::storage)?
            .ok_or(AuthError::grant("invalid or expired code"))?;
        let grant = TokenGrant {
            client_id: consumed.client_id,
            open_id: consumed.open_id,
            resource: resource(self.origin),
        };
        self.dbapi
            .insert_token_pair(
                &access,
                &refresh,
                grant,
                now + ACCESS_TTL,
                now + REFRESH_TTL,
            )
            .await
            .map_err(AuthError::storage)?;
        Ok(token_response(access, refresh))
    }

    /// Validate a client's refresh grant and rotate it to a new token pair.
    pub async fn refresh_token(
        &self,
        request: TokenRequest,
        access: String,
        refresh: String,
        now: i64,
    ) -> Result<Value, AuthError> {
        let expected_resource = resource(self.origin);
        if request
            .resource
            .as_deref()
            .is_some_and(|value| value != expected_resource)
        {
            return Err(AuthError::request("invalid resource"));
        }
        let previous = request
            .refresh_token
            .as_deref()
            .ok_or(AuthError::request("missing refresh token"))?;
        let grant = self
            .dbapi
            .get_refresh_token(previous)
            .await
            .map_err(AuthError::storage)?
            .ok_or(AuthError::grant("invalid or expired refresh token"))?;
        let client = self
            .dbapi
            .get_client(&request.client_id)
            .await
            .map_err(AuthError::storage)?;
        if grant.client_id != request.client_id
            || grant.resource != expected_resource
            || client.is_none()
        {
            return Err(AuthError::grant(
                "refresh token is not valid for this client or user",
            ));
        }
        if !self
            .dbapi
            .replace_refresh_token(
                previous,
                &access,
                &refresh,
                grant,
                now + ACCESS_TTL,
                now + REFRESH_TTL,
            )
            .await
            .map_err(AuthError::storage)?
        {
            return Err(AuthError::grant("invalid or expired refresh token"));
        }
        Ok(token_response(access, refresh))
    }

    /// Check a Bearer token against a live grant for this MCP resource.
    pub async fn authenticate(&self, authorization: Option<&str>) -> Result<bool, AuthError> {
        let Some(token) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
            return Ok(false);
        };
        let grant = self
            .dbapi
            .get_access_token(token)
            .await
            .map_err(AuthError::storage)?;
        Ok(grant.is_some_and(|grant| {
            grant.resource == resource(self.origin) && !grant.open_id.is_empty()
        }))
    }
}

/// Compute the base64url-encoded S256 challenge for a PKCE verifier.
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Accept only verifier lengths and characters allowed by PKCE.
fn valid_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
}

/// Accept HTTPS redirects or loopback HTTP redirects with an explicit port.
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
    use super::{pkce_challenge, valid_redirect_uri};

    #[test]
    fn validates_pkce_and_redirects() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(valid_redirect_uri("http://127.0.0.1:4567/callback"));
        assert!(valid_redirect_uri(
            "https://chatgpt.com/connector/oauth/callback"
        ));
        assert!(!valid_redirect_uri("http://example.com/callback"));
        assert!(!valid_redirect_uri("https://example.com/callback#fragment"));
    }
}
