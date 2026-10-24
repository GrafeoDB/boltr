//! A minimal BoltR server for testing official Neo4j drivers against BoltR.
//!
//! The backend understands one fake statement, `ROWS n`, which returns `n`
//! records `[i]` (column `i`, `i` from 0). Any credentials are accepted.
//!
//! ```text
//! cargo run --example driver_server -- 127.0.0.1:7687
//! ```
//!
//! The address defaults to `127.0.0.1:0` (a free port). The server prints
//! `listening on <address>` once it accepts connections, which the driver
//! tests in `tests/drivers/` read to find the port.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::TcpListener;

use boltr::error::BoltError;
use boltr::server::{
    BoltBackend, BoltRecord, BoltServer, ResultMetadata, ResultStream, SessionConfig,
    SessionHandle, SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};

#[derive(Default)]
struct RowsBackend {
    next_id: AtomicU64,
}

impl RowsBackend {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

#[async_trait::async_trait]
impl BoltBackend for RowsBackend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        Ok(SessionHandle(self.next_id("s")))
    }

    async fn close_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn configure_session(
        &self,
        _session: &SessionHandle,
        _property: SessionProperty,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn reset_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn execute(
        &self,
        _session: &SessionHandle,
        query: &str,
        _parameters: &HashMap<String, BoltValue>,
        _extra: &BoltDict,
        _transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError> {
        let count = query
            .strip_prefix("ROWS ")
            .and_then(|n| n.trim().parse::<i64>().ok())
            .ok_or_else(|| BoltError::Query {
                code: "Neo.ClientError.Statement.SyntaxError".into(),
                message: format!("unknown statement: {query}"),
            })?;
        Ok(ResultStream {
            metadata: ResultMetadata {
                columns: vec!["i".into()],
                extra: BoltDict::new(),
            },
            records: (0..count)
                .map(|i| BoltRecord {
                    values: vec![BoltValue::Integer(i)],
                })
                .collect(),
            summary: BoltDict::new(),
        })
    }

    async fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        Ok(TransactionHandle(self.next_id("tx")))
    }

    async fn commit(
        &self,
        _session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::from([(
            "bookmark".to_string(),
            BoltValue::String(format!("driver-server:{}", transaction.0)),
        )]))
    }

    async fn rollback(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::from([(
            "server".to_string(),
            BoltValue::String(format!("BoltR/{}", env!("CARGO_PKG_VERSION"))),
        )]))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoltError> {
    let addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:0".to_string())
        .parse()
        .map_err(|e| BoltError::Protocol(format!("invalid address: {e}")))?;
    let listener = TcpListener::bind(addr).await?;
    println!("listening on {}", listener.local_addr()?);
    std::io::stdout().flush()?;
    BoltServer::builder(RowsBackend::default())
        .serve_listener(listener)
        .await
}
