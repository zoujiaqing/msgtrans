// Internal connection-state store: get_state is a diagnostic accessor.
#![allow(dead_code)]
use crate::SessionId;
/// Connection state management
///
/// Used for unified connection closing mechanism, preventing duplicate closing and race conditions
use std::sync::Arc;
use tokio::sync::Mutex;

/// Connection state
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// Connected, working normally
    Connected,
    /// Closing in progress, ignore all messages
    Closing,
    /// Closed
    Closed,
}

/// Connection state manager
///
/// Provides thread-safe state management, preventing duplicate closing
pub struct ConnectionStateManager {
    states:
        Arc<crate::transport::concurrent::ConcurrentMap<SessionId, Arc<Mutex<ConnectionState>>>>,
}

impl ConnectionStateManager {
    /// Create new state manager
    pub fn new() -> Self {
        Self {
            states: Arc::new(crate::transport::concurrent::ConcurrentMap::new()),
        }
    }

    /// Add new connection
    pub fn add_connection(&self, session_id: SessionId) {
        let state = Arc::new(Mutex::new(ConnectionState::Connected));
        if let Err(e) = self.states.insert(session_id, state) {
            tracing::error!(
                "[ERROR] Failed to insert connection state for session {}: {:?}",
                session_id,
                e
            );
        }
    }

    /// Try to start closing connection
    ///
    /// Returns true if closing can be started, false if already closing or closed
    pub async fn try_start_closing(&self, session_id: SessionId) -> bool {
        if let Some(state_lock) = self.states.get(&session_id) {
            let mut state = state_lock.lock().await;
            match *state {
                ConnectionState::Connected => {
                    *state = ConnectionState::Closing;
                    true
                }
                ConnectionState::Closing | ConnectionState::Closed => false,
            }
        } else {
            false
        }
    }

    /// Mark connection as closed
    pub async fn mark_closed(&self, session_id: SessionId) {
        if let Some(state_lock) = self.states.get(&session_id) {
            let mut state = state_lock.lock().await;
            *state = ConnectionState::Closed;
        }
    }

    /// Check if connection should ignore messages
    pub async fn should_ignore_messages(&self, session_id: SessionId) -> bool {
        if let Some(state_lock) = self.states.get(&session_id) {
            let state = state_lock.lock().await;
            matches!(*state, ConnectionState::Closing | ConnectionState::Closed)
        } else {
            true // Unknown connection, ignore messages
        }
    }

    /// Get connection state
    pub async fn get_state(&self, session_id: SessionId) -> Option<ConnectionState> {
        if let Some(state_lock) = self.states.get(&session_id) {
            let state = state_lock.lock().await;
            Some(state.clone())
        } else {
            None
        }
    }

    /// Remove connection state
    pub fn remove_connection(&self, session_id: SessionId) {
        if let Err(e) = self.states.remove(&session_id) {
            tracing::warn!(
                "[WARN] Failed to remove connection state for session {}: {:?}",
                session_id,
                e
            );
        }
    }
}

impl Default for ConnectionStateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ConnectionStateManager {
    fn clone(&self) -> Self {
        Self {
            states: self.states.clone(),
        }
    }
}

impl std::fmt::Debug for ConnectionStateManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionStateManager")
            .field("states_count", &self.states.len())
            .finish()
    }
}
