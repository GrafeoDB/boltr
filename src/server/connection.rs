//! Per-TCP-connection Bolt handler.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::chunk::{ChunkReader, ChunkWriter};
use crate::error::BoltError;
use crate::message::decode::decode_client_message;
use crate::message::encode::encode_server_message;
use crate::message::request::ClientMessage;
use crate::message::response::ServerMessage;
use crate::server::auth::AuthValidator;
use crate::server::backend::{
    AuthCredentials, BoltBackend, BoltRecord, SessionConfig, SessionHandle, SessionProperty,
    TransactionHandle,
};
use crate::server::session_manager::SessionManager;
use crate::server::state_machine::ConnectionState;
use crate::types::{BoltDict, BoltValue};

/// Buffered query results waiting for PULL/DISCARD.
struct PendingResult {
    records: Vec<BoltRecord>,
    offset: usize,
    #[allow(dead_code)]
    columns: Vec<String>,
    summary: BoltDict,
}

/// Handles a single Bolt TCP connection.
pub struct Connection<R, W, B: BoltBackend> {
    reader: ChunkReader<R>,
    writer: ChunkWriter<W>,
    backend: Arc<B>,
    session_manager: Arc<SessionManager>,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    state: ConnectionState,
    session: Option<SessionHandle>,
    transaction: Option<TransactionHandle>,
    pending_result: Option<PendingResult>,
    peer_addr: SocketAddr,
}

impl<R, W, B> Connection<R, W, B>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    B: BoltBackend,
{
    pub fn new(
        reader: R,
        writer: W,
        backend: Arc<B>,
        session_manager: Arc<SessionManager>,
        auth_validator: Option<Arc<dyn AuthValidator>>,
        peer_addr: SocketAddr,
    ) -> Self {
        Self {
            reader: ChunkReader::new(reader),
            writer: ChunkWriter::new(writer),
            backend,
            session_manager,
            auth_validator,
            state: ConnectionState::Negotiation,
            session: None,
            transaction: None,
            pending_result: None,
            peer_addr,
        }
    }

    /// Runs the connection lifecycle: handshake → message loop → cleanup.
    pub async fn run(&mut self) -> Result<(), BoltError> {
        // Step 1: Handshake (reads magic + versions from the raw stream).
        // Handshake is done externally before constructing Connection, so we
        // start in Negotiation state waiting for HELLO.

        // Step 2: Message loop.
        loop {
            if self.state == ConnectionState::Defunct {
                break;
            }

            let msg_bytes = match self.reader.read_message().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::debug!(%self.peer_addr, error = %e, "read error");
                    break;
                }
            };

            if msg_bytes.is_empty() {
                // NOOP / keep-alive.
                continue;
            }

