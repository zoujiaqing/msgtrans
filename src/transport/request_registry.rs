// Internal request-lifecycle state machine: some accessors (counters/
// snapshots/entry getters) are exercised only by tests or diagnostics.
#![allow(dead_code)]
use crate::packet::Packet;
use crate::SessionId;
use dashmap::{DashMap, DashSet};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const DEFAULT_TIMEOUT_BUCKET_COUNT: usize = 256;
const DEFAULT_TIMEOUT_TICK: Duration = Duration::from_millis(100);

/// Which side of the wire a tracked request belongs to.
///
/// Inbound and outbound requests live in disjoint id spaces (each peer numbers
/// its own requests), so the same `(session_id, request_id)` can legitimately
/// be in flight in both directions at once. Without this field in the key, an
/// inbound request colliding with a pending outbound one was refused outright,
/// and responding to it could terminate the outbound entry instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestDirection {
    /// A request received from the peer — we owe the response.
    Inbound,
    /// A request we sent — we await the response.
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestKey {
    pub session_id: Option<SessionId>,
    pub request_id: u32,
    pub direction: RequestDirection,
}

impl RequestKey {
    pub fn new(
        session_id: Option<SessionId>,
        request_id: u32,
        direction: RequestDirection,
    ) -> Self {
        Self {
            session_id,
            request_id,
            direction,
        }
    }

    fn inbound(session_id: Option<SessionId>, request_id: u32) -> Self {
        Self::new(session_id, request_id, RequestDirection::Inbound)
    }

    fn outbound(session_id: Option<SessionId>, request_id: u32) -> Self {
        Self::new(session_id, request_id, RequestDirection::Outbound)
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
    /// A response send is in flight: claimed by exactly one responder, not
    /// yet confirmed written. Duplicates are refused while here.
    Responding = 5,
    /// The response send failed (enqueue or write): terminal, counted.
    SendFailed = 6,
}

impl RequestState {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Pending),
            1 => Some(Self::Responded),
            2 => Some(Self::TimedOut),
            3 => Some(Self::SessionClosed),
            4 => Some(Self::Dropped),
            5 => Some(Self::Responding),
            6 => Some(Self::SendFailed),
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
    /// Registration generation: stamped from a registry-wide counter so a
    /// token minted for an earlier registration of the same key can never
    /// act on a later one (ABA defense).
    generation: u64,
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
        match self
            .state
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
        {
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

/// Unforgeable handle to ONE registration of ONE request.
///
/// Carries the full identity — session, direction, message id AND the
/// per-registration generation — so a token from an earlier life of a reused
/// message id can never claim a later registration (the message-ID ABA
/// window is closed by construction). Fields are crate-private and there is
/// no public constructor: the only source of a token is the registry itself
/// at registration time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestToken {
    pub(crate) session_id: Option<SessionId>,
    pub(crate) request_id: u32,
    pub(crate) direction: RequestDirection,
    pub(crate) generation: u64,
}

impl RequestToken {
    /// The request (message) id this token belongs to.
    pub fn request_id(&self) -> u32 {
        self.request_id
    }

    /// The session the request arrived on, if session-scoped.
    pub fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    pub(crate) fn key(&self) -> RequestKey {
        match self.direction {
            RequestDirection::Inbound => RequestKey::inbound(self.session_id, self.request_id),
            RequestDirection::Outbound => RequestKey::outbound(self.session_id, self.request_id),
        }
    }
}

/// A claimed respond (`Pending -> Responding`) whose resolution CANNOT be lost.
///
/// The claim is created in the same poll that wins `begin_respond` and then
/// travels with the response through the outbound queue to the write loop.
/// Whoever ends up owning it resolves it exactly once:
///
/// - `resolve(true)`  — the write reached the socket: `Responding -> Responded`.
/// - `resolve(false)` — the write failed: `Responding -> SendFailed` (counted).
/// - **Drop without resolve** — the carrying future was cancelled, the queue
///   entry was discarded, or the connection died: resolved as a send failure.
///
/// This is what makes the `Responding` state cancellation-safe: completion
/// ownership lives with the queued write, not with the caller's future, so a
/// `timeout`/`select!`/abort around a respond can never strand the registry
/// entry (the timeout wheel deliberately never touches `Responding`).
#[derive(Debug)]
pub(crate) struct RespondClaim {
    registry: Arc<RequestRegistry>,
    token: RequestToken,
    resolved: bool,
}

impl RespondClaim {
    pub(crate) fn new(registry: Arc<RequestRegistry>, token: RequestToken) -> Self {
        Self {
            registry,
            token,
            resolved: false,
        }
    }

