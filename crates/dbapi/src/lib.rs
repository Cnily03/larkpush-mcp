//! Database API shared by the native SQLite and Cloudflare D1 adapters.

use serde::{Deserialize, Serialize};

/// Registered OAuth client and its allowed redirect destinations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Client {
    #[serde(default)]
    pub client_id: String,
    pub redirect_uris: Vec<String>,
}

/// Login state saved while the browser completes Lark authorization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingLogin {
    pub client_id: String,
    pub redirect_uri: String,
    pub client_state: String,
    pub challenge: String,
}

/// One-time code that binds a Lark user to the client's PKCE challenge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorizationCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub challenge: String,
    pub open_id: String,
}

/// User and resource associated with an access or refresh token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenGrant {
    #[serde(default)]
    pub client_id: String,
    pub open_id: String,
    #[serde(default)]
    pub resource: String,
}

/// Typed database operations, currently for OAuth data and cache entries.
/// Expiration arguments are Unix timestamps in seconds.
/// Native handlers require `Send` futures; Workers can use local D1 futures.
#[cfg_attr(feature = "native", async_trait::async_trait)]
#[cfg_attr(not(feature = "native"), async_trait::async_trait(?Send))]
pub trait DbApi: Send + Sync {
    /// Storage error returned by this adapter.
    type Error;

    /// Find a registered client by its public client ID.
    async fn get_client(&self, client_id: &str) -> Result<Option<Client>, Self::Error>;
    /// Persist a new client and its permitted redirect URIs.
    async fn insert_client(&self, client: Client) -> Result<(), Self::Error>;

    /// Cache a login state until `expires_at` for a later Lark callback.
    async fn insert_pending(
        &self,
        state: &str,
        pending: PendingLogin,
        expires_at: i64,
    ) -> Result<(), Self::Error>;
    /// Consume a live login state once; return `None` if absent or expired.
    async fn delete_pending(&self, state: &str) -> Result<Option<PendingLogin>, Self::Error>;

    /// Look up an unexpired authorization code without consuming it.
    async fn get_code(&self, code: &str) -> Result<Option<AuthorizationCode>, Self::Error>;
    /// Cache an authorization code until `expires_at`.
    async fn insert_code(
        &self,
        code: &str,
        grant: AuthorizationCode,
        expires_at: i64,
    ) -> Result<(), Self::Error>;
    /// Consume a live authorization code once; return `None` if absent or expired.
    async fn delete_code(&self, code: &str) -> Result<Option<AuthorizationCode>, Self::Error>;

    /// Find the grant for an unexpired access token.
    async fn get_access_token(&self, token: &str) -> Result<Option<TokenGrant>, Self::Error>;
    /// Find the grant for an unexpired refresh token.
    async fn get_refresh_token(&self, token: &str) -> Result<Option<TokenGrant>, Self::Error>;
    /// Store an access token and refresh token as one grant with separate expiration times.
    async fn insert_token_pair(
        &self,
        access_token: &str,
        refresh_token: &str,
        grant: TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<(), Self::Error>;
    /// Replace a live grant's token pair, invalidating its previous access and refresh tokens.
    /// Return `false` when the old token is absent, expired, or already consumed.
    async fn replace_refresh_token(
        &self,
        old_token: &str,
        access_token: &str,
        refresh_token: &str,
        grant: TokenGrant,
        access_expires_at: i64,
        refresh_expires_at: i64,
    ) -> Result<bool, Self::Error>;
}
