use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const REGISTRY_VERSION: u8 = 1;
const REFRESH_TTL: Duration = Duration::from_secs(90 * 24 * 60 * 60);

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RefreshGrant {
    pub(crate) client_id: String,
    pub(crate) open_id: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct AccessGrant {
    open_id: String,
    expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredRefreshTokens {
    version: u8,
    issuer: String,
    // Keys are SHA-256 hashes; raw refresh tokens are never written to disk.
    grants: HashMap<String, RefreshGrant>,
    #[serde(default)]
    access: HashMap<String, AccessGrant>,
}

pub(crate) struct RefreshRegistry {
    path: Option<PathBuf>,
    stored: StoredRefreshTokens,
}

impl RefreshRegistry {
    pub(crate) fn load(path: PathBuf, issuer: String) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create OAuth token directory {}",
                    parent.display()
                )
            })?;
        }
        let stored = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoredRefreshTokens>(&bytes)
                .with_context(|| format!("invalid OAuth token registry {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoredRefreshTokens {
                version: REGISTRY_VERSION,
                issuer: issuer.clone(),
                grants: HashMap::new(),
                access: HashMap::new(),
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read OAuth token registry {}", path.display())
                });
            }
        };
        if stored.version != REGISTRY_VERSION || stored.issuer != issuer {
            bail!(
                "OAuth token registry {} has a different version or issuer",
                path.display()
            );
        }
        Ok(Self {
            path: Some(path),
            stored,
        })
    }

    #[cfg(test)]
    pub(crate) fn in_memory(issuer: String) -> Self {
        Self {
            path: None,
            stored: StoredRefreshTokens {
                version: REGISTRY_VERSION,
                issuer,
                grants: HashMap::new(),
                access: HashMap::new(),
            },
        }
    }

    pub(crate) fn get(&self, token: &str) -> Option<&RefreshGrant> {
        self.stored
            .grants
            .get(&token_hash(token))
            .filter(|grant| grant.expires_at > now_seconds())
    }

    pub(crate) fn access_open_id(&self, token: &str) -> Option<&str> {
        self.stored
            .access
            .get(&token_hash(token))
            .filter(|grant| grant.expires_at > now_seconds())
            .map(|grant| grant.open_id.as_str())
    }

    pub(crate) async fn issue(
        &mut self,
        client_id: String,
        open_id: String,
    ) -> Result<(String, String)> {
        let previous = self.stored.clone();
        self.prune_expired();
        let refresh = super::random_secret();
        let access = super::random_secret();
        self.stored.grants.insert(
            token_hash(&refresh),
            RefreshGrant {
                client_id,
                open_id: open_id.clone(),
                expires_at: now_seconds() + REFRESH_TTL.as_secs(),
            },
        );
        self.insert_access(&access, open_id);
        if let Err(error) = self.persist().await {
            self.stored = previous;
            return Err(error);
        }
        Ok((access, refresh))
    }

    pub(crate) async fn rotate(&mut self, old: &str) -> Result<Option<(String, String)>> {
        let old_key = token_hash(old);
        let Some(mut grant) = self.get(old).cloned() else {
            return Ok(None);
        };
        let previous = self.stored.clone();
        self.prune_expired();
        let refresh = super::random_secret();
        let access = super::random_secret();
        self.stored.grants.remove(&old_key);
        grant.expires_at = now_seconds() + REFRESH_TTL.as_secs();
        self.stored
            .grants
            .insert(token_hash(&refresh), grant.clone());
        self.insert_access(&access, grant.open_id);
        if let Err(error) = self.persist().await {
            self.stored = previous;
            return Err(error);
        }
        Ok(Some((access, refresh)))
    }

    fn insert_access(&mut self, token: &str, open_id: String) {
        self.stored.access.insert(
            token_hash(token),
            AccessGrant {
                open_id,
                expires_at: now_seconds() + super::TOKEN_TTL.as_secs(),
            },
        );
    }

    fn prune_expired(&mut self) {
        let now = now_seconds();
        self.stored.grants.retain(|_, grant| grant.expires_at > now);
        self.stored.access.retain(|_, grant| grant.expires_at > now);
    }

    async fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&self.stored)?;
        let name = path
            .file_name()
            .context("OAuth token registry path must name a file")?;
        let temporary = path.with_file_name(format!(
            ".{}.{}.tmp",
            name.to_string_lossy(),
            super::random_secret()
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await.with_context(|| {
            format!(
                "failed to create OAuth token registry {}",
                temporary.display()
            )
        })?;
        if let Err(error) = file.write_all(&bytes).await {
            drop(file);
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| {
                format!(
                    "failed to write OAuth token registry {}",
                    temporary.display()
                )
            });
        }
        if let Err(error) = file.sync_all().await {
            drop(file);
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| {
                format!(
                    "failed to sync OAuth token registry {}",
                    temporary.display()
                )
            });
        }
        drop(file);
        if let Err(error) = tokio::fs::rename(&temporary, path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| {
                format!("failed to replace OAuth token registry {}", path.display())
            });
        }
        Ok(())
    }
}

fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
