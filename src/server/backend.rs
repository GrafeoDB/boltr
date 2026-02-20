//! The `BoltBackend` trait — core abstraction for Bolt server implementations.

use std::collections::HashMap;

use crate::error::BoltError;
use crate::types::{BoltDict, BoltValue};

/// Opaque handle identifying a Bolt session (one per TCP connection).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionHandle(pub String);

/// Opaque handle identifying a transaction within a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionHandle(pub String);

/// Configuration extracted from the HELLO message.
pub struct SessionConfig {
    pub user_agent: String,
    pub database: Option<String>,
}

/// A session property that can be modified.
pub enum SessionProperty {
    Database(String),
}

/// Transaction access mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
}

/// Authentication credentials extracted from HELLO/LOGON.
#[derive(Debug, Clone)]
pub struct AuthCredentials {
    pub scheme: String,
    pub principal: Option<String>,
    pub credentials: Option<String>,
}

/// A single row of query results.
#[derive(Debug, Clone)]
pub struct BoltRecord {
    pub values: Vec<BoltValue>,
}

/// Metadata about a query result set.
#[derive(Debug, Clone)]
pub struct ResultMetadata {
    pub columns: Vec<String>,
    pub extra: BoltDict,
}

/// A complete query result: metadata + records + summary.
#[derive(Debug, Clone)]
pub struct ResultStream {
    pub metadata: ResultMetadata,
    pub records: Vec<BoltRecord>,
    pub summary: BoltDict,
}

/// The core backend trait that Bolt server implementations must provide.
///
/// One session maps to one TCP connection. The connection handler calls
/// these methods in response to Bolt messages.
#[async_trait::async_trait]
pub trait BoltBackend: Send + Sync + 'static {
    // -- Session lifecycle --

    /// Create a new session. Called once during HELLO processing.
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError>;

    /// Close a session and release resources. Called on GOODBYE or disconnect.
    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError>;

    /// Update a session property (e.g., switch database).
    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), BoltError>;

    /// Reset session to clean state (default database, no transaction).
    async fn reset_session(&self, session: &SessionHandle) -> Result<(), BoltError>;

    // -- Query execution --

    /// Execute a query. The `extra` dict may contain `db`, `language`, `timeout`, etc.
    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        extra: &BoltDict,
        transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError>;

    // -- Transactions --

    /// Begin an explicit transaction.
    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError>;

    /// Commit the current explicit transaction.
    async fn commit(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError>;

    /// Roll back the current explicit transaction.
    async fn rollback(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<(), BoltError>;

    // -- Server info --

    /// Returns metadata to include in the HELLO SUCCESS response.
    async fn get_server_info(&self) -> Result<BoltDict, BoltError>;
}
