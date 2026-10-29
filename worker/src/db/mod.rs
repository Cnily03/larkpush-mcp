//! Worker database adapter using Cloudflare D1 and Upstash Redis for cache entries.

mod redis;

use larkpush_dbapi::{AuthorizationCode, Client, DbApi, PendingLogin, TokenGrant};
use sea_query::{Alias, Expr, ExprTrait, Query, SqliteQueryBuilder, Value, Values};
use serde::Deserialize;
use tokio::sync::OnceCell;
use wasm_bindgen::JsValue;
use worker::{Result, d1::D1Database};

const TABLE_CLIENTS: &str = "oauth_clients";
const TABLE_TOKENS: &str = "oauth_tokens";

// Each isolate initializes the idempotent schema once; failures remain retryable.
static SCHEMA_READY: OnceCell<()> = OnceCell::const_new();

type Statement = (String, Values);

#[derive(Deserialize)]
struct ClientRow {
    client_id: String,
    redirect_uris: String,
}

/// Cloudflare D1 adapter for the shared database API.
pub struct D1Store {
    db: D1Database,
    cache: redis::RedisCache,
}

impl D1Store {
    /// Initialize D1 on first use, then bind persistent and cached storage.
    pub async fn new(db: D1Database, redis_url: &str) -> Result<Self> {
        SCHEMA_READY
            .get_or_try_init(|| async {
                // D1 exec splits on newlines, so batch complete SQL statements instead.
                let statements = schema_statements()
                    .into_iter()
                    .map(|statement| db.prepare(statement))
                    .collect();
                db.batch(statements).await?;
                log::info!("D1 schema initialized");
                Ok::<(), worker::Error>(())
            })
            .await?;
        Ok(Self {
            db,
            cache: redis::RedisCache::new(redis_url)?,
        })
    }

    /// Execute a prepared D1 query and deserialize its first row.
    async fn first<T: for<'de> Deserialize<'de>>(
        &self,
        statement: &Statement,
    ) -> Result<Option<T>> {
        self.db
            .prepare(&statement.0)
            .bind(&parameters(&statement.1)?)?
            .first(None)
            .await
    }

    /// Execute a prepared D1 statement without returning rows.
    async fn run(&self, statement: &Statement) -> Result<()> {
        self.db
            .prepare(&statement.0)
            .bind(&parameters(&statement.1)?)?
            .run()
            .await?;
        Ok(())
    }

    /// Look up a live grant by either token in the shared D1 table.
    async fn grant(
        &self,
        token_column: &str,
        expiry_column: &str,
        token: &str,
    ) -> Result<Option<TokenGrant>> {
        let statement = select_one(
            TABLE_TOKENS,
            token_column,
            token,
            &["client_id", "open_id", "resource"],
            Some((expiry_column, now_seconds())),
        );
        self.first(&statement).await
    }
}

/// Split the static schema, which contains no semicolons in SQL string literals.
fn schema_statements() -> Vec<String> {
    let schema = include_str!("migrations/0001_initial.sql")
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    schema
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_owned)
        .collect()
}

#[async_trait::async_trait(?Send)]
impl DbApi for D1Store {
    type Error = worker::Error;

    async fn get_client(&self, client_id: &str) -> Result<Option<Client>> {
        let statement = select_one(
            TABLE_CLIENTS,
            "client_id",
            client_id,
            &["client_id", "redirect_uris"],
            None,
        );
        let Some(row): Option<ClientRow> = self.first(&statement).await? else {
            return Ok(None);
        };
        let redirect_uris = serde_json::from_str(&row.redirect_uris)
            .map_err(|error| worker::Error::RustError(error.to_string()))?;
        Ok(Some(Client {
            client_id: row.client_id,
            redirect_uris,
        }))
    }

    async fn insert_client(&self, client: Client) -> Result<()> {
        let redirect_uris = serde_json::to_string(&client.redirect_uris)
            .map_err(|error| worker::Error::RustError(error.to_string()))?;
        let statement = Query::insert()
            .into_table(Alias::new(TABLE_CLIENTS))
            .columns([Alias::new("client_id"), Alias::new("redirect_uris")])
            .values_panic([client.client_id.into(), redirect_uris.into()])
            .build(SqliteQueryBuilder);
        self.run(&statement).await
    }

    async fn insert_pending(
        &self,
        state: &str,
        pending: PendingLogin,
        expires_at: i64,
    ) -> Result<()> {
        self.cache.insert_pending(state, &pending, expires_at).await
    }

    async fn delete_pending(&self, state: &str) -> Result<Option<PendingLogin>> {
        self.cache.delete_pending(state).await
    }

