//! Per-TCP-connection Bolt handler.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

use crate::chunk::reader::DEFAULT_MAX_MESSAGE_SIZE;
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
use crate::server::builder::DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE;
use crate::server::session_manager::SessionManager;
use crate::server::state_machine::ConnectionState;
use crate::types::{BoltDict, BoltValue};

/// How long a closing connection keeps draining client input (see
/// `Connection::close_gracefully`).
const CLOSE_LINGER: Duration = Duration::from_secs(1);

/// Maximum number of results a transaction may keep open at once. Each one
/// buffers its records until it is pulled or discarded.
const MAX_OPEN_RESULTS: usize = 1024;

/// Buffered query results waiting for PULL/DISCARD.
struct PendingResult {
    records: Vec<BoltRecord>,
    offset: usize,
    summary: BoltDict,
}

/// Results of RUN statements that have not been fully pulled or discarded.
///
/// Inside an explicit transaction several results can be open at once
/// (Bolt 4.0+): every RUN gets a query id (`qid`, reported in its SUCCESS),
/// and PULL or DISCARD select a result with `{"qid": n}`, where `-1` (the
/// default) means the most recent RUN. In auto-commit mode at most one result
/// is open.
#[derive(Default)]
struct OpenResults {
    /// Open results keyed by qid.
    results: BTreeMap<i64, PendingResult>,
    /// qid of the most recent RUN, which `qid: -1` refers to.
    last_qid: Option<i64>,
    /// qid of the next RUN in the current transaction.
    next_qid: i64,
}

impl OpenResults {
    /// Forgets every open result and restarts qids at 0.
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    fn ensure_capacity(&self) -> Result<(), BoltError> {
        if self.results.len() >= MAX_OPEN_RESULTS {
            return Err(BoltError::Protocol(format!(
                "too many open results in this transaction (limit {MAX_OPEN_RESULTS}); \
                 pull or discard some first"
            )));
        }
        Ok(())
    }

    /// Registers the result of a RUN and returns its qid.
    fn open(&mut self, result: PendingResult) -> i64 {
        let qid = self.next_qid;
        self.next_qid = self.next_qid.saturating_add(1);
        self.results.insert(qid, result);
        self.last_qid = Some(qid);
        qid
    }

    /// Finds the result a PULL or DISCARD refers to.
    fn select(
        &mut self,
        extra: &BoltDict,
        message: &str,
    ) -> Result<(i64, &mut PendingResult), BoltError> {
        let qid = match extra.get("qid") {
            None | Some(BoltValue::Integer(-1)) => self
                .last_qid
                .filter(|qid| self.results.contains_key(qid))
                .ok_or_else(|| {
                    BoltError::Protocol(format!(
                        "no pending result to {}",
                        message.to_ascii_lowercase()
                    ))
                })?,
            Some(BoltValue::Integer(qid)) if *qid >= 0 => *qid,
            Some(BoltValue::Integer(qid)) => {
                return Err(BoltError::Protocol(format!(
                    "invalid {message} qid: {qid}, must be -1 or a query id"
                )));
            }
            Some(other) => {
                return Err(BoltError::Protocol(format!(
                    "{message} qid must be an Integer, got {}",
                    other.type_name()
                )));
            }
        };
        let result = self
            .results
            .get_mut(&qid)
            .ok_or_else(|| BoltError::Protocol(format!("no open result with qid {qid}")))?;
        Ok((qid, result))
    }
}

