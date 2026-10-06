//! Arbitrary bytes sent by a client after the version handshake: the server
//! connection must never panic, and must finish once the client stops
//! sending (no hang, whatever the input).

#![no_main]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use boltr::error::BoltError;
use boltr::server::connection::Connection;
use boltr::server::{
    BoltBackend, BoltRecord, ResultMetadata, ResultStream, SessionConfig, SessionHandle,
    SessionManager, SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use libfuzzer_sys::fuzz_target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers every statement with three small records.
struct Backend;

#[async_trait::async_trait]
impl BoltBackend for Backend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        Ok(SessionHandle("fuzz".into()))
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
        if query.is_empty() {
            return Err(BoltError::Query {
                code: "Neo.ClientError.Statement.SyntaxError".into(),
                message: "empty".into(),
            });
        }
        Ok(ResultStream {
            metadata: ResultMetadata {
                columns: vec!["x".into()],
                extra: BoltDict::new(),
            },
            records: (0..3)
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
        Ok(TransactionHandle("tx".into()))
    }

    async fn commit(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }

    async fn rollback(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }
}

fuzz_target!(|data: &[u8]| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let mut connection = Connection::new(
            server_read,
            server_write,
            Arc::new(Backend),
            Arc::new(SessionManager::new(Some(4))),
            None,
            "127.0.0.1:7687".parse().expect("address"),
            Some(1 << 20),
        );
        let (mut client_read, mut client_write) = tokio::io::split(client);
        let input = data.to_vec();

        let feed = async move {
            let _ = client_write.write_all(&input).await;
            let _ = client_write.shutdown().await;
        };
        let drain = async move {
            let mut sink = vec![0u8; 8192];
            while matches!(client_read.read(&mut sink).await, Ok(n) if n > 0) {}
        };
        let serve = async move {
            let _ = connection.run().await;
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(feed, drain, serve);
        })
        .await
        .expect("the connection must finish once the client stops sending");
    });
});