            let msg = match decode_client_message(&msg_bytes) {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!(%self.peer_addr, error = %e, "decode error");
                    self.send_failure("Neo.ClientError.Request.InvalidFormat", &e.to_string())
                        .await?;
                    self.state = ConnectionState::Failed;
                    continue;
                }
            };

            if !self.state.accepts(&msg) {
                tracing::debug!(
                    %self.peer_addr,
                    state = ?self.state,
                    msg = ?std::mem::discriminant(&msg),
                    "message not allowed in current state",
                );
                if matches!(msg, ClientMessage::Goodbye) {
                    self.state = ConnectionState::Defunct;
                    break;
                }
                self.send_ignored().await?;
                continue;
            }

            let result = self.handle_message(msg.clone()).await;
            match result {
                Ok(()) => {}
                Err(e) => {
                    tracing::debug!(%self.peer_addr, error = %e, "handler error");
                    let meta = e.to_failure_metadata();
                    self.send_message(&ServerMessage::Failure { metadata: meta })
                        .await?;
                    self.state = self.state.transition_failure(&msg);
                }
            }
        }

        // Cleanup.
        if let Some(ref session) = self.session {
            self.session_manager.remove(&session.0);
            let _ = self.backend.close_session(session).await;
        }

        Ok(())
    }

    async fn handle_message(&mut self, msg: ClientMessage) -> Result<(), BoltError> {
        match msg {
            ClientMessage::Hello { ref extra } => self.handle_hello(extra).await,
            ClientMessage::Logon { ref auth } => self.handle_logon(auth).await,
            ClientMessage::Logoff => self.handle_logoff().await,
            ClientMessage::Goodbye => {
                self.state = ConnectionState::Defunct;
                Ok(())
            }
            ClientMessage::Reset => self.handle_reset().await,
            ClientMessage::Run {
                ref query,
                ref parameters,
                ref extra,
            } => self.handle_run(query, parameters, extra).await,
            ClientMessage::Pull { ref extra } => self.handle_pull(extra).await,
            ClientMessage::Discard { ref extra } => self.handle_discard(extra).await,
            ClientMessage::Begin { ref extra } => self.handle_begin(extra).await,
            ClientMessage::Commit => self.handle_commit().await,
            ClientMessage::Rollback => self.handle_rollback().await,
        }
    }

    async fn handle_hello(&mut self, extra: &BoltDict) -> Result<(), BoltError> {
        let user_agent = extra
            .get("user_agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let config = SessionConfig {
            user_agent,
            database: None,
        };

        let session = self.backend.create_session(&config).await?;
        self.session_manager
            .register(session.clone(), self.peer_addr)?;
        self.session = Some(session);

        let mut metadata = self.backend.get_server_info().await.unwrap_or_default();
        metadata
            .entry("connection_id".into())
            .or_insert_with(|| BoltValue::String(uuid::Uuid::new_v4().to_string()));

        // Indicate authentication is required (Bolt 5.1+).
        let hints = BoltDict::new();
        metadata.insert("hints".into(), BoltValue::Dict(hints));

        self.send_message(&ServerMessage::Success { metadata }).await?;
        self.state = self.state.transition_success(&ClientMessage::Hello {
            extra: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_logon(&mut self, auth: &BoltDict) -> Result<(), BoltError> {
        if let Some(ref validator) = self.auth_validator {
            let creds = AuthCredentials {
                scheme: auth
                    .get("scheme")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none")
                    .to_string(),
                principal: auth.get("principal").and_then(|v| v.as_str()).map(String::from),
                credentials: auth
                    .get("credentials")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            };
            validator.validate(&creds).await?;
        }

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self.state.transition_success(&ClientMessage::Logon {
            auth: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_logoff(&mut self) -> Result<(), BoltError> {
        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self.state.transition_success(&ClientMessage::Logoff);
        Ok(())
    }

    async fn handle_reset(&mut self) -> Result<(), BoltError> {
        // Abort any pending transaction.
        if let (Some(session), Some(tx)) = (&self.session, self.transaction.take()) {
            let _ = self.backend.rollback(session, &tx).await;
        }
        self.pending_result = None;

        if let Some(ref session) = self.session {
            self.backend.reset_session(session).await?;
        }

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = ConnectionState::Ready;
        Ok(())
    }

    async fn handle_run(
        &mut self,
        query: &str,
        parameters: &BoltDict,
        extra: &BoltDict,
    ) -> Result<(), BoltError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| BoltError::Session("no active session".into()))?;

        // Switch database if requested.
        if let Some(BoltValue::String(db)) = extra.get("db") {
            self.backend
                .configure_session(session, SessionProperty::Database(db.clone()))
                .await?;
        }

        self.session_manager.touch(&session.0);

        let result = self
            .backend
            .execute(session, query, parameters, extra, self.transaction.as_ref())
            .await?;

        // Buffer results for PULL.
        let columns = result.metadata.columns.clone();
        self.pending_result = Some(PendingResult {
            records: result.records,
            offset: 0,
            columns: columns.clone(),
            summary: result.summary,
        });

        let mut meta = BoltDict::new();
        meta.insert(
            "fields".into(),
            BoltValue::List(columns.into_iter().map(BoltValue::String).collect()),
        );
        meta.insert("t_first".into(), BoltValue::Integer(0));

        self.send_message(&ServerMessage::Success { metadata: meta })
            .await?;

        let transition_msg = ClientMessage::Run {
            query: String::new(),
            parameters: BoltDict::new(),
            extra: BoltDict::new(),
        };
        self.state = self.state.transition_success(&transition_msg);
        Ok(())
    }

    async fn handle_pull(&mut self, extra: &BoltDict) -> Result<(), BoltError> {
        let pending = self
            .pending_result
            .as_ref()
            .ok_or_else(|| BoltError::Protocol("no pending result to pull".into()))?;

        let n = extra.get("n").and_then(|v| v.as_int()).unwrap_or(-1);

        let offset = pending.offset;
        let total = pending.records.len();
        let count = if n == -1 { total - offset } else { n as usize };
        let end = (offset + count).min(total);

        // Collect records to send (avoids borrowing self while sending).
        let records: Vec<Vec<BoltValue>> = pending.records[offset..end]
            .iter()
            .map(|r| r.values.clone())
            .collect();

        // Send RECORD messages.
        for data in records {
            self.send_message(&ServerMessage::Record { data }).await?;
        }

        // Update offset.
        if let Some(ref mut pending) = self.pending_result {
            pending.offset = end;
        }

        let has_more = end < total;
        let mut meta = BoltDict::new();
        meta.insert("has_more".into(), BoltValue::Boolean(has_more));

        if !has_more {
            // Include summary metadata.
            let pending = self.pending_result.take().unwrap();
            meta.extend(pending.summary);
            self.state = self.state.complete_streaming();
        }

        self.send_message(&ServerMessage::Success { metadata: meta })
            .await?;
        Ok(())
    }

    async fn handle_discard(&mut self, _extra: &BoltDict) -> Result<(), BoltError> {
        self.pending_result = None;
        self.state = self.state.complete_streaming();

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::from([("has_more".into(), BoltValue::Boolean(false))]),
        })
        .await?;
        Ok(())
    }

    async fn handle_begin(&mut self, extra: &BoltDict) -> Result<(), BoltError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| BoltError::Session("no active session".into()))?;

        // Switch database if requested.
        if let Some(BoltValue::String(db)) = extra.get("db") {
            self.backend
                .configure_session(session, SessionProperty::Database(db.clone()))
                .await?;
        }

        let tx = self.backend.begin_transaction(session, extra).await?;
        self.transaction = Some(tx);

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self.state.transition_success(&ClientMessage::Begin {
            extra: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_commit(&mut self) -> Result<(), BoltError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| BoltError::Session("no active session".into()))?;
        let tx = self
            .transaction
            .take()
            .ok_or_else(|| BoltError::Transaction("no active transaction".into()))?;

        let metadata = self.backend.commit(session, &tx).await?;

        self.send_message(&ServerMessage::Success { metadata }).await?;
        self.state = self.state.transition_success(&ClientMessage::Commit);
        Ok(())
    }

    async fn handle_rollback(&mut self) -> Result<(), BoltError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| BoltError::Session("no active session".into()))?;
        let tx = self
            .transaction
            .take()
            .ok_or_else(|| BoltError::Transaction("no active transaction".into()))?;

        self.backend.rollback(session, &tx).await?;

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self.state.transition_success(&ClientMessage::Rollback);
        Ok(())
    }

    // -- Helpers --

    async fn send_message(&mut self, msg: &ServerMessage) -> Result<(), BoltError> {
        let mut buf = BytesMut::new();
        encode_server_message(&mut buf, msg);
        self.writer.write_message(&buf).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn send_failure(&mut self, code: &str, message: &str) -> Result<(), BoltError> {
        self.send_message(&ServerMessage::Failure {
            metadata: BoltDict::from([
                ("code".into(), BoltValue::String(code.into())),
                ("message".into(), BoltValue::String(message.into())),
            ]),
        })
        .await
    }

    async fn send_ignored(&mut self) -> Result<(), BoltError> {
        self.send_message(&ServerMessage::Ignored).await
    }
}
