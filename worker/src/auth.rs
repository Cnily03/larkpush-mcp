//! Cloudflare HTTP adapter for the shared OAuth workflow.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use larkpush_auth::{
    AuthError, AuthFlow, AuthorizeRequest, LarkCallback, Registration, TokenRequest,
    authorization_metadata, bearer_challenge, lark_access_token, lark_open_id, lark_token_request,
    resource_metadata,
};
use larkpush_dbapi::DbApi;
use serde_json::{Value, json};
use url::Url;
use wasm_bindgen::{JsCast, JsValue};
use worker::{Error, Fetch, Method, Request, RequestInit, Response, Result};

use crate::config::Config;

/// Serve authorization-server discovery using the shared OAuth metadata.
pub fn metadata(config: &Config) -> Result<Response> {
    Response::from_json(&authorization_metadata(&config.public_base_url))
}

/// Serve protected-resource discovery for the MCP endpoint.
pub fn protected_resource_metadata(config: &Config) -> Result<Response> {
    Response::from_json(&resource_metadata(&config.public_base_url))
}

/// Parse a registration request and persist a validated client.
pub async fn register(
    req: &mut Request,
    config: &Config,
    dbapi: &impl DbApi<Error = Error>,
) -> Result<Response> {
    let request: Registration = match req.json().await {
        Ok(value) => value,
        Err(_) => return oauth_error(400, "invalid_request", "invalid client registration"),
    };
    let flow = AuthFlow::new(dbapi, &config.public_base_url);
    match flow.register(request, random_secret()?).await {
        Ok(client) => {
            log::info!("OAuth client registered client_id={}", client.client_id);
            Ok(Response::from_json(&json!({
                "client_id": client.client_id,
                "redirect_uris": client.redirect_uris,
                "token_endpoint_auth_method": "none"
            }))?
            .with_status(201))
        }
        Err(error) => auth_error(error),
    }
}

/// Save a login state and redirect the browser to Lark.
pub async fn authorize(
    req: &Request,
    config: &Config,
    dbapi: &impl DbApi<Error = Error>,
) -> Result<Response> {
    let request: AuthorizeRequest = match parse_query(req) {
        Ok(request) => request,
        Err(_) => return oauth_error(400, "invalid_request", "invalid authorization request"),
    };
    let client_id = request.client_id.clone();
    let redirect_origin = redirect_origin(&request.redirect_uri);
    let flow = AuthFlow::new(dbapi, &config.public_base_url);
    match flow
        .begin_authorize(request, &config.lark, random_secret()?, now_seconds())
        .await
    {
        Ok(destination) => {
            log::info!(
                "Lark login initiated client_id={client_id} redirect_origin={redirect_origin}"
            );
            Response::redirect(destination)
        }
        Err(error) => auth_error(error),
    }
}

/// Complete Lark login and redirect the browser with an OAuth code.
pub async fn callback(
    req: &Request,
    config: &Config,
    dbapi: &impl DbApi<Error = Error>,
) -> Result<Response> {
    let query: LarkCallback = match parse_query(req) {
        Ok(query) => query,
        Err(_) => return oauth_error(400, "invalid_request", "missing login state"),
    };
    let flow = AuthFlow::new(dbapi, &config.public_base_url);
    let pending = match flow.take_pending(&query.state).await {
        Ok(pending) => pending,
        Err(error) => return auth_error(error),
    };
    let Some(code) = query.code else {
        return oauth_error(400, "access_denied", "Lark login was denied");
    };
    let token_data = fetch_json(
        &config.lark.lark_token_url,
        Method::Post,
        None,
        Some(lark_token_request(
            &config.lark,
            &config.public_base_url,
            &code,
        )),
    )
    .await?;
    let user_token = match lark_access_token(&token_data) {
        Ok(token) => token,
        Err(error) => return auth_error(error),
    };
    let user = fetch_json(
        &config.lark.lark_user_info_url,
        Method::Get,
        Some(user_token),
        None,
    )
    .await?;
    let open_id = match lark_open_id(&user) {
        Ok(open_id) => open_id,
        Err(error) => return auth_error(error),
    };
    let client_id = pending.client_id.clone();
    let redirect_origin = redirect_origin(&pending.redirect_uri);
    match flow
        .finish_login(pending, open_id, random_secret()?, now_seconds())
        .await
    {
        Ok(destination) => {
            log::info!(
                "Lark login completed client_id={client_id} redirect_origin={redirect_origin} open_id={open_id}"
            );
            Response::redirect(destination)
        }
        Err(error) => auth_error(error),
    }
}

