//! Per-connection resource limits.
//!
//! 2.0: the write deadline, event-pipe capacity and outbound-queue capacity
//! were process-global atomics with `#[doc(hidden)]` public setters — a hidden
//! way to change behavior for the whole process. They are now immutable,
//! per-connection values carried from the builder to each adapter through
//! [`ConnectionLimits`], surfaced publicly as [`ServerLimits`]/[`ClientLimits`]
//! with PRIVATE fields (so adding a knob later cannot break a struct literal).

use std::time::Duration;

/// Default write deadline: a single socket write may take this long before the
/// connection is declared dead.
pub const DEFAULT_WRITE_DEADLINE: Duration = Duration::from_secs(30);
/// Default event-pipe (data-plane) capacity per connection.
pub const DEFAULT_PIPE_CAPACITY: usize = 8192;
/// Default outbound-queue capacity per connection.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 2048;
/// Default actor mailbox capacity (server-side, per session).
pub const DEFAULT_MAILBOX_CAPACITY: usize = 2048;

/// The immutable per-connection limits threaded into every adapter — the SPI
/// parameter for the protocol config's server builder /
/// `build_connection`. Construct via [`ServerLimits`]/[`ClientLimits`]; read
/// the values with the accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionLimits {
    pub(crate) write_deadline: Duration,
    pub(crate) pipe_capacity: usize,
    pub(crate) outbound_capacity: usize,
}

impl ConnectionLimits {
    /// Maximum duration of a single socket write.
    pub fn write_deadline(&self) -> Duration {
        self.write_deadline
    }
    /// Data-plane event-pipe capacity.
    pub fn pipe_capacity(&self) -> usize {
        self.pipe_capacity
    }
    /// Outbound-queue capacity.
    pub fn outbound_capacity(&self) -> usize {
        self.outbound_capacity
    }
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            write_deadline: DEFAULT_WRITE_DEADLINE,
            pipe_capacity: DEFAULT_PIPE_CAPACITY,
            outbound_capacity: DEFAULT_OUTBOUND_CAPACITY,
        }
    }
}

/// Server-side resource limits, applied to every accepted connection and its
/// per-session actor. Private fields: build with the setters, not a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    pub(crate) connection: ConnectionLimits,
    pub(crate) mailbox_capacity: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            connection: ConnectionLimits::default(),
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
        }
    }
}

impl ServerLimits {
    /// Start from the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum duration of a single socket write before the connection is
    /// declared dead (clamped to [10ms, 10min]).
    pub fn write_deadline(mut self, deadline: Duration) -> Self {
        self.connection.write_deadline = clamp_deadline(deadline);
        self
    }

    /// Data-plane event-pipe capacity per connection (clamped to [1, 65536]).
    pub fn pipe_capacity(mut self, capacity: usize) -> Self {
        self.connection.pipe_capacity = capacity.clamp(1, 65536);
        self
    }

    /// Outbound-queue capacity per connection (clamped to [1, 65536]).
    pub fn outbound_queue_capacity(mut self, capacity: usize) -> Self {
        self.connection.outbound_capacity = capacity.clamp(1, 65536);
        self
    }

    /// Per-session actor mailbox capacity (clamped to at least 1).
    pub fn mailbox_capacity(mut self, capacity: usize) -> Self {
        self.mailbox_capacity = capacity.max(1);
        self
    }
}

/// Client-side resource limits, applied to the single connection. Private
/// fields: build with the setters, not a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientLimits {
    pub(crate) connection: ConnectionLimits,
}

impl ClientLimits {
    /// Start from the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum duration of a single socket write before the connection is
    /// declared dead (clamped to [10ms, 10min]).
    pub fn write_deadline(mut self, deadline: Duration) -> Self {
        self.connection.write_deadline = clamp_deadline(deadline);
        self
    }

    /// Data-plane event-pipe capacity (clamped to [1, 65536]).
    pub fn pipe_capacity(mut self, capacity: usize) -> Self {
        self.connection.pipe_capacity = capacity.clamp(1, 65536);
        self
    }

    /// Outbound-queue capacity (clamped to [1, 65536]).
    pub fn outbound_queue_capacity(mut self, capacity: usize) -> Self {
        self.connection.outbound_capacity = capacity.clamp(1, 65536);
        self
    }
}

fn clamp_deadline(deadline: Duration) -> Duration {
    let ms = deadline.as_millis().clamp(10, 600_000) as u64;
    Duration::from_millis(ms)
}
