//! Bolt server builder and TCP listener.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use crate::error::BoltError;
use crate::server::auth::AuthValidator;
use crate::server::backend::BoltBackend;
use crate::server::connection::Connection;
use crate::server::handshake::server_handshake;
use crate::server::session_manager::SessionManager;

#[cfg(feature = "tls")]
use rustls_pki_types::pem::PemObject;
#[cfg(feature = "tls")]
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
#[cfg(feature = "tls")]
use tokio_rustls::TlsAcceptor;

/// Default for [`BoltServer::handshake_timeout`]: 30 seconds.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default for [`BoltServer::max_unauthenticated_message_size`]: 64 KiB.
pub const DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE: usize = 64 * 1024;

/// Pause after a failed `accept()` (for example when the process is out of
/// file descriptors) so the accept loop does not spin at full CPU.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// TLS configuration for the Bolt server.
#[cfg(feature = "tls")]
pub struct TlsConfig {
    acceptor: TlsAcceptor,
}

#[cfg(feature = "tls")]
impl TlsConfig {
    /// Creates a TLS configuration from PEM-encoded certificate and key bytes.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, BoltError> {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<_, _>>()
            .map_err(|e| BoltError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

        let key = PrivateKeyDer::from_pem_slice(key_pem)
            .map_err(|e| BoltError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

        let config = tokio_rustls::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| BoltError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }
}

/// Builder for configuring and starting a Bolt server.
///
/// ```rust,no_run
/// use std::net::SocketAddr;
/// use std::time::Duration;
/// use boltr::server::BoltServer;
/// # use boltr::server::BoltBackend;
///
/// # async fn example(my_backend: impl BoltBackend) -> Result<(), boltr::error::BoltError> {
/// let addr: SocketAddr = "0.0.0.0:7687".parse().unwrap();
///
/// BoltServer::builder(my_backend)
///     .idle_timeout(Duration::from_secs(300))
///     .max_sessions(256)
///     .shutdown(async { drop(tokio::signal::ctrl_c().await) })
///     .serve(addr)
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct BoltServer<B: BoltBackend> {
    backend: B,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    idle_timeout: Option<Duration>,
    max_sessions: Option<usize>,
    max_message_size: Option<usize>,
    max_unauthenticated_message_size: usize,
    handshake_timeout: Option<Duration>,
    shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    #[cfg(feature = "tls")]
    tls_config: Option<TlsConfig>,
}

impl<B: BoltBackend> BoltServer<B> {
    /// Creates a new server builder with the given backend.
    pub fn builder(backend: B) -> Self {
        Self {
            backend,
            auth_validator: None,
            idle_timeout: None,
            max_sessions: None,
            max_message_size: None,
            max_unauthenticated_message_size: DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE,
            handshake_timeout: Some(DEFAULT_HANDSHAKE_TIMEOUT),
            shutdown: None,
            #[cfg(feature = "tls")]
            tls_config: None,
        }
    }

    /// Sets an authentication validator.
    pub fn auth(mut self, validator: impl AuthValidator) -> Self {
        self.auth_validator = Some(Arc::new(validator));
        self
    }

    /// Enables TLS with the given configuration.
    #[cfg(feature = "tls")]
    pub fn tls(mut self, config: TlsConfig) -> Self {
        self.tls_config = Some(config);
        self
    }

    /// Sets the idle session timeout.
    ///
    /// A session that receives no message for this long is closed by a
    /// background reaper, and its connection is dropped at its next message.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Sets the maximum number of concurrent sessions.
    pub fn max_sessions(mut self, limit: usize) -> Self {
        self.max_sessions = Some(limit);
        self
    }

    /// Sets the maximum allowed size for a single Bolt message in bytes.
    ///
    /// Messages exceeding this limit are answered with a FAILURE and the
    /// connection is closed. Default: 16 MiB.
    ///
    /// Note that a decoded message can occupy considerably more memory than
    /// its encoded size: every PackStream value becomes a `BoltValue` of
    /// about 170 bytes, so a message of one-byte values can need a few
    /// hundred times its size. Authenticated clients that are not fully
    /// trusted may warrant a lower limit. Before authentication the stricter
    /// [`max_unauthenticated_message_size`](Self::max_unauthenticated_message_size)
    /// applies.
    pub fn max_message_size(mut self, bytes: usize) -> Self {
        self.max_message_size = Some(bytes);
        self
    }

    /// Sets the maximum size of a message accepted before the client has
    /// authenticated (HELLO and LOGON, and again after LOGOFF).
    ///
    /// Decoding can multiply a message's size in memory a few hundred times
    /// (see [`max_message_size`](Self::max_message_size)), so this bounds what
    /// an unauthenticated client can make the server allocate per connection
    /// (a few tens of MB at the default). Larger messages are answered with a
    /// FAILURE and the connection is closed. The effective limit never
    /// exceeds `max_message_size`. Default:
    /// [`DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE`] (64 KiB), enough for
    /// large bearer or Kerberos tokens.
    pub fn max_unauthenticated_message_size(mut self, bytes: usize) -> Self {
        self.max_unauthenticated_message_size = bytes;
        self
    }