/// Exchange or refresh a grant and prevent token-response caching.
pub async fn token(
    req: &mut Request,
    config: &Config,
    dbapi: &impl DbApi<Error = Error>,
) -> Result<Response> {
    let text = req.text().await?;
    let request: TokenRequest = match serde_urlencoded::from_str(&text) {
        Ok(request) => request,
        Err(_) => return oauth_error(400, "invalid_request", "invalid token request"),
    };
    let grant_type = request.grant_type.clone();
    let client_id = request.client_id.clone();
    let flow = AuthFlow::new(dbapi, &config.public_base_url);
    let access = random_secret()?;
    let refresh = random_secret()?;
    let now = now_seconds();
    let result = match request.grant_type.as_str() {
        "authorization_code" => flow.exchange_code(request, access, refresh, now).await,
        "refresh_token" => flow.refresh_token(request, access, refresh, now).await,
        _ => return oauth_error(400, "unsupported_grant_type", "unsupported grant type"),
    };
    match result {
        Ok(value) => {
            log::info!("OAuth token issued grant_type={grant_type} client_id={client_id}");
            let mut response = Response::from_json(&value)?;
            response.headers_mut().set("cache-control", "no-store")?;
            Ok(response)
        }
        Err(error) => auth_error(error),
    }
}

/// Validate the request Bearer token against the MCP resource.
pub async fn authenticate(
    req: &Request,
    config: &Config,
    dbapi: &impl DbApi<Error = Error>,
) -> Result<bool> {
    let authorization = req.headers().get("authorization")?;
    AuthFlow::new(dbapi, &config.public_base_url)
        .authenticate(authorization.as_deref())
        .await
        .map_err(|error| Error::RustError(error.detail.unwrap_or(error.description.to_owned())))
}

/// Return an OAuth challenge with a discovery URL in WWW-Authenticate.
pub fn challenge(config: &Config) -> Result<Response> {
    let mut response = oauth_error(401, "invalid_token", "Authorization required")?;
    response.headers_mut().set(
        "www-authenticate",
        &bearer_challenge(&config.public_base_url),
    )?;
    Ok(response)
}

/// Deserialize URL query parameters into a typed OAuth request.
fn parse_query<T: for<'de> serde::Deserialize<'de>>(req: &Request) -> Result<T> {
    let url = req.url()?;
    serde_urlencoded::from_str(url.query().unwrap_or_default())
        .map_err(|error| Error::RustError(error.to_string()))
}

/// Keep callback paths and query parameters out of login logs.
fn redirect_origin(redirect_uri: &str) -> String {
    Url::parse(redirect_uri)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default()
}

/// Read Unix time from the Worker runtime for grant expiration.
fn now_seconds() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// Convert a transport-independent OAuth error into a Worker response.
fn auth_error(error: AuthError) -> Result<Response> {
    log::warn!(
        "OAuth request rejected status={} code={} reason={}",
        error.status,
        error.code,
        error.description
    );
    oauth_error(error.status, error.code, error.description)
}

/// Serialize a Worker OAuth error with the requested HTTP status.
fn oauth_error(status: u16, code: &str, description: &str) -> Result<Response> {
    Ok(
        Response::from_json(&json!({"error": code, "error_description": description}))?
            .with_status(status),
    )
}

/// Generate a URL-safe OAuth credential with Web Crypto.
fn random_secret() -> Result<String> {
    let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
        .map_err(|_| Error::RustError("Web Crypto is unavailable".into()))?
        .dyn_into::<web_sys::Crypto>()
        .map_err(|_| Error::RustError("invalid Web Crypto binding".into()))?;
    let mut bytes = [0u8; 32];
    crypto
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| Error::RustError("secure random generation failed".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Call Lark with optional JSON and Bearer authentication; reject HTTP errors.
async fn fetch_json(
    url: &str,
    method: Method,
    bearer: Option<&str>,
    body: Option<Value>,
) -> Result<Value> {
    let mut init = RequestInit::new();
    init.with_method(method);
    if let Some(body) = body {
        init.with_body(Some(JsValue::from_str(&body.to_string())));
    }
    let mut request = Request::new_with_init(url, &init)?;
    if let Some(token) = bearer {
        request
            .headers_mut()?
            .set("authorization", &format!("Bearer {token}"))?;
    }
    if init.body.is_some() {
        request
            .headers_mut()?
            .set("content-type", "application/json")?;
    }
    let mut response = Fetch::Request(request).send().await?;
    if !(200..300).contains(&response.status_code()) {
        return Err(Error::RustError(format!(
            "Lark HTTP error {}",
            response.status_code()
        )));
    }
    response.json().await
}
