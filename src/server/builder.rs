//! Bolt server builder and TCP listener.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use crate::error::BoltError;
use crate::server::auth::AuthValidator;
use crate::server::backend::BoltBackend;
use crate::server::connection::Connection;
use crate::server::handshake::server_handshake;
use crate::server::session_manager::SessionManager;

/// Builder for configuring and starting a Bolt server.
pub struct BoltServer<B: BoltBackend> {
    backend: B,
    auth_validator: Option<Arc<dyn AuthValidator>>,
    idle_timeout: Option<Duration>,
    max_sessions: Option<usize>,
    shutdown: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl<B: BoltBackend> BoltServer<B> {
    /// Creates a new server builder with the given backend.
    pub fn builder(backend: B) -> Self {
        Self {
            backend,
            auth_validator: None,
            idle_timeout: None,
            max_sessions: None,
            shutdown: None,
        }
    }

    /// Sets an authentication validator.
    pub fn auth(mut self, validator: impl AuthValidator) -> Self {
        self.auth_validator = Some(Arc::new(validator));
        self
    }

    /// Sets the idle session timeout.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Sets the maximum number of concurrent sessions.
    pub fn max_sessions(mut self, limit: usize) -> Self {
        self.max_sessions = Some(limit);
        self
    }

    /// Sets a shutdown signal future.
    pub fn shutdown(mut self, signal: impl Future<Output = ()> + Send + 'static) -> Self {
        self.shutdown = Some(Box::pin(signal));
        self
    }

    /// Starts the Bolt server, listening for TCP connections on `addr`.
    pub async fn serve(self, addr: SocketAddr) -> Result<(), BoltError> {
        let listener = TcpListener::bind(addr).await?;
        let backend = Arc::new(self.backend);
        let session_manager = Arc::new(SessionManager::new(self.max_sessions));
        let auth_validator = self.auth_validator;

        // Idle session reaper.
        let reaper_handle = if let Some(timeout) = self.idle_timeout {
            let sm = session_manager.clone();
            let be = backend.clone();
            let handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(timeout / 2);
                loop {
                    interval.tick().await;
                    let expired = sm.reap_idle(timeout);
                    for id in &expired {
                        let handle = crate::server::SessionHandle(id.clone());
                        let _ = be.close_session(&handle).await;
                        tracing::debug!(session_id = %id, "reaped idle Bolt session");
                    }
                }
            });
            Some(handle)
        } else {
            None
        };

        tracing::info!(%addr, "Bolt server listening");

        // Accept loop.
        let shutdown = self.shutdown;
        let accept_result = if let Some(shutdown_signal) = shutdown {
            tokio::pin!(shutdown_signal);
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                spawn_connection(
                                    stream,
                                    peer_addr,
                                    backend.clone(),
                                    session_manager.clone(),
                                    auth_validator.clone(),
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "accept error");
                            }
                        }
                    }
                    () = &mut shutdown_signal => {
                        tracing::info!("Bolt server shutting down");
                        break;
                    }
                }
            }
            Ok(())
        } else {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        spawn_connection(
                            stream,
                            peer_addr,
                            backend.clone(),
                            session_manager.clone(),
                            auth_validator.clone(),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error");
                    }
                }
            }
        };

        // Stop reaper.
        if let Some(handle) = reaper_handle {
            handle.abort();
        }

        tracing::info!("Bolt server stopped");
        accept_result
    }
}

fn spawn_connection<B: BoltBackend>(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    backend: Arc<B>,
    session_manager: Arc<SessionManager>,
    auth_validator: Option<Arc<dyn AuthValidator>>,
) {
    tokio::spawn(async move {
        let (read_half, write_half) = tokio::io::split(stream);

        // Perform handshake on the raw stream, then split for the connection.
        let mut combined = read_half.unsplit(write_half);
        match server_handshake(&mut combined).await {
            Ok(version) => {
                tracing::debug!(%peer_addr, ?version, "Bolt handshake complete");
                let (rh, wh) = tokio::io::split(combined);
                let mut conn =
                    Connection::new(rh, wh, backend, session_manager, auth_validator, peer_addr);
                if let Err(e) = conn.run().await {
                    tracing::debug!(%peer_addr, error = %e, "Bolt connection closed");
                }
            }
            Err(e) => {
                tracing::debug!(%peer_addr, error = %e, "Bolt handshake failed");
            }
        }
    });
}
