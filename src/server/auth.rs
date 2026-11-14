//! Authentication validator trait for Bolt servers.

use std::net::SocketAddr;

use crate::error::BoltError;
use crate::server::AuthCredentials;

/// Authenticated identity returned by the validator.
#[derive(Debug, Clone, Default)]
pub struct AuthInfo {
    /// Authenticated principal (user/token identifier).
    pub principal: String,
    /// If true, the client should change credentials before proceeding.
    pub credentials_expired: bool,
}

/// What the server knows about the connection a LOGON arrives on.
///
/// Non-exhaustive, so a later field is not a breaking change: read its
/// fields, and build one with [`AuthContext::new`] (for example in a test of
/// a validator).
///
/// ```
/// use boltr::server::AuthContext;
///
/// let context = AuthContext::new("192.0.2.7:51234".parse().unwrap());
/// assert_eq!(context.peer_addr.port(), 51234);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuthContext {
    /// The client's address: the TCP peer of the connection, before TLS or
    /// WebSocket. Behind a proxy this is the proxy's address.
    pub peer_addr: SocketAddr,
}

impl AuthContext {
    /// The context of a connection from `peer_addr`.
    #[must_use]
    pub fn new(peer_addr: SocketAddr) -> Self {
        Self { peer_addr }
    }
}

/// Validates authentication credentials during the LOGON phase.
#[async_trait::async_trait]
pub trait AuthValidator: Send + Sync + 'static {
    /// Validate the given credentials, sent on the connection `context`
    /// describes (for example to log the source of a failed attempt).
    /// Return `Ok(AuthInfo)` with the authenticated identity, or `Err(BoltError)` to reject.
    async fn validate(
        &self,
        credentials: &AuthCredentials,
        context: &AuthContext,
    ) -> Result<AuthInfo, BoltError>;
}
