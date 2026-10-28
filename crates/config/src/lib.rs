use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub lark: LarkConfig,
    pub server: ServerConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LarkConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_authorize_url")]
    pub lark_authorize_url: String,
    #[serde(default = "default_token_url")]
    pub lark_token_url: String,
    #[serde(default = "default_user_info_url")]
    pub lark_user_info_url: String,
    #[serde(default = "default_scope")]
    pub lark_scope: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    pub public_base_url: String,
    #[serde(default)]
    pub client_registry_path: Option<PathBuf>,
}

fn default_base_url() -> String {
    "https://open.feishu.cn/open-apis".into()
}

fn default_listen() -> String {
    "127.0.0.1:3000".into()
}
fn default_authorize_url() -> String {
    "https://accounts.feishu.cn/open-apis/authen/v1/authorize".into()
}
fn default_token_url() -> String {
    "https://accounts.feishu.cn/oauth/v3/token".into()
}
fn default_scope() -> String {
    "contact:user.base:readonly".into()
}
fn default_user_info_url() -> String {
    "https://open.feishu.cn/open-apis/authen/v1/user_info".into()
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        if config.lark.app_id.trim().is_empty() || config.lark.app_secret.trim().is_empty() {
            bail!(
                "lark.app_id and lark.app_secret are required in {}",
                path.display()
            );
        }
        let public_url = url::Url::parse(&config.server.public_base_url)
            .context("invalid server.public_base_url")?;
        if !matches!(public_url.scheme(), "http" | "https")
            || public_url.host_str().is_none()
            || public_url.path() != "/"
            || public_url.query().is_some()
            || public_url.fragment().is_some()
        {
            bail!("server.public_base_url must be an HTTP(S) origin without a query or fragment");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn example_uses_lark_and_server_sections() {
        let config: Config = toml::from_str(include_str!("../../../config.example.toml"))
            .expect("example configuration should parse");
        assert_eq!(config.lark.base_url, "https://open.feishu.cn/open-apis");
        assert_eq!(config.server.listen, "127.0.0.1:3000");
    }

    #[test]
    fn oauth_overrides_use_lark_field_names() {
        let current = r#"
            [lark]
            app_id = "app"
            app_secret = "secret"
            lark_scope = "custom:scope"

            [server]
            public_base_url = "http://127.0.0.1:3000"
        "#;
        let config: Config = toml::from_str(current).expect("Lark override should parse");
        assert_eq!(config.lark.lark_scope, "custom:scope");

        let old = current.replace("lark_scope", "unknown_scope");
        assert!(toml::from_str::<Config>(&old).is_err());
    }

    #[test]
    fn old_single_section_layout_is_rejected() {
        let old = r#"
            [server]
            app_id = "app"
            app_secret = "secret"
            public_base_url = "http://127.0.0.1:3000"
        "#;
        assert!(toml::from_str::<Config>(old).is_err());
    }
}