    /// Sets how long a new connection may take to authenticate.
    ///
    /// The timeout runs from the moment the TCP connection is accepted and
    /// covers the TLS and WebSocket handshakes (when enabled), the Bolt
    /// version handshake, HELLO and LOGON. A connection that has not
    /// completed LOGON in time is closed, so clients cannot hold connections
    /// open without authenticating. After a LOGOFF, the connection again has
    /// this long to LOGON. Default: [`DEFAULT_HANDSHAKE_TIMEOUT`] (30 s).
    /// `Duration::ZERO` disables the timeout.
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = (!timeout.is_zero()).then_some(timeout);
        self
    }

    /// Sets a shutdown signal future.
    ///
    /// When the signal completes the server stops accepting new connections
    /// and `serve` returns. Connections that are already open keep running
    /// until their clients disconnect.
    pub fn shutdown(mut self, signal: impl Future<Output = ()> + Send + 'static) -> Self {
        self.shutdown = Some(Box::pin(signal));
        self
    }

    /// Starts the Bolt server, listening for TCP connections on `addr`.
    pub async fn serve(self, addr: SocketAddr) -> Result<(), BoltError> {
        let listener = TcpListener::bind(addr).await?;
        self.serve_listener(listener).await
    }

    /// Starts the Bolt server on an already bound TCP listener.
    ///
    /// Useful to bind port 0 and read the chosen port from
    /// [`TcpListener::local_addr`] before serving, without the race of
    /// binding, dropping and re-binding the same address.
    pub async fn serve_listener(self, listener: TcpListener) -> Result<(), BoltError> {
        self.accept_loop(listener, Transport::Tcp).await
    }

    /// Starts the Bolt server, listening for WebSocket connections on `addr`.
    ///
    /// Each incoming TCP connection is upgraded to WebSocket via the HTTP
    /// upgrade handshake, then the Bolt protocol runs over the WebSocket
    /// connection.
    ///
    /// When the `tls` feature is also enabled, connections are TLS-wrapped
    /// before the WebSocket upgrade (WSS).
    #[cfg(feature = "ws")]
    pub async fn ws_serve(self, addr: SocketAddr) -> Result<(), BoltError> {
        let listener = TcpListener::bind(addr).await?;
        self.ws_serve_listener(listener).await
    }

    /// Like [`ws_serve`](Self::ws_serve), on an already bound TCP listener.
    #[cfg(feature = "ws")]
    pub async fn ws_serve_listener(self, listener: TcpListener) -> Result<(), BoltError> {
        self.accept_loop(listener, Transport::WebSocket).await
    }

    async fn accept_loop(
        self,
        listener: TcpListener,
        transport: Transport,
    ) -> Result<(), BoltError> {
        let addr = listener.local_addr()?;
        let backend = Arc::new(self.backend);
        let session_manager = Arc::new(SessionManager::new(self.max_sessions));
        let context = ConnectionContext {
            backend: Arc::clone(&backend),
            session_manager: Arc::clone(&session_manager),
            auth_validator: self.auth_validator,
            max_message_size: self.max_message_size,
            max_unauthenticated_message_size: self.max_unauthenticated_message_size,
            handshake_timeout: self.handshake_timeout,
        };

        #[cfg(feature = "tls")]
        let tls_acceptor = self.tls_config.map(|c| c.acceptor);
        #[cfg(feature = "tls")]
        let secure = tls_acceptor.is_some();
        #[cfg(not(feature = "tls"))]
        let secure = false;

        let reaper_handle = self
            .idle_timeout
            .map(|timeout| spawn_idle_reaper(timeout, session_manager, backend));

        let label = match (transport, secure) {
            (Transport::Tcp, false) => "",
            (Transport::Tcp, true) => " (TLS)",
            #[cfg(feature = "ws")]
            (Transport::WebSocket, false) => " (WebSocket)",
            #[cfg(feature = "ws")]
            (Transport::WebSocket, true) => " (WebSocket, WSS)",
        };
        tracing::info!(%addr, "Bolt server listening{}", label);

        let mut shutdown = self.shutdown;
        loop {
            let accepted = if let Some(signal) = shutdown.as_mut() {
                tokio::select! {
                    result = listener.accept() => result,
                    () = signal => {
                        tracing::info!("Bolt server shutting down");
                        break;
                    }
                }
            } else {
                listener.accept().await
            };

            match accepted {
                Ok((stream, peer_addr)) => spawn_connection(
                    stream,
                    peer_addr,
                    context.clone(),
                    transport,
                    #[cfg(feature = "tls")]
                    tls_acceptor.clone(),
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "accept error");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                }
            }
        }

        if let Some(handle) = reaper_handle {
            handle.abort();
        }

        tracing::info!("Bolt server stopped");
        Ok(())
    }
}

/// The transport a listener serves Bolt over.
#[derive(Clone, Copy)]
enum Transport {
    Tcp,
    #[cfg(feature = "ws")]
    WebSocket,
}

