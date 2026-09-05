# Migrating from msgtrans 1.x to 2.0

2.0 is a deliberate breaking release, aiming at a small, honest public API.
**The surface is frozen as of 2.0.0-beta.1** — it changes before 2.0.0 only to fix a defect, and any such change is called out in the release notes.
Every change below is a compile error you fix once, not a silent behavior
drift. The wire format is **unchanged** (byte-identical fixtures), so 1.x and
2.0 peers interoperate.

## 1. Server handlers: `on_message` split into `on_message` + `on_request`

`SessionHandler` no longer routes requests through `on_message`. Requests are
delivered to a new required method `on_request`, which receives a consuming
`Responder` — the only way to answer, and the only thing that reports whether
the answer was actually written.

```rust
// 1.x
impl SessionHandler for H {
    async fn on_message(&self, id: SessionId, p: Packet, tx: SessionSender) {
        if p.header.packet_type == PacketType::Request {
            let _ = tx.respond(p.header.message_id, p.header.biz_type, p.payload).await;
        } else {
            let _ = tx.send_data(p.payload).await;
        }
    }
}

// 2.0
impl SessionHandler for H {
    async fn on_message(&self, id: SessionId, p: Packet, tx: SessionSender) {
        let _ = tx.send_data(p.into_payload()).await; // one-way traffic only
    }
    async fn on_request(&self, id: SessionId, req: Packet, responder: Responder) {
        // respond(self).await is write-confirmed: Ok(Written) means the bytes
        // were written; a duplicate/late request yields Ok(AlreadyHandled).
        let _ = responder.respond(req.into_payload()).await;
        // Fire-and-forget variant: responder.respond_detached(bytes);
    }
}
```

`SessionSender::respond(message_id, ..)` — which accepted an arbitrary id — is
**removed**. Responses go through the request-scoped `Responder`, which carries
an unforgeable `RequestToken` (session + direction + id + registration
generation), closing the message-ID reuse (ABA) window by construction.

## 2. `Packet` fields are private; encoding is fallible

`Packet.{header, ext_header, payload}` are private. Use accessors:

| 1.x field access            | 2.0 accessor                    |
|-----------------------------|---------------------------------|
| `p.header.message_id`       | `p.message_id()`                |
| `p.header.packet_type`      | `p.packet_type()`               |
| `p.header.biz_type`         | `p.biz_type()`                  |
| `p.header.compression`      | `p.compression()`               |
| `p.header.reserved`         | `p.reserved()`                  |
| `&p.payload`                | `p.payload()` (`&Bytes`)        |
| `p.payload` (move)          | `p.into_payload()` (`Bytes`)    |
| `&p.ext_header`             | `p.ext_header()` (`&[u8]`)      |
| `p.to_bytes()`              | `p.try_encode()?` (`Result`)    |

Encoding is the single fallible entry point `Packet::try_encode() ->
Result<Bytes, PacketError>`. An ext header over `u16::MAX` or a payload over
`u32::MAX` is a `FrameTooLarge` error instead of the 1.x silent truncation that
produced a self-inconsistent frame. `to_bytes()`/`encode_to_vec()` are removed.

`ext_header` is now `Bytes`; `set_ext_header` takes `impl Into<Bytes>`.
`FixedHeader` is no longer public (its length fields are derived at encode).

## 3. Strict codec by default

- `FramePolicy::default()` is now `Strict` (was `Lenient`). Undecodable frames
  — including WebSocket **text** frames — close the connection. Opt back into
  the 1.x wrap-as-raw-oneway behavior per connection with `FramePolicy::Lenient`.
- `PacketType`/`CompressionType` decode via `TryFrom<u8>`: an unknown wire byte
  is a protocol error, not a silent `OneWay`/`None`.
- New stream/framed decode API: `Packet::decode_one(bytes)` (byte streams,
  returns the consumed length) and `Packet::decode_exact(bytes)` (framed
  transports, rejects trailing bytes). `from_bytes` == `decode_exact`.

