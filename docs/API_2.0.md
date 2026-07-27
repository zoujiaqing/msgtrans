# msgtrans 2.0 public API surface

The **candidate** crate-root surface (`msgtrans::*`) for 2.0. **This API is NOT frozen yet** — it is still changing between alphas; the snapshot gate exists to make every change visible, not to declare it final. Types behind `#[cfg(feature)]` are noted. Everything not listed here is
crate-internal: the implementation modules (transport::*, adapters::*) are
`pub(crate)`, so paths like `msgtrans::transport::request_registry::*` do not
resolve — the public API is exactly the crate root plus the feature-gated
protocol configs.

## Core types

- `SessionId`, `PacketId`, `Result<T>` (alias for `Result<T, TransportError>`)
- `Packet`, `PacketType`, `CompressionType`, `PacketError`, `FramePolicy`,
  `ReservedFlags`, `DecodeLimits`, `DEFAULT_MAX_FRAME_SIZE`
- `TransportError`, `CloseReason`
- `ConnectionInfo`, `ConnectionState`
- `ClientEvent` (`Message(ClientMessage)` | `Request(ClientRequest)` |
  `Connected`/`MessageSent`/`Disconnected`/`Error`), `ClientMessage` (one-way,
  cloneable data), `ClientRequest` (consuming; `respond`/`respond_detached`),
  `TransportEvent`, `RespondOutcome`
- `ClientEvents`

Everything above is reachable **only** through the crate root
(`msgtrans::TypeName`). The implementation modules — `transport`, `protocol`,
`event`, `packet`, `command`, `connection`, `stream`, `error`, `adapters` — are
`pub(crate)`; deep paths like `msgtrans::transport::Transport` or
`msgtrans::event::ClientEvent` do **not** resolve. The only public module is
`msgtrans::spi`. This is enforced by the committed `public-api.txt` snapshot
(CI `public-api` job).

## Packet API

- Constructors: `Packet::one_way/request/response/response_with_biz(message_id,
  impl Into<Bytes>)`, `Packet::new(PacketType, message_id)`
- Accessors: `message_id()`, `packet_type()`, `biz_type()`, `compression()`,
  `version()`, `reserved()`, `payload() -> &Bytes`, `into_payload() -> Bytes`,
  `ext_header() -> &[u8]`, `payload_len()`, `ext_header_len()`, `total_len()`
- Mutators: `set_payload/set_ext_header(impl Into<Bytes>)`, `set_biz_type`,
  `set_message_id`, `set_packet_type`, `set_compression`, `set_reserved`,
  `set_fragmented/set_priority/set_route_tag`, `compress_payload/decompress_payload`
- Codec: `try_encode() -> Result<Bytes>`; borrowed decoders `decode_one[_with]`
  and `decode_exact[_with]` (`from_bytes` == `decode_exact`, copies the body);
  owned zero-copy decoder `decode_one_from(&Bytes)` (slices the body);
  `DecodeLimits`, `DEFAULT_MAX_FRAME_SIZE`

## Server

- `TransportServerBuilder::new().protocol(cfg).limits(ServerLimits)
  .frame_policy(..).max_connections(..).actor_buffer_size(..)
  .transport_config(..).build(Arc<dyn SessionHandler>) -> TransportServer`
- `TransportServer`: `serve()`, `shutdown()`, `shutdown_with_timeout(Duration)
  -> ShutdownReport`, `session_count()`, clone (Arc-shared). Sending:
  - `send(session, bytes)` / `send_with_options(session, bytes, SendOptions)` —
    one-way, write-confirmed, `-> SendReceipt`
  - `request(session, bytes)` / `request_with_options(session, bytes,
    RequestOptions)` — `-> response Bytes`; a timeout is an `Err` carrying the
    deadline you actually asked for
  - `broadcast(bytes, SendOptions) -> Result<BroadcastReport, TransportError>`
    — the options are prepared ONCE before the fan-out, so a request the build
    cannot satisfy (e.g. compression without the codec feature) is an `Err`
    instead of being silently dropped per clone; `Ok` carries the delivered
    count + per-session failures

  None of them take a `Packet`: the transport owns message ids and packet
  types, so a caller-numbered `Request` is unrepresentable.
- `SessionHandler`: `on_message(id, Packet, SessionSender)` +
  `on_request(id, Packet, Responder)` (both required); optional `on_connected`,
  `on_disconnected`, `on_error`, `on_message_sent`
