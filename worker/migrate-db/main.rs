//! Import supported persistent records from native SQLite into the Worker's D1 database.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser};
use sea_query::{Alias, OnConflict, Query, SqliteQueryBuilder, Value};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tempfile::{Builder, NamedTempFile};

const BINDING: &str = "LARKPUSH_DB";
const BATCH_SIZE: usize = 500;
const DEFAULT_CONFIG: &str = "wrangler.toml";

#[derive(Parser)]
#[command(
    name = "migrate-db",
    about = "Import supported persistent data from SQLite into Cloudflare D1",
    long_about = "Import registered clients and unexpired token pairs from a SQLite file into Cloudflare D1. Pending logins and authorization codes are cache entries and are never migrated."
)]
#[command(group(ArgGroup::new("destination").required(true).multiple(false)))]
struct Args {
    /// Path to the source SQLite database file.
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Wrangler configuration containing the target D1 binding.
    #[arg(short = 'c', long = "config", value_name = "FILE", default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// Import into the local D1 database used by wrangler dev.
    #[arg(long, group = "destination")]
    local: bool,
    /// Import into the deployed Cloudflare D1 database.
    #[arg(long, group = "destination")]
    remote: bool,
}

/// Persistent records supported by the native-to-Worker import.
#[derive(Debug)]
enum Record {
    Client {
        client_id: String,
        redirect_uris: String,
    },
    TokenPair {
        access_token: String,
        access_expires_at: i64,
        refresh_token: String,
        refresh_expires_at: i64,
        client_id: String,
        open_id: String,
        resource: String,
    },
}

/// Resolved Wrangler configuration and D1 location.
struct Destination {
    config: PathBuf,
    mode: &'static str,
}

/// Read the source database, initialize D1, and import records in batches.
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let destination = destination(&args.config, args.local)?;
    let source = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot open source SQLite file {}", args.path.display()))?;
    if !source.is_file() {
        bail!("source must be a SQLite file: {}", source.display());
    }
    let records = read_records(&source).await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let (records, expired) = filter_expired(records, now);
    println!(
        "Importing {} records; skipping {expired} expired token pairs.",
        records.len()
    );

    let schema = root.join("worker/src/db/migrations/0001_initial.sql");
    execute_file(&root, &destination, &schema, true)?;
    for batch in records.chunks(BATCH_SIZE) {
        let mut file = Builder::new()
            .prefix("larkpush-migrate-")
            .suffix(".sql")
            .tempfile()
            .context("failed to create private migration file")?;
        for record in batch {
            write_record(&mut file, record)?;
        }
        file.flush()?;
        execute_file(&root, &destination, file.path(), false)?;
    }
    println!("D1 import complete.");
    Ok(())
}

/// Load the selected Wrangler file and validate its target D1 binding.
fn destination(config: &Path, local: bool) -> Result<Destination> {
    let config = config
        .canonicalize()
        .with_context(|| format!("cannot open Wrangler config {}", config.display()))?;
    let contents =
        fs::read_to_string(&config).with_context(|| format!("cannot read {}", config.display()))?;
    let parsed: toml::Value = toml::from_str(&contents)?;
    let binding = parsed
        .get("d1_databases")
        .and_then(toml::Value::as_array)
        .and_then(|databases| {
            databases.iter().find(|database| {
                database.get("binding").and_then(toml::Value::as_str) == Some(BINDING)
            })
        })
        .context("Wrangler config has no LARKPUSH_DB D1 binding")?;
    if !local
        && binding
            .get("database_id")
            .and_then(toml::Value::as_str)
            .is_none_or(|id| id.is_empty() || id.starts_with("REPLACE_WITH_"))
    {
        bail!(
            "set the D1 database_id in {} before a remote import",
            config.display()
        );
    }
    Ok(Destination {
        config,
        mode: if local { "--local" } else { "--remote" },
    })
}