## 4. Single send SPI on `Connection`

Adapters implement ONE send method:

```rust
async fn send_with_completion(&mut self, packet: Packet, completion: WriteCompletion)
    -> Result<(), TransportError>;
```

The fire-and-forget tier is `WriteCompletion::detached()`. There is no parallel
`send()`. The completion is resolved by the write loop with the real write
result (or dropped, which reports failure); never invent an `Ok`.

## 5. One protocol construction SPI

The string-uri factory track is removed: `ProtocolFactory`, `ProtocolRegistry`,
`ProtocolSet`, `StandardProtocols`, `PluginManager`, `create_standard_registry`
and the `Tcp/WebSocket/QuicFactory` units are gone. Construct via the
config-driven path only:

```rust
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("127.0.0.1:8001")?)
    .build(Arc::new(handler)).await?;
```

`ProtocolConfig::merge()` is removed — build the config you want with the
builder methods.

## 6. `ServerLimits` / `ClientLimits` replace the global limit setters

The process-global `set_write_deadline` / `set_default_pipe_capacity` hooks are
removed. Per-connection resource limits are immutable and set on the builder:

```rust
let server = TransportServerBuilder::new()
    .limits(ServerLimits::new()
        .write_deadline(Duration::from_secs(10))
        .pipe_capacity(4096)
        .outbound_queue_capacity(2048)
        .mailbox_capacity(1024))
    .protocol(cfg)
    .build(handler).await?;

let mut client = TransportClientBuilder::new()
    .limits(ClientLimits::new().write_deadline(Duration::from_secs(10)))
    .protocol(cfg)
    .build().await?;
```

## 7. WebSocket TLS: `verify_tls: bool` → `ClientTls`

```rust
// 1.x: WebSocketClientConfig::new(url)?.verify_tls(false)
// 2.0:
WebSocketClientConfig::new(url)?.tls(ClientTls::Insecure)        // dev only
WebSocketClientConfig::new(url)?.tls(ClientTls::CustomCa(pem))   // pinned CA
WebSocketClientConfig::new(url)?.tls(ClientTls::SystemRoots)     // default
```

The 1.x `verify_tls` flag was never wired (the client could not establish TLS).
`ClientTls` variants are all real; TLS is consulted only for `wss://`.

## 8. Sealed implementation modules

**Every** implementation module is now `pub(crate)`: `transport`, `protocol`,
`event`, `packet`, `command`, `connection`, `stream`, `error`, `adapters`.
Reach every public type through the crate root — `msgtrans::Packet`,
`msgtrans::ClientEvent`, `msgtrans::Responder` — not the old deep paths
(`msgtrans::packet::Packet`, `msgtrans::transport::Responder`,
`msgtrans::event::ClientEvent`), which no longer resolve. The only public module
is `msgtrans::spi` (the protocol-extension SPI). The internal `Transport`
per-connection type, `TaggedClientEvent`, and `GlobalConfig` are no longer
reachable at all.

The candidate surface is captured in `public-api.txt` and enforced by the CI
`public-api` job, so it can no longer drift silently.

## 9. Removed public machinery

The following were implementation detail or dead tracks, not API, and are gone:

- Performance internals: `LockFreeCounter/HashMap/Queue`, `MemoryPool`/
  `MemoryStats`, `ProtocolStats`, `RequestManager`, `FlumePoweredProtocolAdapter`.
- The legacy command track: `TransportCommand`, `ConfigUpdate`, `TransportStats`,
  `ProtocolCommand`, `CommandBuilder`, `CommandExecutor`.
- Orphaned event machinery with no producer: the `TcpEvent`/`WebSocketEvent`/
  `QuicEvent` enums, the `ProtocolEvent` trait, and `ConnectionEvent`. Client
  code observes `ClientEvent`; internal plumbing uses `TransportEvent`.
- Unread instrumentation: `AdapterStats`, `ErrorStats`, `TransportError::severity`
  / `ErrorSeverity`, and the read-buffer pool's per-op statistics.
