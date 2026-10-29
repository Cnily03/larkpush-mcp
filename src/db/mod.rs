//! Native database adapter using SQLite for durable records and memory for cache entries.

mod memory_cache;

use std::path::Path;

use anyhow::{Context, Result, bail};
use larkpush_dbapi::{AuthorizationCode, Client, DbApi, PendingLogin, TokenGrant};
use sea_query::{Alias, Expr, ExprTrait, Query, SqliteQueryBuilder, Value, Values};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

pub use memory_cache::MemoryCache;

const TABLE_CLIENTS: &str = "oauth_clients";
const TABLE_TOKENS: &str = "oauth_tokens";
const PENDING_PREFIX: &str = "larkpush:pending:";
const CODE_PREFIX: &str = "larkpush:code:";

type Statement = (String, Values);

/// Return Unix time in seconds for expiry checks and cleanup.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// SeaQuery supplies placeholders; this adapter binds their values to sqlx.
fn bind<'q>(
    statement: &'q Statement,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>> {
    let mut query = sqlx::query(&statement.0);
    for value in &statement.1.0 {
        query = match value {
            Value::String(value) => query.bind(value.clone()),
            Value::BigInt(value) => query.bind(*value),
            Value::Int(value) => query.bind(*value),
            _ => bail!("unsupported SQLite query parameter: {value:?}"),
        };
    }
    Ok(query)
}

#[derive(Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
    cache: MemoryCache,
}