/// Execute one SQL file against the selected D1 database through Wrangler.
fn execute_file(root: &Path, destination: &Destination, file: &Path, yes: bool) -> Result<()> {
    let config = destination.config.to_str().context("invalid config path")?;
    let file = file.to_str().context("invalid SQL file path")?;
    let mut args = vec![
        "d1",
        "execute",
        BINDING,
        destination.mode,
        "-c",
        config,
        "--file",
        file,
    ];
    if yes {
        args.push("--yes");
    }
    run_wrangler(root, &args)
}

/// Read only persistent clients and token grants from the native SQLite database.
async fn read_records(database: &Path) -> Result<Vec<Record>> {
    if !database.is_file() {
        bail!("source SQLite file was not found: {}", database.display());
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(database)
                .read_only(true),
        )
        .await?;
    let clients = Query::select()
        .columns([Alias::new("client_id"), Alias::new("redirect_uris")])
        .from(Alias::new("oauth_clients"))
        .to_string(SqliteQueryBuilder);
    let rows = sqlx::query(&clients).fetch_all(&pool).await.context(
        "source database has no current OAuth tables; start with a fresh SQLite database",
    )?;
    let mut records = Vec::new();
    for row in rows {
        records.push(Record::Client {
            client_id: row.get("client_id"),
            redirect_uris: row.get("redirect_uris"),
        });
    }
    let tokens = Query::select()
        .columns([
            Alias::new("access_token"),
            Alias::new("access_expires_at"),
            Alias::new("refresh_token"),
            Alias::new("refresh_expires_at"),
            Alias::new("client_id"),
            Alias::new("open_id"),
            Alias::new("resource"),
        ])
        .from(Alias::new("oauth_tokens"))
        .to_string(SqliteQueryBuilder);
    for row in sqlx::query(&tokens).fetch_all(&pool).await? {
        records.push(Record::TokenPair {
            access_token: row.get("access_token"),
            access_expires_at: row.get("access_expires_at"),
            refresh_token: row.get("refresh_token"),
            refresh_expires_at: row.get("refresh_expires_at"),
            client_id: row.get("client_id"),
            open_id: row.get("open_id"),
            resource: row.get("resource"),
        });
    }
    pool.close().await;
    Ok(records)
}

/// Omit token pairs only after both tokens have expired.
fn filter_expired(records: Vec<Record>, now: i64) -> (Vec<Record>, usize) {
    let mut retained = Vec::with_capacity(records.len());
    let mut expired = 0;
    for record in records {
        if let Record::TokenPair {
            access_expires_at,
            refresh_expires_at,
            ..
        } = &record
            && *access_expires_at <= now
            && *refresh_expires_at <= now
        {
            expired += 1;
            continue;
        }
        retained.push(record);
    }
    (retained, expired)
}

/// Write an escaped, repeatable D1 upsert for one OAuth record.
fn write_record(file: &mut NamedTempFile, record: &Record) -> Result<()> {
    let (table, columns, values): (&str, &[&str], Vec<Value>) = match record {
        Record::Client {
            client_id,
            redirect_uris,
        } => (
            "oauth_clients",
            &["client_id", "redirect_uris"],
            vec![client_id.clone().into(), redirect_uris.clone().into()],
        ),
        Record::TokenPair {
            access_token,
            access_expires_at,
            refresh_token,
            refresh_expires_at,
            client_id,
            open_id,
            resource,
        } => (
            "oauth_tokens",
            &[
                "access_token",
                "access_expires_at",
                "refresh_token",
                "refresh_expires_at",
                "client_id",
                "open_id",
                "resource",
            ],
            vec![
                access_token.clone().into(),
                (*access_expires_at).into(),
                refresh_token.clone().into(),
                (*refresh_expires_at).into(),
                client_id.clone().into(),
                open_id.clone().into(),
                resource.clone().into(),
            ],
        ),
    };
    let key = if matches!(record, Record::Client { .. }) {
        "client_id"
    } else {
        "access_token"
    };
    let mut query = Query::insert();
    query
        .into_table(Alias::new(table))
        .columns(columns.iter().map(|column| Alias::new(*column)))
        .values_panic(values.into_iter().map(Into::into));
    query.on_conflict(
        OnConflict::column(Alias::new(key))
            .update_columns(
                columns
                    .iter()
                    .filter(|column| **column != key)
                    .map(|column| Alias::new(*column)),
            )
            .to_owned(),
    );
    writeln!(file, "{};", query.to_string(SqliteQueryBuilder))?;
    Ok(())
}