/// Handles a single Bolt TCP connection.
pub struct Connection<R, W, B: BoltBackend> {
    reader: ChunkReader<R>,
    writer: ChunkWriter<W>,
    backend: Arc<B>,
    session_manager: Arc<SessionManager>,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    state: ConnectionState,
    /// Set only by a successful LOGON and cleared by LOGOFF. Checked
    /// independently of `state` before any message that needs an
    /// authenticated connection is dispatched.
    authenticated: bool,
    /// How long an unauthenticated connection may stay open.
    auth_timeout: Option<Duration>,
    /// When the current unauthenticated phase must end (see `auth_timeout`).
    auth_deadline: Option<Instant>,
    session: Option<SessionHandle>,
    transaction: Option<TransactionHandle>,
    results: OpenResults,
    peer_addr: SocketAddr,
    /// Set once reading failed (the peer closed or the stream broke).
    peer_gone: bool,
    /// Message size limit once authenticated.
    max_message_size: usize,
    /// Message size limit before authentication (HELLO, LOGON). A decoded
    /// message can take far more memory than its encoded size, so this keeps
    /// what an unauthenticated client can make the server allocate small.
    max_unauthenticated_message_size: usize,
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
        max_message_size: Option<usize>,
    ) -> Self {
        let max_message_size = max_message_size.unwrap_or(DEFAULT_MAX_MESSAGE_SIZE);
        let max_unauthenticated_message_size = DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE;
        let mut chunk_reader = ChunkReader::new(reader);
        chunk_reader.set_max_message_size(max_unauthenticated_message_size.min(max_message_size));
        Self {
            reader: chunk_reader,
            writer: ChunkWriter::new(writer),
            backend,
            session_manager,
            auth_validator,
            state: ConnectionState::Negotiation,
            authenticated: false,
            auth_timeout: None,
            auth_deadline: None,
            session: None,
            transaction: None,
            results: OpenResults::default(),
            peer_addr,
            peer_gone: false,
            max_message_size,
            max_unauthenticated_message_size,
        }
    }

    /// Sets the message size limit that applies before authentication
    /// (capped by the general maximum message size).
    pub(crate) fn set_max_unauthenticated_message_size(&mut self, bytes: usize) {
        self.max_unauthenticated_message_size = bytes;
        self.apply_message_size_limit();
    }

    /// Applies the message size limit for the current authentication state.
    fn apply_message_size_limit(&mut self) {
        let limit = if self.authenticated {
            self.max_message_size
        } else {
            self.max_unauthenticated_message_size
                .min(self.max_message_size)
        };
        self.reader.set_max_message_size(limit);
    }

    /// Closes the connection if it has not authenticated by `deadline`, and
    /// again if it stays unauthenticated for `timeout` after a LOGOFF.
    pub(crate) fn set_auth_deadline(&mut self, timeout: Duration, deadline: Instant) {
        self.auth_timeout = Some(timeout);
        self.auth_deadline = Some(deadline);
    }

    /// Runs the connection lifecycle: message loop, then cleanup.
    ///
    /// The Bolt handshake is performed before constructing the `Connection`,
    /// so it starts in the `Negotiation` state waiting for HELLO. Cleanup
    /// (rolling back an open transaction and closing the backend session)
    /// runs however the loop ends, including on write errors.
    pub async fn run(&mut self) -> Result<(), BoltError> {
        let result = self.message_loop().await;
        if result.is_err() {
            // A write failed: the peer is unreachable.
            self.peer_gone = true;
        }
        self.cleanup().await;
        self.close_gracefully().await;
        result
    }

    /// Ends a connection the server decided to close (GOODBYE, a fatal
    /// FAILURE, an oversized message, a timeout) without losing the last
    /// response.
    ///
    /// Closing a socket that still has unread input makes the operating
    /// system send a reset, which can destroy a FAILURE the client has not
    /// read yet. So the write side is shut down first (the client sees the
    /// end of the stream after the FAILURE) and pending input is drained for
    /// up to [`CLOSE_LINGER`].
    async fn close_gracefully(&mut self) {
        if self.peer_gone {
            return;
        }
        if self.writer.get_mut().shutdown().await.is_err() {
            return;
        }
        let reader = self.reader.get_mut();
        let drain = async {
            let mut sink = [0u8; 4096];
            while let Ok(read) = reader.read(&mut sink).await {
                if read == 0 {
                    break;
                }
            }
        };
        let _ = tokio::time::timeout(CLOSE_LINGER, drain).await;
    }

    async fn message_loop(&mut self) -> Result<(), BoltError> {
        while self.state != ConnectionState::Defunct {
            let Some(msg_bytes) = self.next_message().await else {
                break;
            };

            if let Some(ref session) = self.session {
                // Detect if idle reaper closed our session.
                if !self.session_manager.contains(&session.0) {
                    tracing::debug!(%self.peer_addr, "session reaped by idle timeout");
                    self.session = None;
                    self.transaction = None;
                    break;
                }
                // Any message, not only RUN, counts as activity.
                self.session_manager.touch(&session.0);
            }

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
                    self.state = self.state.after_protocol_violation();
                    continue;
                }
            };

            if !self.state.accepts(&msg) {
                tracing::debug!(
                    %self.peer_addr,
                    state = %self.state,
                    msg = message_name(&msg),
                    "message not allowed in current state",
                );
                if matches!(msg, ClientMessage::Goodbye) {
                    self.state = ConnectionState::Defunct;
                    break;
                }
                if self.state == ConnectionState::Failed {
                    // Spec: in FAILED everything but RESET and GOODBYE is IGNORED.
                    self.send_ignored().await?;
                } else {
                    let message = format!(
                        "message {} cannot be handled in state {}",
                        message_name(&msg),
                        self.state
                    );
                    self.send_failure("Neo.ClientError.Request.Invalid", &message)
                        .await?;
                    self.state = self.state.after_protocol_violation();
                }
                continue;
            }

            if !self.authenticated && requires_authentication(&msg) {
                // Unreachable through the state machine; fail closed anyway.
                tracing::warn!(
                    %self.peer_addr,
                    msg = message_name(&msg),
                    "message before authentication",
                );
                self.send_failure(
                    "Neo.ClientError.Security.Unauthorized",
                    "authentication required",
                )
                .await?;
                self.state = ConnectionState::Defunct;
                break;
            }

            if let Err(e) = self.handle_message(&msg).await {
                tracing::debug!(%self.peer_addr, error = %e, "handler error");
                let meta = e.to_failure_metadata();
                self.send_message(&ServerMessage::Failure { metadata: meta })
                    .await?;
                self.state = self.state.transition_failure(&msg);
            }

            if let Some(ref session) = self.session {
                self.session_manager.touch(&session.0);
            }
        }
        Ok(())
    }

    /// Reads the next message, or returns `None` when the connection should
    /// close (peer closed, I/O error, framing violation or authentication
    /// timeout).
    async fn next_message(&mut self) -> Option<BytesMut> {
        let read = match (self.authenticated, self.auth_deadline) {
            (false, Some(deadline)) => {
                if let Ok(result) =
                    tokio::time::timeout_at(deadline, self.reader.read_message()).await
                {
                    result
                } else {
                    tracing::debug!(%self.peer_addr, "authentication timed out");
                    return None;
                }
            }
            _ => self.reader.read_message().await,
        };

        match read {
            Ok(bytes) => Some(bytes),
            Err(BoltError::Io(e)) => {
                tracing::debug!(%self.peer_addr, error = %e, "read error");
                self.peer_gone = true;
                None
            }
            Err(e) => {
                // Framing violation (for example an oversized message). The
                // stream position is unknown now, so report and close.
                tracing::warn!(%self.peer_addr, error = %e, "framing error");
                let _ = self
                    .send_failure("Neo.ClientError.Request.Invalid", &e.to_string())
                    .await;
                None
            }
        }
    }

    /// Rolls back any open transaction and closes the backend session,
    /// unless the idle reaper already did.
    async fn cleanup(&mut self) {
        self.results.clear();
        let transaction = self.transaction.take();
        let Some(session) = self.session.take() else {
            return;
        };
        if !self.session_manager.remove_if_present(&session.0) {
            // Reaped while this connection was waiting: already closed.
            return;
        }
        if let Some(tx) = transaction
            && let Err(e) = self.backend.rollback(&session, &tx).await
        {
            tracing::debug!(%self.peer_addr, error = %e, "rollback on disconnect failed");
        }
        if let Err(e) = self.backend.close_session(&session).await {
            tracing::debug!(%self.peer_addr, error = %e, "close_session failed");
        }
    }

    async fn handle_message(&mut self, msg: &ClientMessage) -> Result<(), BoltError> {
        match msg {
            ClientMessage::Hello { extra } => self.handle_hello(extra).await,
            ClientMessage::Logon { auth } => self.handle_logon(auth).await,
            ClientMessage::Logoff => self.handle_logoff().await,
            ClientMessage::Goodbye => {
                self.state = ConnectionState::Defunct;
                Ok(())
            }
            ClientMessage::Reset => self.handle_reset().await,
            ClientMessage::Run {
                query,
                parameters,
                extra,
            } => self.handle_run(query, parameters, extra).await,
            ClientMessage::Pull { extra } => self.handle_pull(extra).await,
            ClientMessage::Discard { extra } => self.handle_discard(extra).await,
            ClientMessage::Begin { extra } => self.handle_begin(extra).await,
            ClientMessage::Commit => self.handle_commit().await,
            ClientMessage::Rollback => self.handle_rollback().await,
            ClientMessage::Route {
                routing,
                bookmarks,
                extra,
            } => self.handle_route(routing, bookmarks, extra).await,
            ClientMessage::Telemetry { .. } => self.handle_telemetry().await,
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
        if let Err(e) = self
            .session_manager
            .register(session.clone(), self.peer_addr)
        {
            // Do not leak the backend session when the session limit is hit.
            let _ = self.backend.close_session(&session).await;
            return Err(e);
        }
        self.session = Some(session);

        let mut metadata = self.backend.get_server_info().await.unwrap_or_default();
        metadata
            .entry("connection_id".into())
            .or_insert_with(|| BoltValue::String(uuid::Uuid::new_v4().to_string()));
        // Connection hints (Bolt 5.1+); the backend may provide its own.
        metadata
            .entry("hints".into())
            .or_insert_with(|| BoltValue::Dict(BoltDict::new()));

        self.send_message(&ServerMessage::Success { metadata })
            .await?;
        self.state = self.state.transition_success(&ClientMessage::Hello {
            extra: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_logon(&mut self, auth: &BoltDict) -> Result<(), BoltError> {
        let auth_info = if let Some(ref validator) = self.auth_validator {
            let creds = AuthCredentials {
                scheme: auth
                    .get("scheme")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none")
                    .to_string(),
                principal: auth
                    .get("principal")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                credentials: auth
                    .get("credentials")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            };
            Some(validator.validate(&creds).await?)
        } else {
            None
        };

        let credentials_expired = auth_info
            .as_ref()
            .is_some_and(|info| info.credentials_expired);

        if let (Some(session), Some(info)) = (&self.session, auth_info) {
            self.backend.set_session_auth(session, info).await?;
        }

        let mut metadata = BoltDict::new();
        if credentials_expired {
            metadata.insert("credentials_expired".into(), BoltValue::Boolean(true));
        }

        self.send_message(&ServerMessage::Success { metadata })
            .await?;
        self.authenticated = true;
        self.apply_message_size_limit();
        self.state = self.state.transition_success(&ClientMessage::Logon {
            auth: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_logoff(&mut self) -> Result<(), BoltError> {
        // Clear any in-flight state: abort pending transaction, discard results.
        if let (Some(session), Some(tx)) = (&self.session, self.transaction.take()) {
            let _ = self.backend.rollback(session, &tx).await;
        }
        self.results.clear();

        // Notify the backend that the session is de-authenticated.
        if let Some(ref session) = self.session {
            self.backend.reset_session(session).await?;
        }

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.authenticated = false;
        self.apply_message_size_limit();
        // A new LOGON must arrive within the authentication timeout.
        self.auth_deadline = self.auth_timeout.map(|timeout| Instant::now() + timeout);
        self.state = self.state.transition_success(&ClientMessage::Logoff);
        Ok(())
    }

    async fn handle_reset(&mut self) -> Result<(), BoltError> {
        // Abort any pending transaction.
        if let (Some(session), Some(tx)) = (&self.session, self.transaction.take()) {
            let _ = self.backend.rollback(session, &tx).await;
        }
        self.results.clear();

        if let Some(ref session) = self.session {
            self.backend.reset_session(session).await?;
        }

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        // RESET never authenticates: without a completed LOGON there is no
        // READY state to return to.
        self.state = if self.authenticated {
            ConnectionState::Ready
        } else {
            ConnectionState::Defunct
        };
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

        let in_transaction = self.transaction.is_some();
        if in_transaction {
            self.results.ensure_capacity()?;
        }

        let result = self
            .backend
            .execute(session, query, parameters, extra, self.transaction.as_ref())
            .await?;

        let mut meta = BoltDict::new();
        meta.insert(
            "fields".into(),
            BoltValue::List(
                result
                    .metadata
                    .columns
                    .into_iter()
                    .map(BoltValue::String)
                    .collect(),
            ),
        );
        meta.insert("t_first".into(), BoltValue::Integer(0));

        // Buffer results for PULL. Auto-commit mode has a single result.
        if !in_transaction {
            self.results.clear();
        }
        let qid = self.results.open(PendingResult {
            records: result.records,
            offset: 0,
            summary: result.summary,
        });
        if in_transaction {
            // Lets the client PULL or DISCARD this result later by qid.
            meta.insert("qid".into(), BoltValue::Integer(qid));
        }

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
        let n = requested_count(extra, "PULL")?;
        let (qid, pending) = self.results.select(extra, "PULL")?;

        let total = pending.records.len();
        let start = pending.offset.min(total);
        let end = n.map_or(total, |n| start.saturating_add(n).min(total));
        pending.offset = end;

        // Each record is sent exactly once, so move it out instead of cloning.
        let batch: Vec<Vec<BoltValue>> = pending.records[start..end]
            .iter_mut()
            .map(|record| std::mem::take(&mut record.values))
            .collect();

        for data in batch {
            self.send_message(&ServerMessage::Record { data }).await?;
        }

        self.finish_stream_batch(qid, end < total).await
    }

    async fn handle_discard(&mut self, extra: &BoltDict) -> Result<(), BoltError> {
        let n = requested_count(extra, "DISCARD")?;
        let (qid, pending) = self.results.select(extra, "DISCARD")?;

        let total = pending.records.len();
        let start = pending.offset.min(total);
        let end = n.map_or(total, |n| start.saturating_add(n).min(total));
        pending.offset = end;
        for record in &mut pending.records[start..end] {
            record.values = Vec::new();
        }

        self.finish_stream_batch(qid, end < total).await
    }

    /// Sends the SUCCESS that ends a PULL or DISCARD batch. When the result
    /// is exhausted it carries the result summary (bookmark, stats, ...), and
    /// once no result is open the connection leaves the streaming state.
    async fn finish_stream_batch(&mut self, qid: i64, has_more: bool) -> Result<(), BoltError> {
        let mut meta = BoltDict::new();
        meta.insert("has_more".into(), BoltValue::Boolean(has_more));
        if !has_more && let Some(pending) = self.results.results.remove(&qid) {
            meta.extend(pending.summary);
        }

        self.send_message(&ServerMessage::Success { metadata: meta })
            .await?;
        if self.results.is_empty() {
            self.state = self.state.complete_streaming();
        }
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
        self.results.clear();

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

        self.send_message(&ServerMessage::Success { metadata })
            .await?;
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

        // ROLLBACK may arrive while results are still open.
        self.results.clear();
        self.backend.rollback(session, &tx).await?;

        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self.state.transition_success(&ClientMessage::Rollback);
        Ok(())
    }

    async fn handle_route(
        &mut self,
        routing: &BoltDict,
        bookmarks: &[String],
        extra: &BoltDict,
    ) -> Result<(), BoltError> {
        let db = extra.get("db").and_then(|v| v.as_str());

        let table = self.backend.route(routing, bookmarks, db).await?;

        let servers: Vec<BoltValue> = table
            .servers
            .iter()
            .map(|s| {
                BoltValue::Dict(BoltDict::from([
                    (
                        "addresses".into(),
                        BoltValue::List(
                            s.addresses
                                .iter()
                                .map(|a| BoltValue::String(a.clone()))
                                .collect(),
                        ),
                    ),
                    ("role".into(), BoltValue::String(s.role.clone())),
                ]))
            })
            .collect();

        let rt = BoltDict::from([
            ("ttl".into(), BoltValue::Integer(table.ttl)),
            ("db".into(), BoltValue::String(table.db)),
            ("servers".into(), BoltValue::List(servers)),
        ]);

        let metadata = BoltDict::from([("rt".into(), BoltValue::Dict(rt))]);

        self.send_message(&ServerMessage::Success { metadata })
            .await?;
        self.state = self.state.transition_success(&ClientMessage::Route {
            routing: BoltDict::new(),
            bookmarks: vec![],
            extra: BoltDict::new(),
        });
        Ok(())
    }

    async fn handle_telemetry(&mut self) -> Result<(), BoltError> {
        // Telemetry is a no-op acknowledgment (Bolt 5.4+).
        self.send_message(&ServerMessage::Success {
            metadata: BoltDict::new(),
        })
        .await?;
        self.state = self
            .state
            .transition_success(&ClientMessage::Telemetry { api: 0 });
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

/// Parses the `n` entry of a PULL or DISCARD extra dictionary.
///
/// Returns `None` for "all remaining" (`-1`, or no `n` at all) and the
/// requested batch size otherwise. Zero, other negative values and
/// non-integers are rejected, as in the Bolt specification.
fn requested_count(extra: &BoltDict, message: &str) -> Result<Option<usize>, BoltError> {
    match extra.get("n") {
        None | Some(BoltValue::Integer(-1)) => Ok(None),
        Some(BoltValue::Integer(n)) if *n > 0 => {
            Ok(Some(usize::try_from(*n).unwrap_or(usize::MAX)))
        }
        Some(BoltValue::Integer(n)) => Err(BoltError::Protocol(format!(
            "invalid {message} n value: {n}, must be -1 or positive"
        ))),
        Some(other) => Err(BoltError::Protocol(format!(
            "{message} n must be an Integer, got {}",
            other.type_name()
        ))),
    }
}

/// Whether a message may only be processed on an authenticated connection.
fn requires_authentication(msg: &ClientMessage) -> bool {
    !matches!(
        msg,
        ClientMessage::Hello { .. } | ClientMessage::Logon { .. } | ClientMessage::Goodbye
    )
}

/// The message name without its payload (unlike `Display`, which includes
/// the query text of a RUN).
fn message_name(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::Hello { .. } => "HELLO",
        ClientMessage::Logon { .. } => "LOGON",
        ClientMessage::Logoff => "LOGOFF",
        ClientMessage::Goodbye => "GOODBYE",
        ClientMessage::Reset => "RESET",
        ClientMessage::Run { .. } => "RUN",
        ClientMessage::Pull { .. } => "PULL",
        ClientMessage::Discard { .. } => "DISCARD",
        ClientMessage::Begin { .. } => "BEGIN",
        ClientMessage::Commit => "COMMIT",
        ClientMessage::Rollback => "ROLLBACK",
        ClientMessage::Route { .. } => "ROUTE",
        ClientMessage::Telemetry { .. } => "TELEMETRY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extra_n(n: BoltValue) -> BoltDict {
        BoltDict::from([("n".to_string(), n)])
    }

    #[test]
    fn requested_count_accepts_all_and_positive() {
        assert_eq!(requested_count(&BoltDict::new(), "PULL").unwrap(), None);
        assert_eq!(
            requested_count(&extra_n(BoltValue::Integer(-1)), "PULL").unwrap(),
            None
        );
        assert_eq!(
            requested_count(&extra_n(BoltValue::Integer(1)), "PULL").unwrap(),
            Some(1)
        );
        assert_eq!(
            requested_count(&extra_n(BoltValue::Integer(i64::MAX)), "PULL").unwrap(),
            Some(usize::try_from(i64::MAX).unwrap_or(usize::MAX))
        );
    }

    #[test]
    fn requested_count_rejects_zero_negative_and_non_integers() {
        for bad in [
            BoltValue::Integer(0),
            BoltValue::Integer(-2),
            BoltValue::Integer(i64::MIN),
            BoltValue::String("10".into()),
            BoltValue::Float(1.0),
            BoltValue::Null,
        ] {
            assert!(
                requested_count(&extra_n(bad.clone()), "DISCARD").is_err(),
                "{bad:?}"
            );
        }
    }
}