- `SessionSender` (one-way only): `send_data`,
  `send_data_with_options(data, SendOptions)` (both write-confirmed),
  `send_data_detached`, `session_id`
- `Responder` (consuming, not `Clone`): `respond(self, impl Into<Bytes>) ->
  Result<RespondOutcome>`, `respond_detached(self, impl Into<Bytes>)`,
  `session_id`, `message_id`
- `ShutdownReport { sessions_closed, sessions_remaining, infra_stopped,
  permits_restored, clean, elapsed }`

## Client

- `TransportClientBuilder::new().protocol(cfg).limits(ClientLimits)
  .transport_config(..).retry_strategy(..).build() -> TransportClient`
- `TransportClient`: `connect()`, `shutdown()`, `send` (write-confirmed ->
  `SendReceipt`), `send_detached` (queued only), `request` (-> response
  `Bytes`; a timeout is an `Err`), `request_with_options`, `send_with_options`,
  `events()`, `is_connected()`, `current_session_id()`, `disconnect()`
- `RetryConfig` (+ `exponential_backoff`)

## Limits

- `ServerLimits::new().write_deadline(Duration).pipe_capacity(usize)
  .outbound_queue_capacity(usize).mailbox_capacity(usize)
  .max_payload_size(usize).max_ext_header_size(usize)`
- `ClientLimits::new().write_deadline(Duration).pipe_capacity(usize)
  .outbound_queue_capacity(usize).max_payload_size(usize)
  .max_ext_header_size(usize)`
- Both are readable back (`connection_limits()`, `ServerLimits::mailbox()`), so
  a caller can assert that what it configured is what the connection enforces
- Frame caps apply to EVERY protocol: `max_frame_size` is derived from the
  payload and ext-header caps, and TCP / WebSocket / QUIC all enforce the same
  values instead of their own constants
- `ConnectionLimits` (SPI parameter; accessor-only)

## Protocol configs (feature-gated)

- *(tcp)* `TcpServerConfig`, `TcpClientConfig`
- *(websocket)* `WebSocketServerConfig`, `WebSocketClientConfig`, `ClientTls`,
  `WS_SUBPROTOCOL_MSGTRANS`
- *(quic)* `QuicServerConfig`, `QuicClientConfig`

## Extension SPI — `msgtrans::spi` (implement a new protocol)

The whole surface an out-of-crate protocol needs, and nothing that leaks an
internal type. This is a *separate module* (`msgtrans::spi`), not the crate root.

- `Connection` — hands out its write half via `writer() -> Arc<dyn
  ConnectionWriter>`; also `close`, `session_id`/`set_session_id`,
  `connection_info`, `is_connected`, `flush`, `take_event_pipe`, and the
  **required** `set_frame_policy`
- `ConnectionWriter` — the single send method,
  `send_with_completion(&self, Packet, WriteCompletion)`. It is separate from
  `Connection` so the transport can clone it under its connection lock and
  release the lock BEFORE awaiting the enqueue
- `WriteCompletion`, `Server`
- `EventSink` — typed by plane, so data can't be routed onto the droppable
  channel and a close can't queue behind data: `message(Packet)` /
  `error(TransportError)` (data, backpressured), `message_sent(id)`
  (droppable diagnostic), `close(reason)` (control). Paired with
  `ConnectionEvents` via `event_channel(NonZeroUsize)`
- ONE object-safe config trait set: `DynProtocolConfig` +
  `DynServerConfig`/`DynClientConfig`, taking `ConnectionLimits`, plus
  `ConfigError`. There is no public generic variant — the generic
  `ProtocolConfig`/`ServerConfig`/`ClientConfig` are crate-private so no
  internal adapter type appears in the public contract
- Re-exported building blocks: `Packet`, `TransportEvent`, `SessionId`,
  `ConnectionInfo`, `CloseReason`, `TransportError`, `ConnectionLimits`

## Cargo features

`default = ["tcp", "websocket", "quic"]`; each protocol gates its adapter,
config, events and dependencies. `flate2`/`zstd` gate payload compression, which is
requested per send through `SendOptions::compression` (and its request /
response counterparts `RequestOptions::compression` and
`Responder::respond_with_options` / `ClientRequest::respond_with_options`). The
transport applies it once, after the body is in place, and a failure — including
"the codec feature is not compiled in" — **fails the send**. Inbound
decompression happens once at the shared event-pipe boundary, so every consumer
sees plaintext with a self-consistent header. (`TransportOptions::compression`
is gone: it stamped the header before compressing and, on a build without the
codec feature, shipped a raw payload under a "compressed" header.)
