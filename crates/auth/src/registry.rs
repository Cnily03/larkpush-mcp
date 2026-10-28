use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const REGISTRY_VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct StoredClients {
    version: u8,
    issuer: String,
    clients: HashMap<String, Vec<String>>,
}

pub(crate) struct ClientRegistry {
    path: Option<PathBuf>,
    stored: StoredClients,
}

impl ClientRegistry {
    pub(crate) fn load(path: PathBuf, issuer: String) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create OAuth client data directory {}",
                    parent.display()
                )
            })?;
        }
        let stored = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoredClients>(&bytes)
                .with_context(|| format!("invalid OAuth client registry {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoredClients {
                version: REGISTRY_VERSION,
                issuer: issuer.clone(),
                clients: HashMap::new(),
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read OAuth client registry {}", path.display())
                });
            }
        };
        if stored.version != REGISTRY_VERSION || stored.issuer != issuer {
            bail!(
                "OAuth client registry {} has a different version or issuer",
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
            stored: StoredClients {
                version: REGISTRY_VERSION,
                issuer,
                clients: HashMap::new(),
            },
        }
    }

    pub(crate) fn redirect_uris(&self, client_id: &str) -> Option<&[String]> {
        self.stored.clients.get(client_id).map(Vec::as_slice)
    }

    pub(crate) fn len(&self) -> usize {
        self.stored.clients.len()
    }

    pub(crate) async fn register(
        &mut self,
        client_id: String,
        redirect_uris: Vec<String>,
    ) -> Result<()> {
        self.stored.clients.insert(client_id.clone(), redirect_uris);
        if let Err(error) = self.persist().await {
            self.stored.clients.remove(&client_id);
            return Err(error);
        }
        Ok(())
    }

    async fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&self.stored)?;
        let name = path
            .file_name()
            .context("OAuth client registry path must name a file")?
            .to_string_lossy();
        // Replace the file only after the complete new registry has been written.
        let temporary = path.with_file_name(format!(".{name}.{}.tmp", super::random_secret()));
        tokio::fs::write(&temporary, bytes).await.with_context(|| {
            format!(
                "failed to write OAuth client registry {}",
                temporary.display()
            )
        })?;
        if let Err(error) = tokio::fs::rename(&temporary, path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| {
                format!("failed to replace OAuth client registry {}", path.display())
            });
        }
        Ok(())
    }
}
