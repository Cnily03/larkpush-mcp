//! Short-lived OAuth state stored in Upstash through its TLS Redis endpoint.

use larkpush_dbapi::{AuthorizationCode, PendingLogin};
use percent_encoding::percent_decode_str;
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use url::Url;
use worker::{Error, Result, SecureTransport, Socket};

const PENDING_PREFIX: &str = "larkpush:pending:";
const CODE_PREFIX: &str = "larkpush:code:";
const MAX_VALUE_SIZE: usize = 64 * 1024;

pub struct RedisCache {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl RedisCache {
    /// Validate and retain an Upstash TLS Redis connection URL.
    pub fn new(value: &str) -> Result<Self> {
        let url = Url::parse(value).map_err(|_| invalid_url())?;
        if url.scheme() != "rediss"
            || url.host_str().is_none()
            || url.password().is_none_or(str::is_empty)
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid_url());
        }
        let password = percent_decode_str(url.password().unwrap_or_default())
            .decode_utf8()
            .map_err(|_| invalid_url())?
            .into_owned();
        if password.is_empty() {
            return Err(invalid_url());
        }
        Ok(Self {
            host: url.host_str().unwrap_or_default().to_owned(),
            port: url.port().unwrap_or(6379),
            username: if url.username().is_empty() {
                "default".to_owned()
            } else {
                percent_decode_str(url.username())
                    .decode_utf8()
                    .map_err(|_| invalid_url())?
                    .into_owned()
            },
            password,
        })
    }

    /// Store a login state with a Redis expiry matching the OAuth deadline.
    pub async fn insert_pending(
        &self,
        state: &str,
        value: &PendingLogin,
        expires_at: i64,
    ) -> Result<()> {
        self.insert(&format!("{PENDING_PREFIX}{state}"), value, expires_at)
            .await
    }

    /// Atomically consume a login state across Worker instances.
    pub async fn delete_pending(&self, state: &str) -> Result<Option<PendingLogin>> {
        self.get(&format!("{PENDING_PREFIX}{state}"), true).await
    }

    /// Read an authorization code for PKCE validation without consuming it.
    pub async fn get_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.get(&format!("{CODE_PREFIX}{code}"), false).await
    }

    /// Store an authorization code with a Redis expiry matching the OAuth deadline.
    pub async fn insert_code(
        &self,
        code: &str,
        value: &AuthorizationCode,
        expires_at: i64,
    ) -> Result<()> {
        self.insert(&format!("{CODE_PREFIX}{code}"), value, expires_at)
            .await
    }

    /// Atomically consume an authorization code across Worker instances.
    pub async fn delete_code(&self, code: &str) -> Result<Option<AuthorizationCode>> {
        self.get(&format!("{CODE_PREFIX}{code}"), true).await
    }

    /// Send SET with a TTL; expired values are never inserted.
    async fn insert<T: Serialize>(&self, key: &str, value: &T, expires_at: i64) -> Result<()> {
        let ttl = expires_at - (js_sys::Date::now() / 1000.0) as i64;
        if ttl <= 0 {
            return Ok(());
        }
        let value = serde_json::to_string(value).map_err(redis_error)?;
        if value.len() > MAX_VALUE_SIZE {
            return Err(Error::RustError("OAuth cache entry is too large".into()));
        }
        let ttl = ttl.to_string();
        match self.command(&["SET", key, &value, "EX", &ttl]).await? {
            Some(reply) if reply == b"OK" => Ok(()),
            _ => Err(protocol_error()),
        }
    }

    /// Read or atomically consume a JSON value from Redis.
    async fn get<T: DeserializeOwned>(&self, key: &str, consume: bool) -> Result<Option<T>> {
        let command = if consume { "GETDEL" } else { "GET" };
        self.command(&[command, key])
            .await?
            .map(|reply| serde_json::from_slice(&reply).map_err(redis_error))
            .transpose()
    }

    /// Open one TLS socket for AUTH and one Redis command.
    async fn command(&self, arguments: &[&str]) -> Result<Option<Vec<u8>>> {
        let socket = Socket::builder()
            .secure_transport(SecureTransport::On)
            .connect(&self.host, self.port)?;
        socket.opened().await?;
        let mut reader = BufReader::new(socket);
        write_command(reader.get_mut(), &["AUTH", &self.username, &self.password]).await?;
        if read_reply(&mut reader).await?.as_deref() != Some(b"OK") {
            return Err(protocol_error());
        }
        write_command(reader.get_mut(), arguments).await?;
        read_reply(&mut reader).await
    }
}

/// Encode command arguments as length-prefixed RESP bulk strings.
async fn write_command(socket: &mut Socket, arguments: &[&str]) -> Result<()> {
    let mut wire = format!("*{}\r\n", arguments.len()).into_bytes();
    for argument in arguments {
        wire.extend_from_slice(format!("${}\r\n", argument.len()).as_bytes());
        wire.extend_from_slice(argument.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    socket.write_all(&wire).await.map_err(redis_error)
}

/// Decode the simple or bulk reply required by AUTH, SET, GET, and GETDEL.
async fn read_reply(reader: &mut BufReader<Socket>) -> Result<Option<Vec<u8>>> {
    let mut line = String::new();
    reader.read_line(&mut line).await.map_err(redis_error)?;
    let body = line.strip_suffix("\r\n").ok_or_else(protocol_error)?;
    if body.is_empty() {
        return Err(protocol_error());
    }
    match body.split_at(1) {
        ("+", value) => Ok(Some(value.as_bytes().to_vec())),
        ("$", "-1") => Ok(None),
        ("$", length) => {
            let length: usize = length.parse().map_err(|_| protocol_error())?;
            if length > MAX_VALUE_SIZE {
                return Err(protocol_error());
            }
            let mut value = vec![0; length + 2];
            reader.read_exact(&mut value).await.map_err(redis_error)?;
            if !value.ends_with(b"\r\n") {
                return Err(protocol_error());
            }
            value.truncate(length);
            Ok(Some(value))
        }
        ("-", _) => Err(Error::RustError("Redis command failed".into())),
        _ => Err(protocol_error()),
    }
}

fn invalid_url() -> Error {
    Error::RustError("REDIS_URL must be a rediss:// URL with a password".into())
}

fn protocol_error() -> Error {
    Error::RustError("unexpected Redis response".into())
}

fn redis_error(error: impl std::fmt::Display) -> Error {
    Error::RustError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::RedisCache;

    #[test]
    fn accepts_rediss_urls_with_or_without_a_trailing_slash() {
        for url in [
            "rediss://default:p%40ss@redis.example:6379",
            "rediss://default:p%40ss@redis.example:6379/",
        ] {
            let cache = RedisCache::new(url).unwrap();
            assert_eq!(cache.host, "redis.example");
            assert_eq!(cache.password, "p@ss");
        }
        assert!(RedisCache::new("redis://default:password@redis.example:6379").is_err());
        assert!(RedisCache::new("rediss://default@redis.example:6379").is_err());
        assert!(RedisCache::new("rediss://default:password@redis.example:6379/db").is_err());
    }
}