    pub(crate) fn resolve(mut self, write_confirmed: bool) {
        self.resolved = true;
        self.registry.finish_respond(&self.token, write_confirmed);
    }
}

impl Drop for RespondClaim {
    fn drop(&mut self) {
        if !self.resolved {
            // Abandoned mid-flight (cancelled future / dropped queue entry /
            // dead connection): the write did not demonstrably happen.
            self.registry.finish_respond(&self.token, false);
        }
    }
}

/// Returned by `try_register_waiter` when a live pending request already exists
/// for the same (session_id, request_id). The new waiter is refused rather than
/// silently replacing the old one (which would cancel the old receiver and could
/// misroute the response).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateRequest {
    pub session_id: Option<SessionId>,
    pub request_id: u32,
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

/// Per-session lifecycle object, replacing the closing-tombstone TTL.
///
/// A session's runtime exists exactly while the session is open: it is created
/// by `open_session` (called only by the owning accept/connect path) and
/// removed by `close_session_pending`. Registration cannot create one, so a
/// register racing a close either finds the runtime and is drained with it, or
/// finds nothing and is refused — absence *is* the closed state, with no
/// wall-clock assumption and no per-disconnect memory left behind.
///
/// The backbone rework extends this into the full session supervisor
/// (connection permit, cancellation token, task handles).
#[derive(Debug)]
struct SessionRuntime {
    /// 0 = Open, 1 = Closing. One-way.
    state: AtomicU8,
    /// Keys of this session's live requests, drained on close.
    requests: DashSet<RequestKey>,
}

impl SessionRuntime {
    const OPEN: u8 = 0;
    const CLOSING: u8 = 1;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::OPEN),
            requests: DashSet::new(),
        }
    }

    fn is_open(&self) -> bool {
        self.state.load(Ordering::SeqCst) == Self::OPEN
    }

    fn begin_close(&self) {
        self.state.store(Self::CLOSING, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct RequestRegistry {
    entries: DashMap<RequestKey, Arc<RequestEntry>>,
    /// Response waiters, keyed like `entries`. Present only for requests whose
    /// caller is awaiting a response (the request/response path); pure
    /// lifecycle-tracked requests (e.g. inbound server requests) have no waiter.
    waiters: DashMap<RequestKey, oneshot::Sender<Packet>>,
    /// Live sessions only. See `SessionRuntime` for the lifecycle contract.
    sessions: DashMap<SessionId, Arc<SessionRuntime>>,
    counters: RequestCounters,
    buckets: Vec<std::sync::Mutex<Vec<RequestKey>>>,
    bucket_count: usize,
    tick_duration: Duration,
    current_tick: AtomicU64,
    /// Allocator for outbound request ids (absorbed from the former
    /// RequestTracker, so the registry owns the whole request lifecycle).
    next_id: std::sync::atomic::AtomicU32,
    /// Monotonic registration generation for RequestToken minting.
    next_generation: AtomicU64,
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

    /// Create a registry whose outbound request ids start at `start_id`.
    pub fn new_with_start_id(start_id: u32) -> Self {
        let registry = Self::new();
        registry
            .next_id
            .store(start_id, std::sync::atomic::Ordering::Relaxed);
        registry
    }

    /// Open a session for request tracking. Called only by the owning
    /// accept/connect path — registration never creates a session, so a
    /// session that was closed (or never opened) refuses all requests.
    pub fn open_session(&self, session_id: SessionId) {
        self.sessions
            .entry(session_id)
            .or_insert_with(|| Arc::new(SessionRuntime::new()));
    }

    /// Allocate the next outbound request id.
    pub fn next_message_id(&self) -> u32 {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
            waiters: DashMap::new(),
            sessions: DashMap::new(),
            counters: RequestCounters::default(),
            buckets,
            bucket_count: safe_bucket_count,
            tick_duration: safe_tick,
            current_tick: AtomicU64::new(0),
            next_id: std::sync::atomic::AtomicU32::new(1),
            next_generation: AtomicU64::new(1),
        }
    }

    /// Track an inbound request (one the peer sent us and we owe a response to).
    pub fn register(
        &self,
        request_id: u32,
        session_id: Option<SessionId>,
        biz_type: u8,
        timeout: Duration,
    ) -> Option<RequestToken> {
        // Inbound requests have no caller-side timeout, so they are scheduled into
        // the timeout wheel and reaped by the background scanner.
        self.register_impl(
            RequestKey::inbound(session_id, request_id),
            biz_type,
            timeout,
            true,
        )
    }

    fn register_impl(
        &self,
        key: RequestKey,
        biz_type: u8,
        timeout: Duration,
        schedule: bool,
    ) -> Option<RequestToken> {
        // Resolve the session runtime up front. No runtime (never opened, or
        // already closed and removed) means refuse — registration can never
        // resurrect a session.
        let runtime = match key.session_id {
            Some(sid) => match self.sessions.get(&sid) {
                Some(rt) if rt.is_open() => Some(rt.clone()),
                _ => return None,
            },
            None => None,
        };

        let now = Instant::now();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let entry = Arc::new(RequestEntry {
            key,
            biz_type,
            created_at: now,
            deadline_at: now + timeout,
            generation,
            state: AtomicU8::new(RequestState::Pending as u8),
        });

        // Counter goes up BEFORE the entry becomes visible, so a concurrent
        // close that transitions the entry and decrements can never drive the
        // counter below zero.
        self.counters
            .pending_requests
            .fetch_add(1, Ordering::Relaxed);

        use dashmap::mapref::entry::Entry;
        match self.entries.entry(key) {
            Entry::Occupied(_) => {
                self.counters
                    .pending_requests
                    .fetch_sub(1, Ordering::Relaxed);
                return None; // duplicate: refuse, do not replace
            }
            Entry::Vacant(vacant) => {
                vacant.insert(entry);
            }
        }

        if let Some(rt) = runtime {
            rt.requests.insert(key);
            // Re-check: the close marks Closing BEFORE draining, so if we see
            // Open here our insert happened before the drain read the set and
            // will be drained with it; if we see Closing, the drain may have
            // run without our key — undo so no Pending entry outlives its
            // session.
            if !rt.is_open() {
                if let Some(entry) = self.entries.get(&key) {
                    if entry
                        .try_transition(RequestState::Pending, RequestState::SessionClosed)
                        .is_ok()
                    {
                        self.counters
                            .pending_requests
                            .fetch_sub(1, Ordering::Relaxed);
                        drop(entry);
                        self.remove_terminal_entry(key);
                    }
                }
                self.waiters.remove(&key);
                return None;
            }
        }

        if schedule {
            self.schedule_for_deadline(key, now + timeout);
        }
        Some(RequestToken {
            session_id: key.session_id,
            request_id: key.request_id,
            direction: key.direction,
            generation,
        })
    }

    /// Register a request together with a response waiter, returning the receiver
    /// the caller awaits. This is the request/response path: the registry is both
    /// the lifecycle source of truth and the response waker.
    ///
    /// Refuses (returns `Err(DuplicateRequest)`) if a live pending request already
    /// exists for the same (session_id, request_id), rather than silently replacing
    /// its waiter — which would cancel the old receiver and could misroute the
    /// response to the wrong caller.
    pub fn try_register_waiter(
        &self,
        request_id: u32,
        session_id: Option<SessionId>,
        biz_type: u8,
        timeout: Duration,
    ) -> Result<oneshot::Receiver<Packet>, DuplicateRequest> {
        // Waiter-based (outbound) requests rely on the caller's own timeout
        // (e.g. tokio::time::timeout) plus explicit removal, so they are NOT
        // scheduled into the timeout wheel. This also avoids unbounded bucket
        // growth on clients that run no timeout scanner.
        let key = RequestKey::outbound(session_id, request_id);
        if self.register_impl(key, biz_type, timeout, false).is_none() {
            return Err(DuplicateRequest {
                session_id,
                request_id,
            });
        }
        let (tx, rx) = oneshot::channel();
        self.waiters.insert(key, tx);
        // If the session closed between register_impl and the waiter insert,
        // the drain removed the entry but could not see this waiter — it would
        // sit orphaned until the caller's own timeout. Detect and undo.
        if self.entries.get(&key).is_none() {
            self.waiters.remove(&key); // drops tx -> rx observes closure
            return Err(DuplicateRequest {
                session_id,
                request_id,
            });
        }
        Ok(rx)
    }

    /// Complete a request with its response: wake the waiter (if any) and move
    /// lifecycle state to Responded. Returns true iff a pending request matched
    /// (same session + id), which is what prevents cross-session response injection.
    /// Pending -> Responded for an entry addressed by key. Outbound-waiter
    /// internal path only: the waiter channel is its own capability, so no
    /// generation check is needed (each registration replaces the waiter).
    fn mark_responded_key(&self, key: RequestKey) -> MarkResult {
        let Some(entry) = self.entries.get(&key) else {
            self.counters
                .duplicate_response_total
                .fetch_add(1, Ordering::Relaxed);
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::Responded) {
            Ok(_) => {
                self.counters
                    .pending_requests
                    .fetch_sub(1, Ordering::Relaxed);
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

    pub fn complete_waiter(
        &self,
        session_id: Option<SessionId>,
        request_id: u32,
        packet: Packet,
    ) -> bool {
        let key = RequestKey::outbound(session_id, request_id);
        match self.mark_responded_key(key) {
            MarkResult::Updated => {
                if let Some((_, tx)) = self.waiters.remove(&key) {
                    let _ = tx.send(packet);
                }
                true
            }
            _ => false,
        }
    }

    /// Abandon a request (caller gave up or the connection dropped): mark it
    /// Dropped and drop its waiter so the receiver observes cancellation.
    /// Returns true if a pending request was aborted.
    pub fn abort_waiter(&self, session_id: Option<SessionId>, request_id: u32) -> bool {
        let key = RequestKey::outbound(session_id, request_id);
        let aborted = matches!(self.mark_dropped_key(key), MarkResult::Updated);
        self.waiters.remove(&key);
        aborted
    }

    /// Abort every in-flight request (e.g. the connection closed), dropping all
    /// waiters. Returns the number aborted.
    pub fn abort_all(&self) -> usize {
        // Connection-level teardown: every session this registry tracks is
        // over. Close their runtimes so late registers are refused, then
        // abort whatever remains keyed without a session.
        let sids: Vec<SessionId> = self.sessions.iter().map(|e| *e.key()).collect();
        for sid in sids {
            if let Some((_, rt)) = self.sessions.remove(&sid) {
                rt.begin_close();
            }
        }
        let keys: Vec<RequestKey> = self.entries.iter().map(|e| *e.key()).collect();
        let mut aborted = 0;
        for key in keys {
            if matches!(self.mark_dropped_key(key), MarkResult::Updated) {
                aborted += 1;
            }
            self.waiters.remove(&key);
        }
        aborted
    }

    pub fn get_state(
        &self,
        session_id: Option<SessionId>,
        request_id: u32,
        direction: RequestDirection,
    ) -> Option<RequestState> {
        self.entries
            .get(&RequestKey::new(session_id, request_id, direction))
            .map(|entry| entry.state())
    }

    pub fn active_len(&self) -> usize {
        self.entries.len()
    }

    /// Claim an inbound request for responding: Pending -> Responding.
    /// Exactly one responder wins; duplicates (including a concurrent
    /// responder currently in flight) are refused with the observed state.
    /// The token's registration generation must match the live entry — a
    /// token minted for an earlier life of a reused message id observes
    /// NotFound instead of claiming the new registration (ABA defense).
    pub fn begin_respond(&self, token: &RequestToken) -> MarkResult {
        let key = token.key();
        let Some(entry) = self.entries.get(&key) else {
            self.counters
                .duplicate_response_total
                .fetch_add(1, Ordering::Relaxed);
            return MarkResult::NotFound;
        };
        if entry.generation != token.generation {
            // Same key, different registration: the token's request is long
            // gone and its id was reused. Refuse without touching the entry.
            self.counters
                .duplicate_response_total
                .fetch_add(1, Ordering::Relaxed);
            return MarkResult::NotFound;
        }
        match entry.try_transition(RequestState::Pending, RequestState::Responding) {
            Ok(_) => MarkResult::Updated,
            Err(state) => {
                if matches!(state, RequestState::Responded | RequestState::Responding) {
                    self.counters
                        .duplicate_response_total
                        .fetch_add(1, Ordering::Relaxed);
                }
                MarkResult::Already(state)
            }
        }
    }

    /// Resolve a claimed respond: Responding -> Responded (write confirmed)
    /// or Responding -> SendFailed. Both are terminal; the entry is removed
    /// and the pending gauge decremented exactly once.
    pub fn finish_respond(&self, token: &RequestToken, write_confirmed: bool) -> MarkResult {
        let key = token.key();
        let Some(entry) = self.entries.get(&key) else {
            return MarkResult::NotFound;
        };
        if entry.generation != token.generation {
            return MarkResult::NotFound;
        }
        let target = if write_confirmed {
            RequestState::Responded
        } else {
            RequestState::SendFailed
        };
        match entry.try_transition(RequestState::Responding, target) {
            Ok(_) => {
                self.counters
                    .pending_requests
                    .fetch_sub(1, Ordering::Relaxed);
                if !write_confirmed {
                    self.counters
                        .response_send_failed_total
                        .fetch_add(1, Ordering::Relaxed);
                }
                drop(entry);
                self.remove_terminal_entry(key);
                MarkResult::Updated
            }
            Err(state) => MarkResult::Already(state),
        }
    }

    pub fn mark_timed_out(&self, key: RequestKey) -> MarkResult {
        let Some(entry) = self.entries.get(&key) else {
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::TimedOut) {
            Ok(_) => {
                self.counters
                    .pending_requests
                    .fetch_sub(1, Ordering::Relaxed);
                self.counters
                    .request_timeout_total
                    .fetch_add(1, Ordering::Relaxed);
                drop(entry);
                self.remove_terminal_entry(key);
                self.waiters.remove(&key); // drop waiter -> receiver observes cancellation
                MarkResult::Updated
            }
            Err(state) => MarkResult::Already(state),
        }
    }

    fn mark_dropped_key(&self, key: RequestKey) -> MarkResult {
        let Some(entry) = self.entries.get(&key) else {
            return MarkResult::NotFound;
        };

        match entry.try_transition(RequestState::Pending, RequestState::Dropped) {
            Ok(_) => {
                self.counters
                    .pending_requests
                    .fetch_sub(1, Ordering::Relaxed);
                drop(entry);
                self.remove_terminal_entry(key);
                MarkResult::Updated
            }
            Err(state) => MarkResult::Already(state),
        }
    }

    pub fn close_session_pending(&self, session_id: SessionId) -> usize {
        // Remove the runtime first (no new registers can find it), mark it
        // Closing (registers already holding the Arc will undo on re-check),
        // then drain. Once this returns, absence of the runtime is the
        // permanent closed state — nothing to age out.
        let Some((_, runtime)) = self.sessions.remove(&session_id) else {
            return 0;
        };
        runtime.begin_close();

        let mut closed = 0usize;
        for key in runtime.requests.iter() {
            let key = *key.key();
            if let Some(entry) = self.entries.get(&key) {
                // Pending AND Responding both die with their session — a
                // respond in flight when the connection ends must not leave
                // an immortal Responding entry behind.
                let died = entry
                    .try_transition(RequestState::Pending, RequestState::SessionClosed)
                    .is_ok()
                    || entry
                        .try_transition(RequestState::Responding, RequestState::SessionClosed)
                        .is_ok();
                if died {
                    closed += 1;
                    self.counters
                        .pending_requests
                        .fetch_sub(1, Ordering::Relaxed);
                    drop(entry);
                    self.entries.remove(&key);
                    self.waiters.remove(&key); // drop waiter -> receiver observes cancellation
                }
            }
        }
        runtime.requests.clear();

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
            if let Some(rt) = self.sessions.get(&session_id) {
                rt.requests.remove(&key);
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
            (remaining.div_ceil(tick_ns) as u64).max(1)
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

    fn open(registry: &RequestRegistry, sids: &[u64]) {
        for sid in sids {
            registry.open_session(SessionId(*sid));
        }
    }

    #[test]
    fn register_and_transition_to_responded_once() {
        let registry = RequestRegistry::new();
        let request_id = 42;
        let session_id = Some(SessionId(7));
        open(&registry, &[7]);

        let token = registry
            .register(request_id, session_id, 1, Duration::from_secs(3))
            .expect("registers");
        assert_eq!(
            registry.get_state(session_id, request_id, RequestDirection::Inbound),
            Some(RequestState::Pending)
        );

        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);
        assert_eq!(registry.finish_respond(&token, true), MarkResult::Updated);
        assert_eq!(
            registry.get_state(session_id, request_id, RequestDirection::Inbound),
            None
        );

        assert_eq!(registry.begin_respond(&token), MarkResult::NotFound);

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.duplicate_response_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn begin_finish_respond_confirmed_write() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(7));
        open(&registry, &[7]);
        let token = registry
            .register(1, sid, 1, Duration::from_secs(3))
            .expect("registers");

        // Claim: Pending -> Responding, exactly once.
        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);
        assert_eq!(
            registry.get_state(sid, 1, RequestDirection::Inbound),
            Some(RequestState::Responding)
        );
        // Concurrent duplicate is refused and counted while in flight.
        assert_eq!(
            registry.begin_respond(&token),
            MarkResult::Already(RequestState::Responding)
        );

        // Write confirmed: terminal, entry removed, gauge decremented once.
        assert_eq!(registry.finish_respond(&token, true), MarkResult::Updated);
        assert_eq!(registry.get_state(sid, 1, RequestDirection::Inbound), None);
        assert_eq!(registry.finish_respond(&token, true), MarkResult::NotFound);

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.duplicate_response_total, 1);
        assert_eq!(snapshot.response_send_failed_total, 0);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn finish_respond_failed_write_is_terminal_and_counted() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(7));
        open(&registry, &[7]);
        let token = registry
            .register(1, sid, 1, Duration::from_secs(3))
            .expect("registers");

        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);
        assert_eq!(registry.finish_respond(&token, false), MarkResult::Updated);
        // Terminal: no retry slot, entry gone.
        assert_eq!(registry.get_state(sid, 1, RequestDirection::Inbound), None);
        assert_eq!(registry.begin_respond(&token), MarkResult::NotFound);

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.response_send_failed_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn close_session_drains_responding_entries() {
        // A respond in flight when the session dies must not leave an immortal
        // Responding entry: close_session transitions it to SessionClosed, and
        // the late finish_respond observes NotFound (no double decrement).
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(7));
        open(&registry, &[7]);
        let token = registry
            .register(1, sid, 1, Duration::from_secs(3))
            .expect("registers");
        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);

        assert_eq!(registry.close_session_pending(SessionId(7)), 1);
        assert_eq!(registry.get_state(sid, 1, RequestDirection::Inbound), None);
        assert_eq!(registry.finish_respond(&token, true), MarkResult::NotFound);

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn timeout_does_not_touch_responding() {
        // A claimed respond is past the point of timing out: only the write
        // outcome (or session close) may resolve it.
        let registry = RequestRegistry::new_with_timing(32, Duration::from_millis(10));
        let sid = Some(SessionId(7));
        open(&registry, &[7]);
        let token = registry
            .register(1, sid, 1, Duration::from_millis(1))
            .expect("registers");
        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);

        std::thread::sleep(Duration::from_millis(20));
        let mut timeout_total = 0usize;
        for _ in 0..4 {
            timeout_total += registry.scan_timeout_bucket();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(timeout_total, 0);
        assert_eq!(
            registry.get_state(sid, 1, RequestDirection::Inbound),
            Some(RequestState::Responding)
        );
        assert_eq!(registry.finish_respond(&token, true), MarkResult::Updated);
    }

    #[test]
    fn same_request_id_is_isolated_by_session() {
        let registry = RequestRegistry::new();
        open(&registry, &[1, 2]);

        let token1 = registry
            .register(77, Some(SessionId(1)), 0, Duration::from_secs(3))
            .expect("registers");
        assert!(registry
            .register(77, Some(SessionId(2)), 0, Duration::from_secs(3))
            .is_some());

        assert_eq!(registry.begin_respond(&token1), MarkResult::Updated);
        assert_eq!(registry.finish_respond(&token1, true), MarkResult::Updated);
        assert_eq!(
            registry.get_state(Some(SessionId(1)), 77, RequestDirection::Inbound),
            None
        );
        assert_eq!(
            registry.get_state(Some(SessionId(2)), 77, RequestDirection::Inbound),
            Some(RequestState::Pending)
        );
        assert_eq!(registry.pending_count(), 1);
    }

    #[test]
    fn timeout_only_updates_pending() {
        let registry = RequestRegistry::new();
        let request_id = 100;
        let key = RequestKey::inbound(None, request_id);

        assert!(registry
            .register(request_id, None, 2, Duration::from_secs(1))
            .is_some());
        assert_eq!(registry.mark_timed_out(key), MarkResult::Updated);
        assert_eq!(
            registry.get_state(None, request_id, RequestDirection::Inbound),
            None
        );

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
        open(&registry, &[999, 1000]);

        assert!(registry
            .register(1, Some(sid), 0, Duration::from_secs(5))
            .is_some());
        assert!(registry
            .register(2, Some(sid), 0, Duration::from_secs(5))
            .is_some());
        assert!(registry
            .register(3, Some(SessionId(1000)), 0, Duration::from_secs(5))
            .is_some());

        let closed = registry.close_session_pending(sid);
        assert_eq!(closed, 2);

        assert_eq!(
            registry.get_state(Some(sid), 1, RequestDirection::Inbound),
            None
        );
        assert_eq!(
            registry.get_state(Some(sid), 2, RequestDirection::Inbound),
            None
        );
        assert_eq!(
            registry.get_state(Some(SessionId(1000)), 3, RequestDirection::Inbound),
            Some(RequestState::Pending)
        );

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 1);
        assert_eq!(snapshot.session_closed_pending_total, 2);
        assert_eq!(registry.active_len(), 1);
    }

    /// The ABA defense the token exists for: after a request terminates and
    /// its message id is REUSED by a new registration, the OLD token must
    /// not be able to claim (or resolve) the new request.
    #[test]
    fn stale_token_cannot_claim_a_reused_message_id() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(7));
        open(&registry, &[7]);

        let old_token = registry
            .register(5, sid, 0, Duration::from_secs(3))
            .expect("first registration");
        // First life ends (responded through its own token).
        assert_eq!(registry.begin_respond(&old_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&old_token, true),
            MarkResult::Updated
        );

        // Same id, new registration, new generation.
        let new_token = registry
            .register(5, sid, 0, Duration::from_secs(3))
            .expect("second registration");

        // A stale responder replaying the old token is refused outright and
        // the NEW request stays untouched and answerable.
        assert_eq!(registry.begin_respond(&old_token), MarkResult::NotFound);
        assert_eq!(
            registry.finish_respond(&old_token, true),
            MarkResult::NotFound
        );
        assert_eq!(
            registry.get_state(sid, 5, RequestDirection::Inbound),
            Some(RequestState::Pending)
        );
        assert_eq!(registry.begin_respond(&new_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&new_token, true),
            MarkResult::Updated
        );
    }

    #[test]
    fn duplicate_register_is_rejected_within_same_session() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(9));
        open(&registry, &[9]);
        assert!(registry
            .register(77, sid, 0, Duration::from_secs(2))
            .is_some());
        assert!(registry
            .register(77, sid, 0, Duration::from_secs(2))
            .is_none());
    }

    #[test]
    fn timeout_scanner_marks_due_requests_only() {
        let registry = RequestRegistry::new_with_timing(32, Duration::from_millis(10));
        assert!(registry
            .register(1, None, 0, Duration::from_millis(15))
            .is_some());
        assert!(registry
            .register(2, None, 0, Duration::from_secs(1))
            .is_some());

        std::thread::sleep(Duration::from_millis(20));
        let mut timeout_total = 0usize;
        for _ in 0..4 {
            timeout_total += registry.scan_timeout_bucket();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(timeout_total >= 1);
        assert_eq!(registry.get_state(None, 1, RequestDirection::Inbound), None);
        assert_eq!(
            registry.get_state(None, 2, RequestDirection::Inbound),
            Some(RequestState::Pending)
        );
        assert_eq!(registry.active_len(), 1);
    }

    #[test]
    fn response_send_failure_is_counted() {
        let registry = RequestRegistry::new();
        registry.record_response_send_failed();

        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.response_send_failed_total, 1);
    }

    #[test]
    fn register_waiter_completes_with_response() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(3));
        open(&registry, &[3]);
        let mut rx = registry
            .try_register_waiter(50, sid, 0, Duration::from_secs(5))
            .expect("fresh key registers");
        assert_eq!(
            registry.get_state(sid, 50, RequestDirection::Outbound),
            Some(RequestState::Pending)
        );

        let resp = Packet::response(50, b"pong".to_vec());
        assert!(registry.complete_waiter(sid, 50, resp));
        let got = rx.try_recv().expect("response delivered to waiter");
        assert_eq!(got.message_id(), 50);
        assert_eq!(
            registry.get_state(sid, 50, RequestDirection::Outbound),
            None
        );
    }

    #[test]
    fn complete_waiter_rejects_wrong_session() {
        let registry = RequestRegistry::new();
        open(&registry, &[1]);
        let mut rx = registry
            .try_register_waiter(60, Some(SessionId(1)), 0, Duration::from_secs(5))
            .expect("fresh key registers");
        assert!(!registry.complete_waiter(
            Some(SessionId(2)),
            60,
            Packet::response(60, Vec::new())
        ));
        assert!(rx.try_recv().is_err());
        assert!(registry.complete_waiter(Some(SessionId(1)), 60, Packet::response(60, Vec::new())));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn timeout_drops_waiter() {
        let registry = RequestRegistry::new();
        let key = RequestKey::outbound(None, 70);
        let mut rx = registry
            .try_register_waiter(70, None, 0, Duration::from_secs(1))
            .expect("fresh key registers");
        assert_eq!(registry.mark_timed_out(key), MarkResult::Updated);
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn close_session_drops_waiter() {
        let registry = RequestRegistry::new();
        let sid = SessionId(88);
        open(&registry, &[88]);
        let mut rx = registry
            .try_register_waiter(80, Some(sid), 0, Duration::from_secs(5))
            .expect("fresh key registers");
        assert_eq!(registry.close_session_pending(sid), 1);
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    #[test]
    fn try_register_waiter_refuses_duplicate() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(5));
        open(&registry, &[5]);
        let _rx = registry
            .try_register_waiter(90, sid, 0, Duration::from_secs(5))
            .expect("first registers");
        // Second waiter for the same key is refused, not silently replaced.
        assert!(registry
            .try_register_waiter(90, sid, 0, Duration::from_secs(5))
            .is_err());
    }

    #[test]
    fn abort_waiter_drops_receiver() {
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(11));
        open(&registry, &[11]);
        let mut rx = registry
            .try_register_waiter(100, sid, 0, Duration::from_secs(5))
            .expect("registers");
        assert!(registry.abort_waiter(sid, 100));
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert_eq!(
            registry.get_state(sid, 100, RequestDirection::Outbound),
            None
        );
    }

    #[test]
    fn same_id_inbound_and_outbound_coexist() {
        // Peers number their own requests independently, so the same id can be
        // in flight in both directions on one session. Before direction landed
        // in the key, the inbound register was refused as a duplicate and
        // responding to it could terminate the outbound entry.
        let registry = RequestRegistry::new();
        let sid = Some(SessionId(42));
        open(&registry, &[42]);

        let mut rx = registry
            .try_register_waiter(500, sid, 0, Duration::from_secs(5))
            .expect("outbound registers");
        let inbound_token = registry
            .register(500, sid, 0, Duration::from_secs(5))
            .expect("inbound with the same id must not be refused as a duplicate");

        // Responding to the inbound request must not complete the outbound one.
        assert_eq!(registry.begin_respond(&inbound_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&inbound_token, true),
            MarkResult::Updated
        );
        assert!(
            rx.try_recv().is_err(),
            "outbound waiter must still be pending"
        );
        assert_eq!(
            registry.get_state(sid, 500, RequestDirection::Outbound),
            Some(RequestState::Pending)
        );
        assert_eq!(
            registry.get_state(sid, 500, RequestDirection::Inbound),
            None
        );

        // The peer's response then completes the outbound normally.
        assert!(registry.complete_waiter(sid, 500, Packet::response(500, b"ok".to_vec())));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn closing_session_refuses_new_registrations() {
        // A register racing close_session_pending must not leak a Pending
        // entry past the drain: once the closing marker is up, both the
        // inbound and the waiter paths refuse.
        let registry = RequestRegistry::new();
        let sid = SessionId(300);
        open(&registry, &[300]);

        assert!(registry
            .register(1, Some(sid), 0, Duration::from_secs(5))
            .is_some());
        assert_eq!(registry.close_session_pending(sid), 1);

        assert!(
            registry
                .register(2, Some(sid), 0, Duration::from_secs(5))
                .is_none(),
            "inbound register on a closing session must be refused"
        );
        assert!(
            registry
                .try_register_waiter(3, Some(sid), 0, Duration::from_secs(5))
                .is_err(),
            "waiter register on a closing session must be refused"
        );
        assert_eq!(registry.pending_count(), 0);
        assert_eq!(registry.active_len(), 0);
    }

    /// Hammer register/close from many threads and assert the invariants the
    /// interleave windows protect. Sessions enter through a sliding window that
    /// the closing threads chase, so register/close contention is sustained for
    /// the whole run instead of dying after the first close sweep. This is a
    /// violation detector, not an interleave proof (deterministic hooks / Loom
    /// are the backbone round's job): it catches Pending entries outliving
    /// their session, counter underflow, and leaked waiters or index sets.
    #[test]
    fn concurrent_register_close_holds_invariants() {
        use std::sync::atomic::AtomicBool;
        let registry = Arc::new(RequestRegistry::new());
        let stop = Arc::new(AtomicBool::new(false));
        let cursor = Arc::new(AtomicU64::new(1000)); // close frontier
        const WINDOW: u64 = 8;
        for sid in 1000..1000 + WINDOW {
            registry.open_session(SessionId(sid));
        }
        let mut handles = Vec::new();

        for t in 0..4u64 {
            let reg = registry.clone();
            let stop = stop.clone();
            let cursor = cursor.clone();
            handles.push(std::thread::spawn(move || {
                let mut id = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    // Register inside the live window [cursor, cursor+WINDOW).
                    // The tail is being closed concurrently, so registers race
                    // the drain at the frontier for the whole run.
                    let base = cursor.load(Ordering::Relaxed);
                    let sid = SessionId(base + (id as u64 + t) % WINDOW);
                    id = id.wrapping_add(1);
                    if id.is_multiple_of(2) {
                        let _ = reg.register(id, Some(sid), 0, Duration::from_secs(5));
                    } else if let Ok(_rx) =
                        reg.try_register_waiter(id, Some(sid), 0, Duration::from_secs(5))
                    {
                        if id % 4 == 1 {
                            reg.complete_waiter(Some(sid), id, Packet::response(id, Vec::new()));
                        } else {
                            reg.abort_waiter(Some(sid), id);
                        }
                    }
                    assert!(
                        reg.pending_count() < u64::MAX / 2,
                        "pending counter underflowed"
                    );
                }
            }));
        }
        for _ in 0..2 {
            let reg = registry.clone();
            let stop = stop.clone();
            let cursor = cursor.clone();
            handles.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Advance the frontier: open the next window session, then
                    // close the oldest — the owner-opens/owner-closes shape of
                    // the real accept path.
                    let sid = cursor.fetch_add(1, Ordering::Relaxed);
                    reg.open_session(SessionId(sid + WINDOW));
                    reg.close_session_pending(SessionId(sid));
                    std::thread::yield_now();
                }
            }));
        }

        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().expect("no thread may panic");
        }

        // Quiesce: close every session that was ever in the window.
        let final_cursor = cursor.load(Ordering::Relaxed);
        for sid in 1000..final_cursor + WINDOW {
            registry.close_session_pending(SessionId(sid));
        }
        assert_eq!(
            registry.active_len(),
            0,
            "entries leaked past session close"
        );
        assert_eq!(registry.pending_count(), 0, "pending counter out of sync");
        // Same-module access: check the maps the public counters cannot see.
        assert!(registry.waiters.is_empty(), "orphaned waiters leaked");
        assert!(registry.sessions.is_empty(), "session runtimes leaked");
    }

    #[test]
    fn unopened_or_closed_session_refuses_and_cannot_resurrect() {
        let registry = RequestRegistry::new();
        // Never opened: refused.
        assert!(registry
            .register(1, Some(SessionId(70)), 0, Duration::from_secs(5))
            .is_none());
        // Open -> works.
        registry.open_session(SessionId(70));
        assert!(registry
            .register(1, Some(SessionId(70)), 0, Duration::from_secs(5))
            .is_some());
        // Closed: refused permanently — registration cannot recreate the
        // runtime, so there is no tombstone and nothing to age out.
        registry.close_session_pending(SessionId(70));
        assert!(registry
            .register(2, Some(SessionId(70)), 0, Duration::from_secs(5))
            .is_none());
        assert!(registry
            .try_register_waiter(3, Some(SessionId(70)), 0, Duration::from_secs(5))
            .is_err());
        assert!(registry.sessions.is_empty());
    }

    #[test]
    fn abort_all_drops_every_waiter() {
        let registry = RequestRegistry::new();
        open(&registry, &[1]);
        let mut rx1 = registry
            .try_register_waiter(1, Some(SessionId(1)), 0, Duration::from_secs(5))
            .expect("registers");
        let mut rx2 = registry
            .try_register_waiter(2, None, 0, Duration::from_secs(5))
            .expect("registers");
        assert_eq!(registry.abort_all(), 2);
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());
    }
}