impl SqliteStore {
    /// Open a private SQLite file and initialize its schema if needed.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create SQLite directory {}", parent.display())
            })?;
        }
        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to create {}", path.display()));
                }
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open SQLite database {}", path.display()))?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&pool)
            .await
            .context("failed to initialize SQLite database")?;
        Ok(Self {
            pool,
            cache: MemoryCache::new(),
        })
    }

    /// Read a live grant by either token in the shared token table.
    async fn get_grant(
        &self,
        token_column: &str,
        expiry_column: &str,
        token: &str,
    ) -> Result<Option<TokenGrant>> {
        let statement = Query::select()
            .columns([
                Alias::new("client_id"),
                Alias::new("open_id"),
                Alias::new("resource"),
            ])
            .from(Alias::new(TABLE_TOKENS))
            .and_where(Expr::col(Alias::new(token_column)).eq(token))
            .and_where(Expr::col(Alias::new(expiry_column)).gt(now_seconds()))
            .build(SqliteQueryBuilder);
        Ok(bind(&statement)?
            .fetch_optional(&self.pool)
            .await?
            .map(|row| TokenGrant {
                client_id: row.get("client_id"),
                open_id: row.get("open_id"),
                resource: row.get("resource"),
            }))
    }

    /// Insert one row containing both tokens inside the caller's transaction.
    async fn insert_pair(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        access_token: &str,
        refresh_token: &str,
        grant: &TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<()> {
        let statement = Query::insert()
            .into_table(Alias::new(TABLE_TOKENS))
            .columns([
                Alias::new("client_id"),
                Alias::new("open_id"),
                Alias::new("resource"),
                Alias::new("access_token"),
                Alias::new("access_expires_at"),
                Alias::new("refresh_token"),
                Alias::new("refresh_expires_at"),
            ])
            .values_panic([
                grant.client_id.clone().into(),
                grant.open_id.clone().into(),
                grant.resource.clone().into(),
                access_token.into(),
                access_expires_at.into(),
                refresh_token.into(),
                refresh_expires_at.into(),
            ])
            .build(SqliteQueryBuilder);
        bind(&statement)?.execute(&mut **transaction).await?;
        Ok(())
    }

    /// Remove rows only after both tokens have expired.
    async fn prune_expired(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        let now = now_seconds();
        let statement = Query::delete()
            .from_table(Alias::new(TABLE_TOKENS))
            .and_where(Expr::col(Alias::new("access_expires_at")).lte(now))
            .and_where(Expr::col(Alias::new("refresh_expires_at")).lte(now))
            .build(SqliteQueryBuilder);
        bind(&statement)?.execute(&mut **transaction).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl DbApi for SqliteStore {
    type Error = anyhow::Error;

    async fn get_client(&self, client_id: &str) -> Result<Option<Client>> {
        let statement = Query::select()
            .columns([Alias::new("client_id"), Alias::new("redirect_uris")])
            .from(Alias::new(TABLE_CLIENTS))
            .and_where(Expr::col(Alias::new("client_id")).eq(client_id))
            .build(SqliteQueryBuilder);
        bind(&statement)?
            .fetch_optional(&self.pool)
            .await?
            .map(|row| {
                Ok(Client {
                    client_id: row.get("client_id"),
                    redirect_uris: serde_json::from_str(&row.get::<String, _>("redirect_uris"))?,
                })
            })
            .transpose()
    }

    async fn insert_client(&self, client: Client) -> Result<()> {
        let redirects = serde_json::to_string(&client.redirect_uris)?;
        let statement = Query::insert()
            .into_table(Alias::new(TABLE_CLIENTS))
            .columns([Alias::new("client_id"), Alias::new("redirect_uris")])
            .values_panic([client.client_id.into(), redirects.into()])
            .build(SqliteQueryBuilder);
        bind(&statement)?.execute(&self.pool).await?;
        Ok(())
    }

    async fn insert_pending(
        &self,
        state: &str,
        pending: PendingLogin,
        expires_at: i64,
    ) -> Result<()> {
        self.cache
            .set(
                format!("{PENDING_PREFIX}{state}"),
                serde_json::to_vec(&pending)?,
                expires_at,
            )
            .await;
        Ok(())
    }

    async fn delete_pending(&self, state: &str) -> Result<Option<PendingLogin>> {
        self.cache
            .get_del(&format!("{PENDING_PREFIX}{state}"))
            .await
            .map(|value| serde_json::from_slice(&value).map_err(Into::into))
            .transpose()
    }

    async fn get_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.cache
            .get(&format!("{CODE_PREFIX}{code}"))
            .await
            .map(|value| serde_json::from_slice(&value).map_err(Into::into))
            .transpose()
    }

    async fn insert_code(
        &self,
        code: &str,
        grant: AuthorizationCode,
        expires_at: i64,
    ) -> Result<()> {
        self.cache
            .set(
                format!("{CODE_PREFIX}{code}"),
                serde_json::to_vec(&grant)?,
                expires_at,
            )
            .await;
        Ok(())
    }

    async fn delete_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.cache
            .get_del(&format!("{CODE_PREFIX}{code}"))
            .await
            .map(|value| serde_json::from_slice(&value).map_err(Into::into))
            .transpose()
    }

    async fn get_access_token(&self, token: &str) -> Result<Option<TokenGrant>> {
        self.get_grant("access_token", "access_expires_at", token)
            .await
    }

    async fn get_refresh_token(&self, token: &str) -> Result<Option<TokenGrant>> {
        self.get_grant("refresh_token", "refresh_expires_at", token)
            .await
    }

    /// Write both tokens and remove expired records in one SQLite transaction.
    async fn insert_token_pair(
        &self,
        access_token: &str,
        refresh_token: &str,
        grant: TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        Self::insert_pair(
            &mut transaction,
            access_token,
            refresh_token,
            &grant,
            access_expires_at,
            refresh_expires_at,
        )
        .await?;
        Self::prune_expired(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Atomically replace both tokens when the current refresh token is live.
    async fn replace_refresh_token(
        &self,
        old_token: &str,
        access_token: &str,
        refresh_token: &str,
        grant: TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<bool> {
        let mut transaction = self.pool.begin().await?;
        let statement = Query::update()
            .table(Alias::new(TABLE_TOKENS))
            .value(Alias::new("client_id"), grant.client_id)
            .value(Alias::new("open_id"), grant.open_id)
            .value(Alias::new("resource"), grant.resource)
            .value(Alias::new("access_token"), access_token)
            .value(Alias::new("access_expires_at"), access_expires_at)
            .value(Alias::new("refresh_token"), refresh_token)
            .value(Alias::new("refresh_expires_at"), refresh_expires_at)
            .and_where(Expr::col(Alias::new("refresh_token")).eq(old_token))
            .and_where(Expr::col(Alias::new("refresh_expires_at")).gt(now_seconds()))
            .returning_all()
            .build(SqliteQueryBuilder);
        if bind(&statement)?
            .fetch_optional(&mut *transaction)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        Self::prune_expired(&mut transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_pair_rotation_keeps_one_row_and_invalidates_old_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let dbapi = SqliteStore::open(&directory.path().join("larkpush.sqlite"))
            .await
            .unwrap();
        let grant = TokenGrant {
            client_id: "client".into(),
            open_id: "ou_test".into(),
            resource: "https://example.com/mcp".into(),
        };
        dbapi
            .insert_token_pair(
                "access-1",
                "refresh-1",
                grant.clone(),
                now_seconds() + 60,
                now_seconds() + 120,
            )
            .await
            .unwrap();
        let initial_id: i64 = sqlx::query_scalar("SELECT id FROM oauth_tokens")
            .fetch_one(&dbapi.pool)
            .await
            .unwrap();
        assert!(dbapi.get_access_token("access-1").await.unwrap().is_some());
        assert!(
            dbapi
                .get_refresh_token("refresh-1")
                .await
                .unwrap()
                .is_some()
        );

        assert!(
            dbapi
                .replace_refresh_token(
                    "refresh-1",
                    "access-2",
                    "refresh-2",
                    grant,
                    now_seconds() + 60,
                    now_seconds() + 120,
                )
                .await
                .unwrap()
        );
        assert!(dbapi.get_access_token("access-1").await.unwrap().is_none());
        assert!(
            dbapi
                .get_refresh_token("refresh-1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(dbapi.get_access_token("access-2").await.unwrap().is_some());
        assert!(
            dbapi
                .get_refresh_token("refresh-2")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT id FROM oauth_tokens")
                .fetch_one(&dbapi.pool)
                .await
                .unwrap(),
            initial_id
        );
    }

    #[tokio::test]
    async fn short_lived_grants_are_consumed_once_and_expire() {
        let directory = tempfile::tempdir().unwrap();
        let dbapi = SqliteStore::open(&directory.path().join("larkpush.sqlite"))
            .await
            .unwrap();
        let pending = PendingLogin {
            client_id: "client".into(),
            redirect_uri: "https://example.com/callback".into(),
            client_state: "state".into(),
            challenge: "challenge".into(),
        };
        dbapi
            .insert_pending("live", pending.clone(), now_seconds() + 60)
            .await
            .unwrap();
        assert!(dbapi.delete_pending("live").await.unwrap().is_some());
        assert!(dbapi.delete_pending("live").await.unwrap().is_none());

        dbapi
            .insert_pending("expired", pending, now_seconds() - 1)
            .await
            .unwrap();
        assert!(dbapi.delete_pending("expired").await.unwrap().is_none());

        let code = AuthorizationCode {
            client_id: "client".into(),
            redirect_uri: "https://example.com/callback".into(),
            challenge: "challenge".into(),
            open_id: "ou_test".into(),
        };
        dbapi
            .insert_code("code", code, now_seconds() + 60)
            .await
            .unwrap();
        assert!(dbapi.get_code("code").await.unwrap().is_some());
        assert!(dbapi.delete_code("code").await.unwrap().is_some());
        assert!(dbapi.get_code("code").await.unwrap().is_none());
    }
}
