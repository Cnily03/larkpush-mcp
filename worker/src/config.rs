//! Worker bindings and deployment configuration.

use larkpush_config::LarkConfig;
use url::Url;
use worker::{Env, Error, Result};

const DEFAULT_LARK_BASE_URL: &str = "https://open.feishu.cn/open-apis";
const DEFAULT_LARK_AUTHORIZE_URL: &str = "https://accounts.feishu.cn/open-apis/authen/v1/authorize";
const DEFAULT_LARK_TOKEN_URL: &str = "https://accounts.feishu.cn/oauth/v3/token";
const DEFAULT_LARK_USER_INFO_URL: &str = "https://open.feishu.cn/open-apis/authen/v1/user_info";
const DEFAULT_LARK_SCOPE: &str = "contact:user.base:readonly";

pub struct Config {
    pub public_base_url: String,
    pub lark: LarkConfig,
    pub redis_url: String,
}

impl Config {
    /// Read bindings and validate the public HTTPS or loopback origin.
    pub fn from_env(env: &Env) -> Result<Self> {
        let public_base_url = required_var(env, "PUBLIC_BASE_URL")?
            .trim_end_matches('/')
            .to_owned();
        let url = Url::parse(&public_base_url)
            .map_err(|_| Error::RustError("invalid PUBLIC_BASE_URL".into()))?;
        let secure = url.scheme() == "https";
        let local = url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !(secure || local)
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::RustError(
                "PUBLIC_BASE_URL must be an HTTPS or loopback origin".into(),
            ));
        }
        Ok(Self {
            public_base_url,
            redis_url: required_secret(env, "REDIS_URL")?,
            lark: LarkConfig {
                app_id: required_var(env, "LARK_APP_ID")?,
                app_secret: required_secret(env, "LARK_APP_SECRET")?,
                base_url: optional(env, "LARK_BASE_URL")
                    .unwrap_or_else(|| DEFAULT_LARK_BASE_URL.to_owned()),
                lark_authorize_url: optional(env, "LARK_AUTHORIZE_URL")
                    .unwrap_or_else(|| DEFAULT_LARK_AUTHORIZE_URL.to_owned()),
                lark_token_url: optional(env, "LARK_TOKEN_URL")
                    .unwrap_or_else(|| DEFAULT_LARK_TOKEN_URL.to_owned()),
                lark_user_info_url: optional(env, "LARK_USER_INFO_URL")
                    .unwrap_or_else(|| DEFAULT_LARK_USER_INFO_URL.to_owned()),
                lark_scope: optional(env, "LARK_SCOPE")
                    .unwrap_or_else(|| DEFAULT_LARK_SCOPE.to_owned()),
            },
        })
    }
}

/// Read a nonempty secret binding such as the Lark app secret.
fn required_secret(env: &Env, name: &str) -> Result<String> {
    let value = env.secret(name)?.to_string();
    if value.trim().is_empty() {
        return Err(Error::RustError(format!("{name} must not be empty")));
    }
    Ok(value)
}

/// Read a nonempty plain-text Worker variable.
fn required_var(env: &Env, name: &str) -> Result<String> {
    let value = env.var(name)?.to_string();
    if value.trim().is_empty() {
        return Err(Error::RustError(format!("{name} must not be empty")));
    }
    Ok(value)
}

/// Read an optional plain-text Worker variable.
fn optional(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|value| value.to_string())
}
