use crate::SessionId;
use dashmap::{DashMap, DashSet};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_BUCKET_COUNT: usize = 256;
const DEFAULT_TIMEOUT_TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestKey {
    pub session_id: Option<SessionId>,
    pub request_id: u32,
}

impl RequestKey {
    pub fn new(session_id: Option<SessionId>, request_id: u32) -> Self {
        Self {
            session_id,
            request_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RequestState {
    Pending = 0,
    Responded = 1,
    TimedOut = 2,
    SessionClosed = 3,
    Dropped = 4,
}

impl RequestState {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Pending),
            1 => Some(Self::Responded),
            2 => Some(Self::TimedOut),
            3 => Some(Self::SessionClosed),
            4 => Some(Self::Dropped),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct RequestEntry {
    pub key: RequestKey,
    pub biz_type: u8,
    pub created_at: Instant,
    pub deadline_at: Instant,
    state: AtomicU8,
}

impl RequestEntry {
    pub fn request_id(&self) -> u32 {
        self.key.request_id
    }

    pub fn session_id(&self) -> Option<SessionId> {
        self.key.session_id
    }

    pub fn state(&self) -> RequestState {
        RequestState::from_u8(self.state.load(Ordering::SeqCst)).unwrap_or(RequestState::Dropped)
    }

    fn try_transition(&self, from: RequestState, to: RequestState) -> Result<(), RequestState> {
        match self.state.compare_exchange(
            from as u8,
            to as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => Ok(()),
            Err(cur) => Err(RequestState::from_u8(cur).unwrap_or(RequestState::Dropped)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkResult {
    Updated,
    Already(RequestState),
    NotFound,
}

#[derive(Debug, Clone, Copy)]
pub struct RequestCountersSnapshot {
    pub pending_requests: u64,
    pub request_timeout_total: u64,
    pub duplicate_response_total: u64,
    pub session_closed_pending_total: u64,
    pub response_send_failed_total: u64,
}

#[derive(Debug, Default)]
pub struct RequestCounters {
    pending_requests: AtomicU64,
    request_timeout_total: AtomicU64,
    duplicate_response_total: AtomicU64,
    session_closed_pending_total: AtomicU64,
    response_send_failed_total: AtomicU64,
}

impl RequestCounters {
    fn snapshot(&self) -> RequestCountersSnapshot {
        RequestCountersSnapshot {
            pending_requests: self.pending_requests.load(Ordering::Relaxed),
            request_timeout_total: self.request_timeout_total.load(Ordering::Relaxed),
            duplicate_response_total: self.duplicate_response_total.load(Ordering::Relaxed),
            session_closed_pending_total: self.session_closed_pending_total.load(Ordering::Relaxed),
            response_send_failed_total: self.response_send_failed_total.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct RequestRegistry {
    entries: DashMap<RequestKey, Arc<RequestEntry>>,
    session_index: DashMap<SessionId, DashSet<RequestKey>>,
    counters: RequestCounters,
    buckets: Vec<std::sync::Mutex<Vec<RequestKey>>>,
    bucket_count: usize,
    tick_duration: Duration,
    current_tick: AtomicU64,
}

impl Default for RequestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestRegistry {
    pub fn new() -> Self {
        Self::new_with_timing(DEFAULT_TIMEOUT_BUCKET_COUNT, DEFAULT_TIMEOUT_TICK)
    }

    pub fn new_with_timing(bucket_count: usize, tick_duration: Duration) -> Self {
        let safe_bucket_count = bucket_count.max(8);
        let safe_tick = if tick_duration.is_zero() {
            DEFAULT_TIMEOUT_TICK
        } else {
            tick_duration
        };

        let mut buckets = Vec::with_capacity(safe_bucket_count);
        for _ in 0..safe_bucket_count {
            buckets.push(std::sync::Mutex::new(Vec::new()));
        }

        Self {
            entries: DashMap::new(),
            session_index: DashMap::new(),
            counters: RequestCounters::default(),
            buckets,
            bucket_count: safe_bucket_count,
            tick_duration: safe_tick,
            current_tick: AtomicU64::new(0),
        }
    }

    pub fn register(
        &self,
        request_id: u32,
        session_id: Option<SessionId>,
        biz_type: u8,
        timeout: Duration,
    ) -> bool {
        let key = RequestKey::new(session_id, request_id);
        let now = Instant::now();
        let entry = Arc::new(RequestEntry {
            key,
            biz_type,
            created_at: now,
            deadline_at: now + timeout,
            state: AtomicU8::new(RequestState::Pending as u8),
        });

        if self.entries.insert(key, entry).is_some() {
            return false;
        }

        if let Some(sid) = session_id {
            let set = self.session_index.entry(sid).or_default();
            set.insert(key);
        }

        self.counters.pending_requests.fetch_add(1, Ordering::Relaxed);
        self.schedule_for_deadline(key, now + timeout);
        true
    }

    pub fn get_state(&self, session_id: Option<SessionId>, request_id: u32) -> Option<RequestState> {
        self.entries
            .get(&RequestKey::new(session_id, request_id))
            .map(|entry| entry.state())
    }

    pub fn active_len(&self) -> usize {
        self.entries.len()
    }

    pub fn mark_responded(&self, session_id: Option<SessionId>, request_id: u32) -> MarkResult {
        let key = RequestKey::new(session_id, request_id);
        let Some(entry) = self.entries.get(&key) else {
            self.counters
                .duplicate_response_total
                .fetch_add(1, Ordering::Relaxed);
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::Responded) {
            Ok(_) => {
                self.counters.pending_requests.fetch_sub(1, Ordering::Relaxed);
                drop(entry);
                self.remove_terminal_entry(key);
                MarkResult::Updated
            }
            Err(state) => {
                if state == RequestState::Responded {
                    self.counters
                        .duplicate_response_total
                        .fetch_add(1, Ordering::Relaxed);
                }
                MarkResult::Already(state)
            }
        }
    }

    pub fn mark_timed_out(&self, key: RequestKey) -> MarkResult {
        let Some(entry) = self.entries.get(&key) else {
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::TimedOut) {
            Ok(_) => {
                self.counters.pending_requests.fetch_sub(1, Ordering::Relaxed);
                self.counters
                    .request_timeout_total
                    .fetch_add(1, Ordering::Relaxed);
                drop(entry);
                self.remove_terminal_entry(key);
                MarkResult::Updated
            }
            Err(state) => MarkResult::Already(state),
        }
    }

    pub fn mark_dropped(&self, session_id: Option<SessionId>, request_id: u32) -> MarkResult {
        let key = RequestKey::new(session_id, request_id);
        let Some(entry) = self.entries.get(&key) else {
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::Dropped) {
            Ok(_) => {
                self.counters.pending_requests.fetch_sub(1, Ordering::Relaxed);
                drop(entry);
                self.remove_terminal_entry(key);
                MarkResult::Updated
            }
            Err(state) => MarkResult::Already(state),
        }
    }

    pub fn close_session_pending(&self, session_id: SessionId) -> usize {
        let Some((_, ids)) = self.session_index.remove(&session_id) else {
            return 0;
        };

        let mut closed = 0usize;

        for key in ids.iter() {
            if let Some(entry) = self.entries.get(key.key()) {
                if entry
                    .try_transition(RequestState::Pending, RequestState::SessionClosed)
                    .is_ok()
                {
                    closed += 1;
                    self.counters.pending_requests.fetch_sub(1, Ordering::Relaxed);
                    let key = *key.key();
                    drop(entry);
                    self.entries.remove(&key);
                }
            }
        }

        if closed > 0 {
            self.counters
                .session_closed_pending_total
                .fetch_add(closed as u64, Ordering::Relaxed);
        }

        closed
    }

    pub fn record_response_send_failed(&self) {
        self.counters
            .response_send_failed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn counters_snapshot(&self) -> RequestCountersSnapshot {
        self.counters.snapshot()
    }

    pub fn pending_count(&self) -> u64 {
        self.counters.pending_requests.load(Ordering::Relaxed)
    }

    pub fn tick_duration(&self) -> Duration {
        self.tick_duration
    }

    pub fn scan_timeout_bucket(&self) -> usize {
        let next_tick = self.current_tick.fetch_add(1, Ordering::SeqCst) + 1;
        let bucket_idx = (next_tick as usize) % self.bucket_count;
        let mut drained = Vec::new();

        if let Ok(mut bucket) = self.buckets[bucket_idx].lock() {
            std::mem::swap(&mut drained, &mut *bucket);
        }

        if drained.is_empty() {
            return 0;
        }

        let now = Instant::now();
        let mut timed_out = 0usize;

        for key in drained {
            let Some(entry) = self.entries.get(&key) else {
                continue;
            };

            if entry.state() != RequestState::Pending {
                continue;
            }

            if entry.deadline_at <= now {
                drop(entry);
                if self.mark_timed_out(key) == MarkResult::Updated {
                    timed_out += 1;
                }
            } else {
                self.schedule_for_deadline(key, entry.deadline_at);
            }
        }

        timed_out
    }

    fn remove_terminal_entry(&self, key: RequestKey) {
        self.entries.remove(&key);
        if let Some(session_id) = key.session_id {
            if let Some(set) = self.session_index.get(&session_id) {
                set.remove(&key);
            }
        }
    }

    fn schedule_for_deadline(&self, key: RequestKey, deadline_at: Instant) {
        let now = Instant::now();
        let ticks_from_now = if deadline_at <= now {
            1
        } else {
            let remaining = deadline_at.duration_since(now).as_nanos();
            let tick_ns = self.tick_duration.as_nanos();
            (((remaining + tick_ns - 1) / tick_ns) as u64).max(1)
        };

        let base_tick = self.current_tick.load(Ordering::Relaxed);
        let target_tick = base_tick.saturating_add(ticks_from_now);
        let bucket_idx = (target_tick as usize) % self.bucket_count;

        if let Ok(mut bucket) = self.buckets[bucket_idx].lock() {
            bucket.push(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_transition_to_responded_once() {
        let registry = RequestRegistry::new();
        let request_id = 42;
        let session_id = Some(SessionId(7));

        assert!(registry.register(request_id, session_id, 1, Duration::from_secs(3)));
        assert_eq!(
            registry.get_state(session_id, request_id),
            Some(RequestState::Pending)
        );

        assert_eq!(
            registry.mark_responded(session_id, request_id),
            MarkResult::Updated
        );
        assert_eq!(registry.get_state(session_id, request_id), None);

        assert_eq!(
            registry.mark_responded(session_id, request_id),
            MarkResult::NotFound
        );

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.duplicate_response_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn same_request_id_is_isolated_by_session() {
        let registry = RequestRegistry::new();

        assert!(registry.register(77, Some(SessionId(1)), 0, Duration::from_secs(3)));
        assert!(registry.register(77, Some(SessionId(2)), 0, Duration::from_secs(3)));

        assert_eq!(
            registry.mark_responded(Some(SessionId(1)), 77),
            MarkResult::Updated
        );
        assert_eq!(registry.get_state(Some(SessionId(1)), 77), None);
        assert_eq!(
            registry.get_state(Some(SessionId(2)), 77),
            Some(RequestState::Pending)
        );
        assert_eq!(registry.pending_count(), 1);
    }

    #[test]
    fn timeout_only_updates_pending() {
        let registry = RequestRegistry::new();
        let request_id = 100;
        let key = RequestKey::new(None, request_id);

        assert!(registry.register(request_id, None, 2, Duration::from_secs(1)));
        assert_eq!(registry.mark_timed_out(key), MarkResult::Updated);
        assert_eq!(registry.get_state(None, request_id), None);

        assert_eq!(registry.mark_timed_out(key), MarkResult::NotFound);

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.request_timeout_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn close_session_batch_transitions_pending_to_session_closed() {
        let registry = RequestRegistry::new();
        let sid = SessionId(999);

        assert!(registry.register(1, Some(sid), 0, Duration::from_secs(5)));
        assert!(registry.register(2, Some(sid), 0, Duration::from_secs(5)));
        assert!(registry.register(3, Some(SessionId(1000)), 0, Duration::from_secs(5)));

        let closed = registry.close_session_pending(sid);
        assert_eq!(closed, 2);

        assert_eq!(registry.get_state(Some(sid), 1), None);
        assert_eq!(registry.get_state(Some(sid), 2), None);
        assert_eq!(
            registry.get_state(Some(SessionId(1000)), 3),
            Some(RequestState::Pending)
        );

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 1);
        assert_eq!(snapshot.session_closed_pending_total, 2);
        assert_eq!(registry.active_len(), 1);
    }

    #[test]
    fn duplicate_register_is_rejected_within_same_session() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(9));
        assert!(registry.register(77, sid, 0, Duration::from_secs(2)));
        assert!(!registry.register(77, sid, 0, Duration::from_secs(2)));
    }

    #[test]
    fn timeout_scanner_marks_due_requests_only() {
        let registry = RequestRegistry::new_with_timing(32, Duration::from_millis(10));
        assert!(registry.register(1, None, 0, Duration::from_millis(15)));
        assert!(registry.register(2, None, 0, Duration::from_secs(1)));

        std::thread::sleep(Duration::from_millis(20));
        let mut timeout_total = 0usize;
        for _ in 0..4 {
            timeout_total += registry.scan_timeout_bucket();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(timeout_total >= 1);
        assert_eq!(registry.get_state(None, 1), None);
        assert_eq!(registry.get_state(None, 2), Some(RequestState::Pending));
        assert_eq!(registry.active_len(), 1);
    }

    #[test]
    fn response_send_failure_is_counted() {
        let registry = RequestRegistry::new();
        registry.record_response_send_failed();

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.response_send_failed_total, 1);
    }
}
