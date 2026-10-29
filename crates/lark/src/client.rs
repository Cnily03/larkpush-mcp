use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use larkpush_config::LarkConfig;
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct LarkClient {
    http: Client,
    config: LarkConfig,
    tenant_token: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    code: i64,
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<u64>,
}

impl LarkClient {
    /// Build an HTTP client and initialize a shared tenant-token cache.
    pub fn new(config: LarkConfig) -> Result<Self> {
        let builder = Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.timeout(Duration::from_secs(30));
        let http = builder.build()?;
        Ok(Self {
            http,
            config,
            tenant_token: Arc::new(Mutex::new(None)),
        })
    }

    /// Join an API path to the configured Lark Open API base URL.
    pub(crate) fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Reuse a cached tenant token or refresh it before its expiry margin.
    pub(crate) async fn token(&self) -> Result<String> {
        let mut cached = self.tenant_token.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.expires_at > Instant::now())
        {
            return Ok(token.value.clone());
        }
        let response: TokenResponse = self
            .http
            .post(self.url("auth/v3/tenant_access_token/internal"))
            .json(&json!({"app_id": self.config.app_id, "app_secret": self.config.app_secret}))
            .send()
            .await
            .context("tenant token request failed")?
            .error_for_status()
            .context("tenant token HTTP error")?
            .json()
            .await
            .context("invalid tenant token response")?;
        if response.code != 0 {
            bail!("Lark token error {}: {}", response.code, response.msg);
        }
        let value = response
            .tenant_access_token
            .context("missing tenant_access_token")?;
        let ttl = response.expire.unwrap_or(7200).saturating_sub(60).max(1);
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(value)
    }

    /// Send an authenticated API request and return its successful `data` payload.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let token = self.token().await?;
        let mut request = self
            .http
            .request(method, self.url(path))
            .bearer_auth(token)
            .query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("Lark request failed")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("invalid Lark JSON response")?;
        if !status.is_success() || value["code"].as_i64() != Some(0) {
            bail!(
                "Lark API error (HTTP {}, code {}): {}",
                status,
                value["code"],
                value["msg"].as_str().unwrap_or("unknown error")
            );
        }
        Ok(value["data"].clone())
    }

    /// Upload multipart content and return the successful Lark `data` payload.
    pub(crate) async fn upload(&self, path: &str, form: reqwest::multipart::Form) -> Result<Value> {
        let response = self
            .http
            .post(self.url(path))
            .bearer_auth(self.token().await?)
            .multipart(form)
            .send()
            .await
            .context("Lark upload failed")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("invalid Lark upload response")?;
        if !status.is_success() || value["code"].as_i64() != Some(0) {
            bail!(
                "Lark upload error (HTTP {}, code {}): {}",
                status,
                value["code"],
                value["msg"].as_str().unwrap_or("unknown error")
            );
        }
        Ok(value["data"].clone())
    }
}
