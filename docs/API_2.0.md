# msgtrans 2.0 public API surface

The frozen crate-root surface (`msgtrans::*`). Types behind `#[cfg(feature)]` are noted. Everything not listed here is
crate-internal: the implementation modules (transport::*, adapters::*) are
`pub(crate)`, so paths like `msgtrans::transport::request_registry::*` do not
resolve — the public API is exactly the crate root plus the feature-gated
protocol configs.

## Core types

- `SessionId`, `PacketId`, `Result<T>` (alias for `Result<T, TransportError>`)
- `Packet`, `PacketType`, `PacketError`, `FramePolicy`
- `TransportError`, `CloseReason`
- `ConnectionInfo`, `TransportCommand`, `TransportStats`
- `ClientEvent`, `TransportEvent`, `RespondOutcome`
- `TcpEvent` *(feature = "tcp")*, `WebSocketEvent` *(feature = "websocket")*,
  `QuicEvent` *(feature = "quic")*
- `ClientEvents`

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
  -> ShutdownReport`, `send(session, bytes)`, `request(session, bytes)`,
  `session_count()`, clone (Arc-shared)
- `SessionHandler`: `on_message(id, Packet, SessionSender)` +
  `on_request(id, Packet, Responder)` (both required); optional `on_connected`,
  `on_disconnected`, `on_error`, `on_message_sent`
- `SessionSender`: `send`, `send_data`, `session_id`
- `Responder` (consuming, not `Clone`): `respond(self, impl Into<Bytes>) ->
  Result<RespondOutcome>`, `respond_detached(self, impl Into<Bytes>)`,
  `session_id`, `message_id`
- `ShutdownReport { sessions_closed, sessions_remaining, infra_stopped,
  permits_restored, clean, elapsed }`

## Client

- `TransportClientBuilder::new().protocol(cfg).limits(ClientLimits)
  .transport_config(..).retry_strategy(..).build() -> TransportClient`
- `TransportClient`: `connect()`, `shutdown()`, `send`, `request`,
  `request_with_options`, `send_with_options`, `events()`, `is_connected()`,
  `current_session_id()`, `disconnect()`
- `RetryConfig` (+ `exponential_backoff`)

## Limits

- `ServerLimits::new().write_deadline(Duration).pipe_capacity(usize)
  .outbound_queue_capacity(usize).mailbox_capacity(usize)`
- `ClientLimits::new().write_deadline(Duration).pipe_capacity(usize)
  .outbound_queue_capacity(usize)`
- `ConnectionLimits` (SPI parameter; accessor-only)

## Protocol configs (feature-gated)

- *(tcp)* `TcpServerConfig`, `TcpClientConfig`
- *(websocket)* `WebSocketServerConfig`, `WebSocketClientConfig`, `ClientTls`,
  `WS_SUBPROTOCOL_MSGTRANS`
- *(quic)* `QuicServerConfig`, `QuicClientConfig`

## Extension SPI (implement a new protocol)

- `Connection` (single send method: `send_with_completion(Packet,
  WriteCompletion)`), `WriteCompletion`, `Server`, `ConnectionFactory`
- `ServerConfig` / `ClientConfig` (+ object-safe `DynServerConfig` /
  `DynClientConfig`), taking `ConnectionLimits`
- `TransportConfig`, `TransportContext`, `Transport`

## Cargo features

`default = ["tcp", "websocket", "quic"]`; each protocol gates its adapter,
config, events and dependencies. `flate2`/`zstd` gate payload compression.
