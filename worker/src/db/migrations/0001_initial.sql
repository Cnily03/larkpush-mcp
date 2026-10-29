-- Initial Cloudflare D1 database schema. OAuth tables store durable grants.
DROP TABLE IF EXISTS oauth_pending_logins;
DROP TABLE IF EXISTS oauth_authorization_codes;
DROP TABLE IF EXISTS oauth_access_tokens;
DROP TABLE IF EXISTS oauth_refresh_tokens;

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id TEXT PRIMARY KEY,
    redirect_uris TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_tokens (
    id INTEGER PRIMARY KEY,
    client_id TEXT NOT NULL,
    open_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    access_token TEXT NOT NULL UNIQUE,
    access_expires_at INTEGER NOT NULL,
    refresh_token TEXT NOT NULL UNIQUE,
    refresh_expires_at INTEGER NOT NULL
);