- The `MessageIdManager` and the old cloneable `Message` struct (superseded by
  `ClientMessage`/`ClientRequest`).

## 10. Send / request results are typed and honest

`TransportResult` and `TransportStatus` are **removed**. They could encode
impossible combinations, and — worse — a request **timeout** was reported as
`Ok(TransportResult { status: Timeout, data: None })`, so a caller who checked
only `is_ok()` treated a request that never got an answer as a success.

```rust
// 1.x
let result = client.request(b"ping").await?;
if let Some(data) = result.data { /* ... */ }   // silently also the timeout path

// 2.0
let response: Bytes = client.request(b"ping").await?;  // timeout => Err
```

`send` is now **write-confirmed** on both client and server: it returns only
once the bytes reached the socket, and yields a typed `SendReceipt`. The
fire-and-forget tier is explicitly named:

```rust
let receipt = client.send(b"hello").await?;          // confirmed written
let receipt = client.send_detached(b"hello").await?; // queued only
```

`SessionHandler::on_message_sent` is documented as **droppable** (it rides the
diagnostic plane): never use it as a delivery ledger.

## 11. `ConnectionInfo` contains only real data

`last_activity`, `packets_sent`, `packets_received`, `bytes_sent` and
`bytes_received` are **removed**: no adapter ever updated them, so reading them
returned a constant zero that looked like a live counter. The addresses are now
real on every protocol (WebSocket used to report `0.0.0.0:0`; QUIC discarded the
addresses it had already resolved).

`SessionHandler::on_connected` is a notification, not an admission decision —
it returns `()`. To reject a peer, close its session explicitly.

## 12. Adapters hand out a `ConnectionWriter`

`Connection::send_with_completion` is replaced by
`Connection::writer() -> Arc<dyn ConnectionWriter>`, and the send method moves
onto `ConnectionWriter` (taking `&self`):

```rust
// 2.0
fn writer(&self) -> Arc<dyn ConnectionWriter> {
    Arc::new(MyWriter { queue: self.send_queue.clone() })
}
```

This exists so the transport can clone the writer under its connection lock and
**release the lock before awaiting the enqueue** — previously a saturated
outbound queue parked reconnect/close/shutdown behind every waiting sender.

`Connection::set_frame_policy` is now a **required** method (the old no-op
default let an adapter silently ignore `Strict`).

## 13. One object-safe protocol config trait set

The generic `ServerConfig`/`ClientConfig` traits are crate-private: their
associated types named the concrete private adapters, which wrote internal
types into the public API. Implement the object-safe set only —
`DynProtocolConfig` + `DynServerConfig`/`DynClientConfig`. The dead
`as_any`, `clone_dyn` and `get_target_info` methods are gone.

`EventSink` is typed by plane: `message(Packet)`/`error(TransportError)` (data,
backpressured), `message_sent(id)` (droppable diagnostic), `close(reason)`
(control). `event_channel` takes a `NonZeroUsize`.

## 14. Message ids are always allocated by the transport

`TransportOptions::message_id` is removed. A caller-chosen id could be reused
while a previous request's response was still in flight, and the peer echoes the
id with no generation, so a late old response could be handed to the new caller.
Request ids are allocated **per session**, are strictly monotonic, and **refuse
to wrap**: the counter saturates and the request fails with a resource error
rather than restarting the id space under still-live requests (a reconnect gets
a fresh session with a fresh counter). One-way messages use a separate counter
that may wrap harmlessly, since nothing matches on their ids.
(`SessionSender::send_data` likewise stopped stamping every one-way message with
the fixed id 0.)

## 15. Extensible enums are `#[non_exhaustive]`

`TransportError`, `CloseReason`, `ClientEvent`, `RespondOutcome`,
`ConnectionState` and the output-only `ShutdownReport`/`ConnectionInfo` are
`#[non_exhaustive]`, so future variants and fields are additive. Add a `_ => {}`
arm to exhaustive matches.

