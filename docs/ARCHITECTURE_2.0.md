# msgtrans 2.0 architecture

An async multi-protocol transport for Rust: one API over TCP / WebSocket / QUIC,
with protocols plugged in through an object-safe SPI rather than hardcoded.

This document describes what the code **actually does**. The 1.0.7 spec is kept
at [`archive/ARCHITECTURE_SPEC_1.0.7.zh-CN.md`](archive/ARCHITECTURE_SPEC_1.0.7.zh-CN.md)
and does not apply to 2.0.

## 1. Layers

```text
  application
      │  TransportClient / TransportServer + SessionHandler
      ▼
  transport            session lifecycle, request registry, generations
      │  Connection / ConnectionWriter / EventSink   (msgtrans::spi)
      ▼
  adapters             tcp · websocket · quic  (or yours)
      │
      ▼
  socket
```

Only two things are public: the **crate root** (the application API) and
**`msgtrans::spi`** (the protocol-implementor API). Everything else —
`transport::*`, `adapters::*`, the request state machine, the session actor —
is `pub(crate)`, and a CI snapshot (`public-api.txt`) fails the build on any
unrecorded change to that surface.

## 2. Per-connection actor, no event bus

Every connection is driven by its own actor with a **bounded** mailbox:

```text
Connection → flume(bounded) → SessionActor → SessionHandler
```

A handler that falls behind backpressures **its own** connection only. There is
deliberately no fan-out bus: under one, a single slow subscriber caused message
loss for everyone (measured at ~91% loss at saturation, which is what motivated
the rework).

### Event planes

A connection's events travel on three planes with different guarantees, and the
`EventSink` API is typed per plane so an adapter cannot mix them up:

| Plane | Method | Guarantee |
|---|---|---|
| Data | `message(Packet)`, `error(TransportError)` | Bounded, backpressured, never dropped |
| Diagnostic | `message_sent(id)` | **Droppable** under load, so confirmations can never displace data |
| Control | `close(reason)` | Never blocks, never queues behind data; exactly one `ConnectionClosed` ends the stream |

## 3. Sending: one path, honest results

Adapters expose their write half separately from the connection object:

```rust
fn writer(&self) -> Arc<dyn ConnectionWriter>;      // Connection
async fn send_with_completion(&self, Packet, WriteCompletion);  // ConnectionWriter
```

The transport clones the writer **under** its connection lock and releases the
lock **before** awaiting the enqueue. Without that split, a saturated outbound
queue parked reconnect/close/shutdown behind every waiting sender.

`WriteCompletion` is the single ownership handle for "what happened to this
packet". It is resolved by the write loop with the real result, or **dropped** —
and dropping reports failure by construction. So:

- Every public `send*` is **write-confirmed**: `Ok` means the bytes reached the
  socket. The fire-and-forget tier is explicitly named `*_detached`.
- `request` returns the response `Bytes`; every failure, **including a timeout**,
  is an `Err`.
- `broadcast` returns a `BroadcastReport` (delivered count + per-session
  failures), not a bare `Ok(())`.

## 4. Request lifecycle

One registry is the sole arbiter of "exactly one response per request".

**States**: `Pending → Responding → Responded | SendFailed`, plus terminal
`TimedOut` / `SessionClosed` / `Dropped`. Transitions are CAS'd, so exactly one
responder wins.

**`RequestToken`** — an unforgeable handle to ONE registration: session,
direction, id **and** a registration generation. It is the only way to respond,
and the generation check refuses a token minted for an earlier life of a reused
id.

**`RespondClaim`** — created in the same poll that wins the claim and moved into
the queued write. Cancelling the caller's future therefore cannot strand
registry state: whoever owns the claim resolves it, and dropping it records a
send failure.

**Cleanup is deterministic, not scanner-dependent.** `Responder` and
`ClientRequest` resolve an unanswered request on `Drop`. The timeout scanner
only reaps a request that is *held* past its deadline.

### Request ids never repeat within a session

Responses are matched on the wire by `(session_id, message_id)` — the 16-byte
header has no room for a generation. So the id counter is **per session** and
**refuses to wrap**: it saturates and reports exhaustion rather than restarting
the space under live requests. A reconnect gets a fresh session with a fresh
counter. One-way messages use a separate, freely-wrapping counter (nothing
matches on them).

An inbound request that cannot be registered (peer reused a live id, or the
session is closing) is **dropped, never delivered** — delivering it would let
the respond path bypass the registry and emit a second response.

## 5. Generations

Each installed connection gets a monotonically increasing `SessionId`. The
connection and its generation live in **one slot under one lock**, so validating
the generation and taking the writer is a single atomic step. A response built
against generation N is enqueued on generation N's queue or not at all — never
onto the replacement connection after a reconnect.

## 6. Shutdown

`shutdown` is awaitable and **proves** what it did via `ShutdownReport`
(`sessions_closed`, `sessions_remaining`, `infra_stopped`, `permits_restored`,
`clean`, `elapsed`). Task ownership never enters a cancellable public future:
callers observe a watch and join owned handles, so cancelling `shutdown()` loses
nothing. No background task — including the client's fallback request scanner —
outlives it.

## 7. Framing

`FramePolicy::Strict` is the default: an undecodable frame closes the
connection. `Lenient` restores the 1.x behavior of downgrading it to a raw
one-way message. `Connection::set_frame_policy` is a **required** SPI method —
a silent no-op default let adapters ignore `Strict`.

`PacketType`/`CompressionType` decode via `TryFrom<u8>`, so an unknown wire byte
is a protocol error rather than a silent default. Encoding is the single
fallible entry point `try_encode()`; oversized ext headers/payloads are an error
instead of 1.x's silent truncation into a self-inconsistent frame.

Compression is a **send option** (`SendOptions::compression`) applied by the
transport after the body is in place; a failure (e.g. the codec feature is not
compiled in) fails the send instead of shipping a raw payload under a
"compressed" header, which is what the old `TransportOptions::compression` did.

Inbound decompression happens **once**, at the shared event-pipe boundary that
every adapter pushes through, so the client, the server's `SessionHandler` and
any custom adapter's consumer all see plaintext with a self-consistent header.
An undecodable body closes the connection.

## 8. Resource limits

Per-connection limits are immutable and set on the builder
(`ServerLimits`/`ClientLimits` → `ConnectionLimits`): write deadline, event-pipe
capacity, outbound-queue capacity, actor mailbox size. The process-global
setters of 1.x are gone. `max_connections` is enforced by a semaphore whose
permit lives in the session's actor, so capacity is released exactly when the
actor ends.

## 9. Wire format

Unchanged from 1.x and pinned by byte-identical fixtures, so 1.x and 2.0 peers
interoperate. See [`WIRE_FORMAT.md`](WIRE_FORMAT.md).