/// Everything a connection task needs, shared by all connections of a server.
pub(crate) struct ConnectionContext<B: BoltBackend> {
    pub(crate) backend: Arc<B>,
    pub(crate) session_manager: Arc<SessionManager>,
    pub(crate) auth_validator: Option<Arc<dyn AuthValidator>>,
    pub(crate) max_message_size: Option<usize>,
    pub(crate) max_unauthenticated_message_size: usize,
    pub(crate) handshake_timeout: Option<Duration>,
}

impl<B: BoltBackend> Clone for ConnectionContext<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            session_manager: Arc::clone(&self.session_manager),
            auth_validator: self.auth_validator.clone(),
            max_message_size: self.max_message_size,
            max_unauthenticated_message_size: self.max_unauthenticated_message_size,
            handshake_timeout: self.handshake_timeout,
        }
    }
}

/// Periodically closes sessions that have been idle longer than `timeout`.
fn spawn_idle_reaper<B: BoltBackend>(
    timeout: Duration,
    session_manager: Arc<SessionManager>,
    backend: Arc<B>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // `tokio::time::interval` panics on a zero period.
        let period = (timeout / 2).max(Duration::from_millis(1));
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            let expired = session_manager.reap_idle(timeout);
            for id in &expired {
                let handle = crate::server::SessionHandle(id.clone());
                let _ = backend.close_session(&handle).await;
                tracing::debug!(session_id = %id, "reaped idle Bolt session");
            }
        }
    })
}

/// Awaits `future`, giving up (returning `None`) at `deadline` if one is set.
async fn with_deadline<F: Future>(deadline: Option<Instant>, future: F) -> Option<F::Output> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future).await.ok(),
        None => Some(future.await),
    }
}

fn spawn_connection<B: BoltBackend>(
    stream: TcpStream,
    peer_addr: SocketAddr,
    context: ConnectionContext<B>,
    transport: Transport,
    #[cfg(feature = "tls")] tls_acceptor: Option<TlsAcceptor>,
) {
    tokio::spawn(async move {
        let started = Instant::now();

        #[cfg(feature = "tls")]
        if let Some(acceptor) = tls_acceptor {
            let deadline = context.handshake_timeout.map(|t| started + t);
            match with_deadline(deadline, acceptor.accept(stream)).await {
                Some(Ok(tls_stream)) => {
                    serve_transport(tls_stream, peer_addr, context, transport, started).await;
                }
                Some(Err(e)) => {
                    tracing::debug!(%peer_addr, error = %e, "TLS handshake failed");
                }
                None => tracing::debug!(%peer_addr, "TLS handshake timed out"),
            }
            return;
        }

        serve_transport(stream, peer_addr, context, transport, started).await;
    });
}

/// Runs the transport-specific upgrade (if any), then the Bolt connection.
async fn serve_transport<S, B>(
    stream: S,
    peer_addr: SocketAddr,
    context: ConnectionContext<B>,
    transport: Transport,
    started: Instant,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    B: BoltBackend,
{
    match transport {
        Transport::Tcp => run_handshake_and_connection(stream, peer_addr, context, started).await,
        #[cfg(feature = "ws")]
        Transport::WebSocket => {
            let deadline = context.handshake_timeout.map(|t| started + t);
            match with_deadline(deadline, tokio_tungstenite::accept_async(stream)).await {
                Some(Ok(ws_stream)) => {
                    let adapted = crate::ws::WsStream::new(ws_stream);
                    run_handshake_and_connection(adapted, peer_addr, context, started).await;
                }
                Some(Err(e)) => {
                    tracing::debug!(%peer_addr, error = %e, "WebSocket upgrade failed");
                }
                None => tracing::debug!(%peer_addr, "WebSocket upgrade timed out"),
            }
        }
    }
}

/// Performs the Bolt handshake, then runs the connection until it closes.
///
/// `started` is when the connection was accepted: the handshake timeout of
/// `context` (if any) is measured from it.
pub(crate) async fn run_handshake_and_connection<S, B>(
    mut stream: S,
    peer_addr: SocketAddr,
    context: ConnectionContext<B>,
    started: Instant,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    B: BoltBackend,
{
    let deadline = context.handshake_timeout.map(|t| started + t);
    let version = match with_deadline(deadline, server_handshake(&mut stream)).await {
        Some(Ok(version)) => version,
        Some(Err(e)) => {
            tracing::debug!(%peer_addr, error = %e, "Bolt handshake failed");
            return;
        }
        None => {
            tracing::debug!(%peer_addr, "Bolt handshake timed out");
            return;
        }
    };
    tracing::debug!(%peer_addr, ?version, "Bolt handshake complete");

    let (read_half, write_half) = tokio::io::split(stream);
    let mut conn = Connection::new(
        read_half,
        write_half,
        context.backend,
        context.session_manager,
        context.auth_validator,
        peer_addr,
        context.max_message_size,
    );
    conn.set_max_unauthenticated_message_size(context.max_unauthenticated_message_size);
    if let (Some(timeout), Some(deadline)) = (context.handshake_timeout, deadline) {
        conn.set_auth_deadline(timeout, deadline);
    }
    if let Err(e) = conn.run().await {
        tracing::debug!(%peer_addr, error = %e, "Bolt connection closed");
    }
}