/// Run Wrangler from the project root and surface command failures.
fn run_wrangler(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("wrangler")
        .args(args)
        .current_dir(root)
        .status()
        .context("failed to run Wrangler; install it and add it to PATH")?;
    if !status.success() {
        bail!("Wrangler command failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Args, DEFAULT_CONFIG, filter_expired, read_records, write_record};
    use clap::Parser;
    use sea_query::{Alias, Query, SqliteQueryBuilder};
    use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
    use std::path::Path;

    #[test]
    fn accepts_default_and_custom_wrangler_config() {
        let local = Args::try_parse_from(["migrate-db", "source.sqlite", "--local"]).unwrap();
        assert_eq!(local.config, Path::new(DEFAULT_CONFIG));

        let remote = Args::try_parse_from([
            "migrate-db",
            "source.sqlite",
            "--remote",
            "-c",
            "custom.toml",
        ])
        .unwrap();
        assert_eq!(remote.config, Path::new("custom.toml"));
    }

    #[tokio::test]
    async fn imports_current_sqlite_schema_into_d1_schema() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("larkpush.sqlite");
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(&source)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql(include_str!("../src/db/migrations/0001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let client = Query::insert()
            .into_table(Alias::new("oauth_clients"))
            .columns([Alias::new("client_id"), Alias::new("redirect_uris")])
            .values_panic([
                "it's a client".into(),
                r#"["https://example.com/callback"]"#.into(),
            ])
            .to_string(SqliteQueryBuilder);
        sqlx::query(&client).execute(&pool).await.unwrap();
        let token = Query::insert()
            .into_table(Alias::new("oauth_tokens"))
            .columns([
                Alias::new("access_token"),
                Alias::new("access_expires_at"),
                Alias::new("refresh_token"),
                Alias::new("refresh_expires_at"),
                Alias::new("client_id"),
                Alias::new("open_id"),
                Alias::new("resource"),
            ])
            .values_panic([
                "live-token".into(),
                200_i64.into(),
                "live-refresh".into(),
                300_i64.into(),
                "it's a client".into(),
                "ou_test".into(),
                "old-resource".into(),
            ])
            .to_string(SqliteQueryBuilder);
        sqlx::query(&token).execute(&pool).await.unwrap();
        // A previous schema may still contain cache rows; the import must ignore them.
        sqlx::raw_sql(
            "CREATE TABLE oauth_pending_logins (state TEXT); \
             INSERT INTO oauth_pending_logins VALUES ('pending'); \
             CREATE TABLE oauth_authorization_codes (code TEXT); \
             INSERT INTO oauth_authorization_codes VALUES ('code');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let records = read_records(&source).await.unwrap();
        assert_eq!(records.len(), 2);
        let (records, expired) = filter_expired(records, 100);
        assert_eq!(expired, 0);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for record in &records {
            write_record(&mut file, record).unwrap();
        }
        let sql = std::fs::read_to_string(file.path()).unwrap();
        assert!(sql.contains("it''s a client"));

        let destination = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../src/db/migrations/0001_initial.sql"))
            .execute(&destination)
            .await
            .unwrap();
        sqlx::raw_sql(&sql).execute(&destination).await.unwrap();
        let row = sqlx::query("SELECT id, access_token, refresh_token, resource FROM oauth_tokens")
            .fetch_one(&destination)
            .await
            .unwrap();
        assert!(row.get::<i64, _>("id") > 0);
        assert_eq!(row.get::<String, _>("access_token"), "live-token");
        assert_eq!(row.get::<String, _>("refresh_token"), "live-refresh");
        assert_eq!(row.get::<String, _>("resource"), "old-resource");
    }
}
