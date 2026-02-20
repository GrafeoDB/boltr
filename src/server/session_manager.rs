//! Bolt session tracking and idle reaping.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::error::BoltError;
use crate::server::SessionHandle;

/// Tracked state for a single Bolt session.
pub struct SessionState {
    pub handle: SessionHandle,
    pub peer_addr: SocketAddr,
    pub created_at: Instant,
    pub last_active: Instant,
}

/// Manages active Bolt sessions: capacity limits and idle reaping.
pub struct SessionManager {
    sessions: RwLock<HashMap<String, SessionState>>,
    max_sessions: Option<usize>,
}

impl SessionManager {
    pub fn new(max_sessions: Option<usize>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_sessions,
        }
    }

    /// Registers a new session. Fails if the capacity limit is reached.
    pub fn register(
        &self,
        handle: SessionHandle,
        peer_addr: SocketAddr,
    ) -> Result<(), BoltError> {
        let mut sessions = self.sessions.write().unwrap();
        if let Some(limit) = self.max_sessions {
            if sessions.len() >= limit {
                return Err(BoltError::ResourceExhausted(format!(
                    "max sessions ({limit}) reached"
                )));
            }
        }
        let now = Instant::now();
        sessions.insert(
            handle.0.clone(),
            SessionState {
                handle,
                peer_addr,
                created_at: now,
                last_active: now,
            },
        );
        Ok(())
    }

    /// Removes a session.
    pub fn remove(&self, id: &str) {
        self.sessions.write().unwrap().remove(id);
    }

    /// Updates the last-active timestamp for a session.
    pub fn touch(&self, id: &str) {
        if let Some(state) = self.sessions.write().unwrap().get_mut(id) {
            state.last_active = Instant::now();
        }
    }

    /// Returns the number of active sessions.
    pub fn count(&self) -> usize {
        self.sessions.read().unwrap().len()
    }

    /// Removes sessions that have been idle longer than `timeout`.
    /// Returns the IDs of removed sessions.
    pub fn reap_idle(&self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        let mut sessions = self.sessions.write().unwrap();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, state)| now.duration_since(state.last_active) > timeout)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            sessions.remove(id);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:9999".parse().unwrap()
    }

    #[test]
    fn register_and_remove() {
        let mgr = SessionManager::new(None);
        mgr.register(SessionHandle("s1".into()), addr()).unwrap();
        assert_eq!(mgr.count(), 1);
        mgr.remove("s1");
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn capacity_limit() {
        let mgr = SessionManager::new(Some(1));
        mgr.register(SessionHandle("s1".into()), addr()).unwrap();
        let result = mgr.register(SessionHandle("s2".into()), addr());
        assert!(result.is_err());
    }
}
