//! Shared adapter primitives: a single connection-state source of truth and an
//! owner for background tasks.
//!
//! These replace per-adapter ad-hoc state (three different fields expressing
//! "is this connection alive?") and per-adapter task teardown (which could
//! orphan a task when a `select!` dropped its `JoinHandle`).

use crate::SessionId;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Lifecycle state of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ConnStatus {
    Connecting = 0,
    Connected = 1,
    Closed = 2,
}

impl ConnStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => ConnStatus::Connected,
            2 => ConnStatus::Closed,
            _ => ConnStatus::Connecting,
        }
    }
}

/// Single source of truth for a connection's liveness and session id, shared
/// cheaply (via `Arc`) between an adapter and its event loop.
#[derive(Debug, Clone)]
pub(crate) struct ConnState {
    status: Arc<AtomicU8>,
    session_id: Arc<AtomicU64>,
}

impl ConnState {
    pub(crate) fn new(status: ConnStatus) -> Self {
        Self {
            status: Arc::new(AtomicU8::new(status as u8)),
            session_id: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn status(&self) -> ConnStatus {
        ConnStatus::from_u8(self.status.load(Ordering::SeqCst))
    }

    pub(crate) fn set_status(&self, status: ConnStatus) {
        self.status.store(status as u8, Ordering::SeqCst);
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.status() == ConnStatus::Connected
    }

    pub(crate) fn session_id(&self) -> SessionId {
        SessionId(self.session_id.load(Ordering::SeqCst))
    }

    pub(crate) fn set_session_id(&self, session_id: SessionId) {
        self.session_id.store(session_id.0, Ordering::SeqCst);
    }
}

/// Owns background tasks and cancels them deterministically on teardown.
///
/// Solves the ownerless-task problem: a `select!` that awaits handles by value
/// drops the losing handle, and dropping a `JoinHandle` does *not* cancel its
/// task -- it becomes an orphan. Handing tasks to a `TaskGroup` means teardown
/// (explicit `abort_all` or `Drop`) actively aborts every remaining task.
#[derive(Default)]
pub(crate) struct TaskGroup {
    handles: Vec<JoinHandle<()>>,
}

impl TaskGroup {
    pub(crate) fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, handle: JoinHandle<()>) {
        self.handles.push(handle);
    }

    /// Abort every task still running.
    pub(crate) fn abort_all(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for TaskGroup {
    fn drop(&mut self) {
        self.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn conn_state_transitions_and_session_id() {
        let state = ConnState::new(ConnStatus::Connecting);
        assert!(!state.is_connected());
        assert_eq!(state.session_id(), SessionId(0));

        state.set_status(ConnStatus::Connected);
        assert!(state.is_connected());

        state.set_session_id(SessionId(42));
        assert_eq!(state.session_id(), SessionId(42));

        // A clone observes the same shared state.
        let clone = state.clone();
        state.set_status(ConnStatus::Closed);
        assert_eq!(clone.status(), ConnStatus::Closed);
        assert!(!clone.is_connected());
    }

    #[tokio::test]
    async fn task_group_aborts_remaining_tasks_on_teardown() {
        let flag = Arc::new(AtomicU8::new(0));

        let mut group = TaskGroup::new();
        let f = flag.clone();
        // A task that would run "forever" unless aborted.
        group.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                f.fetch_add(1, Ordering::SeqCst);
            }
        }));

        // Give it a moment to start.
        tokio::time::sleep(Duration::from_millis(25)).await;
        group.abort_all();
        let after_abort = flag.load(Ordering::SeqCst);

        // It must not keep incrementing after being aborted.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(flag.load(Ordering::SeqCst), after_abort);
    }

    #[tokio::test]
    async fn task_group_drop_aborts() {
        let flag = Arc::new(AtomicU8::new(0));
        {
            let mut group = TaskGroup::new();
            let f = flag.clone();
            group.push(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    f.fetch_add(1, Ordering::SeqCst);
                }
            }));
            tokio::time::sleep(Duration::from_millis(25)).await;
        } // dropped here -> aborts

        let after_drop = flag.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(flag.load(Ordering::SeqCst), after_drop);
    }
}
