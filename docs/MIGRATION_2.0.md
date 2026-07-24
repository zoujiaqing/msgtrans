# Migrating from msgtrans 1.x to 2.0

2.0 is a deliberate breaking release: it freezes a small, honest public API.
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

The `transport::*` and `adapters::*` implementation submodules are now
`pub(crate)`. Reach everything through the crate root: `msgtrans::Packet`, not
`msgtrans::packet::Packet` deep paths into internals like
`msgtrans::transport::request_registry::RequestState` no longer resolve (they
were never meant to be public). The request-lifecycle state machine and the
session-actor internals are fully crate-private.

## 9. Removed public machinery

The internal performance types are no longer re-exported at the crate root:
`LockFreeCounter/HashMap/Queue`, `MemoryPool`/`MemoryStats`, `ProtocolStats`,
`RequestManager`, `FlumePoweredProtocolAdapter`. They were implementation
detail, not API.
