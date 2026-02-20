//! High-level Bolt session — connect, authenticate, run queries.

use std::collections::HashMap;
use std::net::SocketAddr;

use crate::error::BoltError;
use crate::types::{BoltDict, BoltValue};

use super::connection::BoltConnection;

/// A high-level Bolt session that handles connection, authentication,
/// and provides a convenient query API.
pub struct BoltSession {
    conn: BoltConnection,
}

impl BoltSession {
    /// Connects and authenticates (HELLO + LOGON with "none" scheme).
    pub async fn connect(addr: SocketAddr) -> Result<Self, BoltError> {
        let mut conn = BoltConnection::connect(addr).await?;
        let extra = BoltDict::from([(
            "user_agent".to_string(),
            BoltValue::String("boltr-client/0.1".to_string()),
        )]);
        conn.hello(extra).await?;
        conn.logon("none", None, None).await?;
        Ok(Self { conn })
    }

    /// Connects and authenticates with basic auth.
    pub async fn connect_basic(
        addr: SocketAddr,
        username: &str,
        password: &str,
    ) -> Result<Self, BoltError> {
        let mut conn = BoltConnection::connect(addr).await?;
        let extra = BoltDict::from([(
            "user_agent".to_string(),
            BoltValue::String("boltr-client/0.1".to_string()),
        )]);
        conn.hello(extra).await?;
        conn.logon("basic", Some(username), Some(password)).await?;
        Ok(Self { conn })
    }

    /// Returns the negotiated Bolt version.
    pub fn version(&self) -> (u8, u8) {
        self.conn.version()
    }

    /// Runs a query and returns all results (auto-commit).
    pub async fn run(
        &mut self,
        query: &str,
    ) -> Result<QueryResult, BoltError> {
        self.run_with_params(query, HashMap::new(), BoltDict::new())
            .await
    }

    /// Runs a query with parameters and extra metadata.
    pub async fn run_with_params(
        &mut self,
        query: &str,
        params: HashMap<String, BoltValue>,
        extra: BoltDict,
    ) -> Result<QueryResult, BoltError> {
        let run_meta = self.conn.run(query, params, extra).await?;

        let columns: Vec<String> = run_meta
            .get("fields")
            .and_then(|v| {
                if let BoltValue::List(items) = v {
                    Some(
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(String::from))
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let (records, summary) = self.conn.pull_all().await?;

        Ok(QueryResult {
            columns,
            records,
            summary,
        })
    }

    /// Begins an explicit transaction.
    pub async fn begin(&mut self) -> Result<(), BoltError> {
        self.conn.begin(BoltDict::new()).await
    }

    /// Commits the current transaction.
    pub async fn commit(&mut self) -> Result<(), BoltError> {
        self.conn.commit().await?;
        Ok(())
    }

    /// Rolls back the current transaction.
    pub async fn rollback(&mut self) -> Result<(), BoltError> {
        self.conn.rollback().await
    }

    /// Resets the connection to a clean state.
    pub async fn reset(&mut self) -> Result<(), BoltError> {
        self.conn.reset().await
    }

    /// Sends GOODBYE (graceful disconnect).
    pub async fn close(mut self) -> Result<(), BoltError> {
        self.conn.goodbye().await
    }

    /// Returns a mutable reference to the underlying connection
    /// for advanced operations.
    pub fn connection(&mut self) -> &mut BoltConnection {
        &mut self.conn
    }
}

/// Result of a Bolt query execution.
#[derive(Debug)]
pub struct QueryResult {
    /// Column names from the RUN metadata.
    pub columns: Vec<String>,
    /// Records (rows), each a list of `BoltValue`.
    pub records: Vec<Vec<BoltValue>>,
    /// Summary metadata from the final PULL SUCCESS.
    pub summary: BoltDict,
}