    async fn get_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.cache.get_code(code).await
    }

    async fn insert_code(
        &self,
        code: &str,
        grant: AuthorizationCode,
        expires_at: i64,
    ) -> Result<()> {
        self.cache.insert_code(code, &grant, expires_at).await
    }

    async fn delete_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.cache.delete_code(code).await
    }

    async fn get_access_token(&self, token: &str) -> Result<Option<TokenGrant>> {
        self.grant("access_token", "access_expires_at", token).await
    }

    async fn get_refresh_token(&self, token: &str) -> Result<Option<TokenGrant>> {
        self.grant("refresh_token", "refresh_expires_at", token)
            .await
    }

    /// Batch a token-pair insert with cleanup of fully expired D1 records.
    async fn insert_token_pair(
        &self,
        access_token: &str,
        refresh_token: &str,
        grant: TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<()> {
        let pair = insert_token_pair(
            access_token,
            refresh_token,
            &grant,
            access_expires_at,
            refresh_expires_at,
        );
        let now = now_seconds();
        let expired = Query::delete()
            .from_table(Alias::new(TABLE_TOKENS))
            .and_where(Expr::col(Alias::new("access_expires_at")).lte(now))
            .and_where(Expr::col(Alias::new("refresh_expires_at")).lte(now))
            .build(SqliteQueryBuilder);
        self.db
            .batch(vec![
                self.db.prepare(&pair.0).bind(&parameters(&pair.1)?)?,
                self.db.prepare(&expired.0).bind(&parameters(&expired.1)?)?,
            ])
            .await?;
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
        Ok(self.first::<TokenGrant>(&statement).await?.is_some())
    }
}

/// Build one D1 row for an OAuth token pair.
fn insert_token_pair(
    access_token: &str,
    refresh_token: &str,
    grant: &TokenGrant,
    access_expires_at: i64,
    refresh_expires_at: i64,
) -> Statement {
    insert(
        TABLE_TOKENS,
        &[
            "client_id",
            "open_id",
            "resource",
            "access_token",
            "access_expires_at",
            "refresh_token",
            "refresh_expires_at",
        ],
        vec![
            grant.client_id.clone().into(),
            grant.open_id.clone().into(),
            grant.resource.clone().into(),
            access_token.into(),
            access_expires_at.into(),
            refresh_token.into(),
            refresh_expires_at.into(),
        ],
    )
}

/// Build a parameterized insert for a named OAuth table.
fn insert(table: &str, columns: &[&str], values: Vec<Value>) -> Statement {
    Query::insert()
        .into_table(Alias::new(table))
        .columns(columns.iter().map(|column| Alias::new(*column)))
        .values_panic(values.into_iter().map(Into::into))
        .build(SqliteQueryBuilder)
}

/// Build a keyed select with an optional expiration guard.
fn select_one(
    table: &str,
    key_column: &str,
    key: &str,
    columns: &[&str],
    expiry: Option<(&str, i64)>,
) -> Statement {
    let mut query = Query::select();
    query
        .columns(columns.iter().map(|column| Alias::new(*column)))
        .from(Alias::new(table))
        .and_where(Expr::col(Alias::new(key_column)).eq(key));
    if let Some((column, now)) = expiry {
        query.and_where(Expr::col(Alias::new(column)).gt(now));
    }
    query.build(SqliteQueryBuilder)
}

/// Read Unix time from the Worker runtime for D1 expiry checks.
fn now_seconds() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// Convert SeaQuery values into JavaScript values accepted by D1.
fn parameters(values: &Values) -> Result<Vec<JsValue>> {
    values
        .0
        .iter()
        .map(|value| match value {
            Value::String(Some(value)) => Ok(JsValue::from_str(value)),
            Value::BigInt(Some(value)) => Ok(JsValue::from_f64(*value as f64)),
            Value::Int(Some(value)) => Ok(JsValue::from_f64(*value as f64)),
            Value::String(None) | Value::BigInt(None) | Value::Int(None) => Ok(JsValue::NULL),
            _ => Err(worker::Error::RustError(format!(
                "unsupported D1 query parameter: {value:?}"
            ))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::schema_statements;

    #[test]
    fn schema_is_split_into_complete_d1_statements() {
        let statements = schema_statements();
        assert_eq!(statements.len(), 6);
        assert!(
            statements
                .iter()
                .all(|statement| !statement.starts_with("--"))
        );
        assert!(
            statements.iter().any(|statement| {
                statement.starts_with("CREATE TABLE IF NOT EXISTS oauth_tokens")
            })
        );
    }
}
