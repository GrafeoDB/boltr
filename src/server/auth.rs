//! Authentication validator trait for Bolt servers.

use crate::error::BoltError;
use crate::server::AuthCredentials;

/// Validates authentication credentials during the LOGON phase.
#[async_trait::async_trait]
pub trait AuthValidator: Send + Sync + 'static {
    /// Validate the given credentials.
    /// Return `Ok(())` to accept, or `Err(BoltError)` to reject.
    async fn validate(&self, credentials: &AuthCredentials) -> Result<(), BoltError>;
}
