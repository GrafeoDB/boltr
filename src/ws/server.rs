//! Server-side WebSocket support for accepting Bolt-over-WS connections.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;

use crate::error::BoltError;
use crate::server::auth::AuthValidator;
use crate::server::backend::BoltBackend;
use crate::server::builder::run_handshake_and_connection;
use crate::server::session_manager::SessionManager;
use crate::ws::WsStream;

/// Accepts a pre-upgraded WebSocket connection and runs the Bolt protocol on it.
///
/// Use this when your HTTP server (e.g., Axum) has already performed the
/// WebSocket upgrade. Pass the resulting [`WebSocketStream`] here and BoltR
/// handles the Bolt handshake, authentication, and message processing.
///
/// The connection is spawned on the Tokio runtime and this function returns
/// immediately.
///
/// # Example
///
/// ```rust,no_run
/// use std::net::SocketAddr;
/// use std::sync::Arc;
/// use boltr::server::{BoltBackend, SessionManager};
///
/// # async fn example<B: BoltBackend>(
/// #     ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
/// #     backend: Arc<B>,
/// # ) {
/// let session_manager = Arc::new(SessionManager::new(Some(256)));
///
/// boltr::ws::server::accept_ws(
///     ws_stream,
///     "127.0.0.1:7687".parse().unwrap(),
///     backend,
///     session_manager,
///     None,
///     None,
/// );
/// # }
/// ```
pub fn accept_ws<S, B>(
    ws_stream: WebSocketStream<S>,
    peer_addr: SocketAddr,
    backend: Arc<B>,
    session_manager: Arc<SessionManager>,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    max_message_size: Option<usize>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    B: BoltBackend,
{
    let adapted = WsStream::new(ws_stream);
    tokio::spawn(async move {
        run_handshake_and_connection(
            adapted,
            peer_addr,
            backend,
            session_manager,
            auth_validator,
            max_message_size,
        )
        .await;
    });
}

/// Accepts a pre-upgraded WebSocket connection and runs the Bolt protocol,
/// returning only when the connection is closed.
///
/// Unlike [`accept_ws`], this does not spawn a task: the caller controls
/// the execution context.
pub async fn handle_ws<S, B>(
    ws_stream: WebSocketStream<S>,
    peer_addr: SocketAddr,
    backend: Arc<B>,
    session_manager: Arc<SessionManager>,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    max_message_size: Option<usize>,
) -> Result<(), BoltError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    B: BoltBackend,
{
    let adapted = WsStream::new(ws_stream);
    run_handshake_and_connection(
        adapted,
        peer_addr,
        backend,
        session_manager,
        auth_validator,
        max_message_size,
    )
    .await;
    Ok(())
}