## 16. `send*` is confirmed everywhere; `broadcast` reports

Every public `send*` is now write-confirmed, including `SessionSender::send_data`
and `send_with_options`. The fire-and-forget tier is explicitly named:
`TransportClient::send_detached`, `SessionSender::send_data_detached`.

`TransportServer::broadcast` returned `Ok(())` even when every send failed. It
now returns a `BroadcastReport`, wrapped in a `Result` because the send options
are prepared once **before** the fan-out — a request this build cannot satisfy
(compression without the codec feature) fails the whole call rather than being
dropped silently on each clone:

```rust
let report = server.broadcast(bytes, SendOptions::new().biz_type(7)).await?;
if !report.is_complete() {
    warn!("broadcast reached {} sessions, {} failed", report.delivered, report.failed_count());
}
```

## 17. `TransportOptions::compression` is removed

It set the compressed flag on the header and then compressed; a compression
failure only logged a warning and sent the RAW payload under a "compressed"
header. On a default build (no `flate2`/`zstd` feature) that failure was
guaranteed. Compression is a send OPTION again (see section 22), but performed by the
transport so the header and body can never disagree:

```rust
let options = SendOptions::new().compression(CompressionType::Zstd);
```

## 18. Dead events removed; `Connected` actually fires

`TransportEvent::{ServerStarted, ServerStopped, ClientConnected,
ClientDisconnected}` are deleted — nothing ever produced them.

`ClientEvent::Connected` had the same problem (nothing produced
`ConnectionEstablished`), so code waiting for it waited forever. It is now
**emitted on every connection**, including reconnects, carrying the real
`ConnectionInfo`.

## 19. Double `connect()` is rejected

Calling `connect()` on an already-connected client returns an error instead of
silently replacing the live connection (which orphaned the old socket's session
while the caller believed it still had one). Call `disconnect()` first.

## 20. The SPI lives only in `msgtrans::spi`

`Connection`, `ConnectionWriter`, `Server`, `WriteCompletion` and the `Dyn*`
config traits are no longer re-exported from the crate root — one type, one
public path. Application code uses the crate root; protocol implementors use
`msgtrans::spi`. The generic `ProtocolConfig` is crate-private too, so the
object-safe set is the only trait set to implement.

## 21. Raw-packet sends are gone; options are split

No public send API takes a `Packet` any more — that is what makes a
caller-numbered `Request` unrepresentable (a raw `Request` could take an id the
registry was about to allocate, and its response then completed the wrong
waiter).

| 1.x / early 2.0 alpha | 2.0 |
|---|---|
| `server.send_to_session(id, packet)` | `server.send_with_options(id, bytes, SendOptions::new().biz_type(b))` |
| `server.broadcast(packet)` | `server.broadcast(bytes, SendOptions)` -> `Result<BroadcastReport, _>` |
| `sender.send(packet)` | `sender.send_data_with_options(bytes, SendOptions)` |
| `sender.send_detached(packet)` | `sender.send_data_detached(bytes)` |

`TransportOptions` is split so a one-way send cannot carry a response deadline:

```rust
use msgtrans::{SendOptions, RequestOptions, CompressionType};
use std::time::Duration;

let send = SendOptions::new().biz_type(7).compression(CompressionType::Zstd);
let request = RequestOptions::new().biz_type(7).timeout(Duration::from_millis(500));
```

## 22. Compression is a send option again, performed by the transport

`SendOptions::compression`/`RequestOptions::compression` compress the payload
**inside** the transport, after the body is in place. If the codec feature is
not compiled in, the send returns an error — it never ships a raw payload under
a "compressed" header, which is what the old `TransportOptions::compression`
did.

Inbound, decompression happens once at the shared event-pipe boundary, so the
client's `ClientMessage`/`ClientRequest`, the server's `SessionHandler` and any
custom adapter's consumer all receive PLAINTEXT with a self-consistent header.
An undecodable body closes the connection with `CloseReason::Error`.
