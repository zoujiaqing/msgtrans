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
///
/// This is THE default: `TransportServerBuilder` uses it whether or not the
/// caller passes [`ServerLimits`]. It used to disagree with the builder's own
/// fallback (2048 here vs 512 there), so a server built without explicit
/// limits did not get the documented default.
///
/// 512 is the load-tested value and is deliberately SMALLER than
/// [`DEFAULT_OUTBOUND_CAPACITY`]: the mailbox is a fast-draining hop in front
/// of the adapter's outbound queue, not the main buffer — under load every
/// queue-full error came from the outbound queue, never from here.
pub const DEFAULT_MAILBOX_CAPACITY: usize = 512;

/// Default maximum decoded payload per frame.
///
/// One number, enforced identically by every adapter. It used to be three:
/// TCP capped payloads at 1 MiB (hardcoded, unconfigurable), while WebSocket
/// and QUIC allowed 16 MiB — so the same message succeeded on one protocol and
/// killed the connection on another. It matches the decompression-bomb cap, so
/// a frame that decodes cannot exceed what decompression will accept either.
///
/// Memory cost is bounded by what a peer actually sends: buffers grow with the
/// bytes received, they are not preallocated per connection. A deployment that
/// wants a tighter bound sets [`ServerLimits::max_payload_size`].
pub const DEFAULT_MAX_PAYLOAD_SIZE: usize = crate::packet::DEFAULT_MAX_FRAME_SIZE;

/// Default maximum ext-header per frame: the wire field is a `u16`, so this is
/// the largest value that can be expressed at all.
pub const DEFAULT_MAX_EXT_HEADER_SIZE: usize = u16::MAX as usize;

/// The immutable per-connection limits threaded into every adapter — the SPI
/// parameter for the protocol config's server builder /
/// `build_connection`. Construct via [`ServerLimits`]/[`ClientLimits`]; read
/// the values with the accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionLimits {
    pub(crate) write_deadline: Duration,
    pub(crate) pipe_capacity: usize,
    pub(crate) outbound_capacity: usize,
    pub(crate) decode: crate::packet::DecodeLimits,
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
    /// Frame-decoding caps. An adapter MUST enforce these rather than its own
    /// constants, otherwise the same payload is accepted on one protocol and
    /// rejected on another.
    pub fn decode_limits(&self) -> crate::packet::DecodeLimits {
        self.decode
    }
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            write_deadline: DEFAULT_WRITE_DEADLINE,
            pipe_capacity: DEFAULT_PIPE_CAPACITY,
            outbound_capacity: DEFAULT_OUTBOUND_CAPACITY,
            decode: decode_limits(DEFAULT_MAX_PAYLOAD_SIZE, DEFAULT_MAX_EXT_HEADER_SIZE),
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

    /// The per-connection limits these settings produce — what every adapter
    /// actually receives. Readable so a caller can assert that what it
    /// configured is what the connection will enforce.
    pub fn connection_limits(&self) -> ConnectionLimits {
        self.connection
    }

    /// The configured per-session actor mailbox capacity.
    pub fn mailbox(&self) -> usize {
        self.mailbox_capacity
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

    /// Maximum decoded payload per frame, enforced by every protocol
    /// (clamped to [64 B, 16 MiB] — the upper bound is the decompression-bomb
    /// cap, so a frame can never be larger than decompression would accept).
    pub fn max_payload_size(mut self, size: usize) -> Self {
        self.connection.decode = decode_limits(
            clamp_payload(size),
            self.connection.decode.max_ext_header_size,
        );
        self
    }

    /// Maximum ext-header per frame (clamped to [0, 65535] — the wire field is
    /// a `u16`).
    pub fn max_ext_header_size(mut self, size: usize) -> Self {
        self.connection.decode =
            decode_limits(self.connection.decode.max_payload_size, clamp_ext(size));
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

    /// The per-connection limits these settings produce — what the adapter
    /// actually receives.
    pub fn connection_limits(&self) -> ConnectionLimits {
        self.connection
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

    /// Maximum decoded payload per frame, enforced by every protocol
    /// (clamped to [64 B, 16 MiB] — the upper bound is the decompression-bomb
    /// cap, so a frame can never be larger than decompression would accept).
    pub fn max_payload_size(mut self, size: usize) -> Self {
        self.connection.decode = decode_limits(
            clamp_payload(size),
            self.connection.decode.max_ext_header_size,
        );
        self
    }

    /// Maximum ext-header per frame (clamped to [0, 65535] — the wire field is
    /// a `u16`).
    pub fn max_ext_header_size(mut self, size: usize) -> Self {
        self.connection.decode =
            decode_limits(self.connection.decode.max_payload_size, clamp_ext(size));
        self
    }
}

/// The frame cap is DERIVED, never set independently: a `max_frame_size` that
/// disagreed with `max_payload_size + max_ext_header_size` would reject frames
/// whose parts were each individually legal.
fn decode_limits(
    max_payload_size: usize,
    max_ext_header_size: usize,
) -> crate::packet::DecodeLimits {
    crate::packet::DecodeLimits {
        max_frame_size: crate::packet::FIXED_HEADER_SIZE + max_ext_header_size + max_payload_size,
        max_payload_size,
        max_ext_header_size,
    }
}

fn clamp_payload(size: usize) -> usize {
    size.clamp(64, DEFAULT_MAX_PAYLOAD_SIZE)
}

fn clamp_ext(size: usize) -> usize {
    size.min(DEFAULT_MAX_EXT_HEADER_SIZE)
}

fn clamp_deadline(deadline: Duration) -> Duration {
    let ms = deadline.as_millis().clamp(10, 600_000) as u64;
    Duration::from_millis(ms)
}
