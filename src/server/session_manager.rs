//! Bolt session tracking and idle reaping.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
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

    // The map is always left consistent (no code panics while holding the
    // lock), so a poisoned lock is safe to keep using.
    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, SessionState>> {
        self.sessions.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, SessionState>> {
        self.sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a new session. Fails if the capacity limit is reached.
    pub fn register(&self, handle: SessionHandle, peer_addr: SocketAddr) -> Result<(), BoltError> {
        let mut sessions = self.write();
        if let Some(limit) = self.max_sessions
            && sessions.len() >= limit
        {
            return Err(BoltError::ResourceExhausted(format!(
                "max sessions ({limit}) reached"
            )));
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
        self.write().remove(id);
    }

    /// Removes a session, returning true if it was still registered (false
    /// if it was already removed, for example by the idle reaper).
    pub(crate) fn remove_if_present(&self, id: &str) -> bool {
        self.write().remove(id).is_some()
    }

    /// Updates the last-active timestamp for a session.
    pub fn touch(&self, id: &str) {
        if let Some(state) = self.write().get_mut(id) {
            state.last_active = Instant::now();
        }
    }

    /// Returns true if the session with the given ID is still registered.
    pub fn contains(&self, id: &str) -> bool {
        self.read().contains_key(id)
    }

    /// Returns the number of active sessions.
    pub fn count(&self) -> usize {
        self.read().len()
    }

    /// Removes sessions that have been idle longer than `timeout`.
    /// Returns the IDs of removed sessions.
    pub fn reap_idle(&self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        let mut sessions = self.write();
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

    #[test]
    fn capacity_is_released_on_remove() {
        let mgr = SessionManager::new(Some(1));
        mgr.register(SessionHandle("s1".into()), addr()).unwrap();
        mgr.remove("s1");
        mgr.register(SessionHandle("s2".into()), addr()).unwrap();
        assert_eq!(mgr.count(), 1);
    }

    #[test]
    fn remove_if_present_reports_prior_registration() {
        let mgr = SessionManager::new(None);
        mgr.register(SessionHandle("s1".into()), addr()).unwrap();
        assert!(mgr.remove_if_present("s1"));
        assert!(!mgr.remove_if_present("s1"));
        assert!(!mgr.remove_if_present("never-registered"));
    }

    #[test]
    fn reap_idle_removes_only_expired_sessions() {
        let mgr = SessionManager::new(None);
        mgr.register(SessionHandle("old".into()), addr()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        mgr.register(SessionHandle("new".into()), addr()).unwrap();

        let reaped = mgr.reap_idle(Duration::from_millis(15));
        assert_eq!(reaped, vec!["old".to_string()]);
        assert!(!mgr.contains("old"));
        assert!(mgr.contains("new"));
    }

    #[test]
    fn touch_keeps_a_session_alive() {
        let mgr = SessionManager::new(None);
        mgr.register(SessionHandle("s1".into()), addr()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        mgr.touch("s1");
        assert!(mgr.reap_idle(Duration::from_millis(15)).is_empty());
        // Touching an unknown session is a no-op.
        mgr.touch("unknown");
    }
}
