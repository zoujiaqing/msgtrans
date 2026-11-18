# 🚀 MsgTrans - Modern Multi-Protocol Communication Framework

[![Rust](https://img.shields.io/badge/rust-1.88+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

**English** | [简体中文](https://github.com/zoujiaqing/msgtrans/blob/main/README.zh-Hans.md) | [繁體中文](https://github.com/zoujiaqing/msgtrans/blob/main/README.zh-Hant.md) | [Français](https://github.com/zoujiaqing/msgtrans/blob/main/README.fr-FR.md) | [Deutsch](https://github.com/zoujiaqing/msgtrans/blob/main/README.de.md) | [Español](https://github.com/zoujiaqing/msgtrans/blob/main/README.es.md) | [Українська](https://github.com/zoujiaqing/msgtrans/blob/main/README.uk.md) | [Русский](https://github.com/zoujiaqing/msgtrans/blob/main/README.ru.md) | [日本語](https://github.com/zoujiaqing/msgtrans/blob/main/README.ja-JP.md) | [한국어](https://github.com/zoujiaqing/msgtrans/blob/main/README.ko-KR.md) | [Tiếng Việt](https://github.com/zoujiaqing/msgtrans/blob/main/README.vi-VN.md)

> **Modern multi-protocol communication framework with a unified interface over TCP, WebSocket and QUIC**

## 🌟 Core Features

### 🏗️ Unified Architecture

- **Three-layer architecture**: Application → Transport → Protocol, with clear separation
- **Protocol-agnostic business logic**: one codebase, multi-protocol deployment
- **Configuration-driven**: switch protocols through configuration without changing business logic
- **Pluggable adapters**: implement the `Connection` trait to add a new protocol

### ⚡ Modern Concurrency

- **Per-session actors**: session handlers run independently; a slow callback can delay work on its own session
- **Shared payload buffers**: `Packet` carries a `Bytes` payload, handed straight to the wire where possible
- **Event-driven model**: fully asynchronous, non-blocking event handling
- **Bounded backpressure**: outbound queues are bounded per connection so a slow peer cannot exhaust memory or stall a fan-out loop

### 🔌 Protocols

- **TCP**: direct connections for native clients and services, with TLS when needed.
- **WebSocket**: browsers and HTTP gateways that support WebSocket Upgrade; use WSS in production.
- **QUIC**: native clients on variable networks where UDP is available. With `msgtrans/2` ALPN, the server can send first without an extra application round trip.
- **Custom protocols** - implement the `Connection` trait

### 🎯 Minimalist API

- **Builder pattern**: fluent, readable configuration
- **Type safety**: compile-time checked configuration
- **Sensible defaults**: works out of the box, tune only when needed

## 🚀 Quick Start

### Installation

The Rust crate requires Rust 1.88 or newer. To run the server example below, also add `tokio` and `async-trait` as direct dependencies.

```toml
[dependencies]
msgtrans = "2.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
```

### Create a Multi-Protocol Server

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    Responder, SessionHandler, SessionSender, TransportServerBuilder,
    TcpServerConfig, WebSocketServerConfig, QuicServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

// Business logic lives in a handler. Each connection gets its own actor that
// calls it, so a slow handler slows only its own connection instead of
// dropping messages for everyone.
struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        // One-way traffic: echo it back - protocol transparent.
        let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
        let _ = sender.send_data(response.into_bytes()).await;
    }

    async fn on_request(&self, _session: SessionId, request: Packet, responder: Responder) {
        // Requests carry an obligation to answer: the consuming Responder is
        // the only way to do it, and Ok(Written) means the bytes were written.
        let _ = responder.respond(request.into_payload()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure multiple protocols - the same business logic serves all of them.
    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?.path("/ws");
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    let server = TransportServerBuilder::new()
        .max_connections(10000)
        .protocol(tcp_config)
        .protocol(websocket_config)
        .protocol(quic_config)
        .build(Arc::new(Echo))
        .await?;

    // Serves until the listeners stop. Use shutdown() to await full teardown.
    server.serve().await?;
    Ok(())
}
```

### Create a Client Connection

```rust,no_run
use msgtrans::{
    TransportClientBuilder,
    TcpClientConfig,
    ClientEvent,
};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tcp_config = TcpClientConfig::new("127.0.0.1:8001")?
        .connect_timeout(Duration::from_secs(30));

    let mut client = TransportClientBuilder::new()
        .protocol(tcp_config)
        .build()
        .await?;

    client.connect().await?;

    // Consume events. The stream has a single consumer and is taken once.
    let mut events = client.events().await?;
    let event_task = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                ClientEvent::Message(msg) => {
                    println!("Received: {}", msg.as_text_lossy());
                }
                ClientEvent::Request(req) => {
                    // Server-initiated request: `req` is consuming; answer it.
                    req.respond_detached(b"ack".to_vec());
                }
                ClientEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
    });

    // Send a one-way message. `send` is write-confirmed: it returns once the
    // bytes reached the socket (use `send_detached` for fire-and-forget).
    let receipt = client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("Message sent (id {})", receipt.message_id);

    // Send a request and wait for the response. `Ok` carries the response
    // bytes; every failure, including a timeout, is an `Err`.
    let response = client.request("What time is it?".as_bytes()).await?;
    println!("Received response: {}", String::from_utf8_lossy(&response));

    client.shutdown().await?;
    event_task.await?;
    Ok(())
}
```

## 🏗️ Architecture Design

### Three-Layer Architecture

```text
+-------------------------------------+
|  Application Layer                  |  <- Business logic, protocol-agnostic
+-------------------------------------+
|  Transport Layer                    |  <- Connection management, unified API
|  - TransportServer / TransportClient|     - Connection lifecycle
|  - SessionActor                     |     - Event routing
|  - RequestRegistry                  |     - Request/response lifecycle
+-------------------------------------+
|  Protocol Layer                     |  <- Protocol implementation
|  - TCP / WebSocket / QUIC adapters  |     - Connection trait
|  - Protocol configs                 |     - Protocol registration
+-------------------------------------+
```

### Design Principles

**Unified abstraction, protocol transparency** — `TransportServer`/`TransportClient`
expose one business interface; each adapter implements the `Connection` trait and
hides protocol details.

**Configuration-driven** — the same server code runs on any protocol; only the
config passed to `.protocol(..)` changes:

```rust,no_run
# use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, QuicServerConfig, Packet, SessionId};
# use std::sync::Arc;
# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
#     async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
# let handler = Arc::new(H);
// TCP server
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build(handler.clone()).await?;

// QUIC server - identical business logic
let server = TransportServerBuilder::new()
    .protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build(handler).await?;
# Ok(()) }
```

### Handler Model

The server delivers each session's traffic to a `SessionHandler`; the client
still exposes an event stream, since a client has exactly one connection.

```rust
use msgtrans::{
    ClientEvent,
    ConnectionInfo,
    TransportError, CloseReason,
    Packet,
    Responder, SessionHandler, SessionSender,
    SessionId,
};

// Server side: implement the handler. `on_message` (one-way traffic) and
// `on_request` (requests, answered through the consuming Responder) are
// required; the lifecycle hooks are optional.
struct MyHandler;

#[async_trait::async_trait]
impl SessionHandler for MyHandler {
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender) { /* ... */ }
    async fn on_request(&self, session_id: SessionId, request: Packet, responder: Responder) {
        let _ = responder.respond(request.into_payload()).await; // Ok(Written) == bytes written
    }
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) { /* ... */ }
    async fn on_disconnected(&self, session_id: SessionId, reason: CloseReason) { /* ... */ }
    async fn on_error(&self, session_id: SessionId, error: TransportError) { /* ... */ }
}

// Client side: events
# fn _client_events(ev: ClientEvent) { match ev {
ClientEvent::Connected { info } => { /* ... */ }
ClientEvent::Message(msg) => { /* one-way data; msg.payload() */ }
ClientEvent::Request(req) => { /* consuming; req.respond_detached(..) */ }
ClientEvent::MessageSent { message_id } => { /* ... */ }
ClientEvent::Disconnected { reason } => { /* ... */ }
ClientEvent::Error { error } => { /* ... */ }
# _ => {} } }
```

## ⚡ Usage Patterns

### Concurrent Sending

`TransportServer` is cheaply cloneable (it shares state via `Arc`), so it can be
moved into spawned tasks for concurrent session access:

```rust,no_run
# use msgtrans::{TransportServer, SessionHandler, SessionSender, Packet, SessionId};
# use std::sync::Arc;
struct Echo { server: TransportServer }

#[async_trait::async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, session_id: SessionId, packet: Packet, _tx: SessionSender) {
        // Handing work to a spawned task keeps this session's actor free to
        // pick up the next message.
        let server = self.server.clone();
        tokio::spawn(async move {
            let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
            let _ = server.send(session_id, response.as_bytes()).await;
        });
    }

    async fn on_request(&self, _s: SessionId, request: Packet, responder: msgtrans::Responder) {
        // respond_detached hands the write off; the registry still records
        // the true outcome.
        responder.respond_detached(request.into_payload());
    }
}
```

### Request / Response

```rust,no_run
# use msgtrans::TransportClient;
# async fn f(client: &TransportClient) -> Result<(), Box<dyn std::error::Error>> {
// `Ok` carries the response bytes; a timeout is an `Err`.
let response = client.request(b"Get user data").await?;
println!("Got {} bytes", response.len());
# Ok(()) }
```

## 🔌 Protocol Extension

To add a protocol, implement the `Connection` trait for your adapter and a
matching config type. See the built-in `adapters::{tcp, websocket, quic}` for
complete, working references; the outline below shows the shape:

```rust
use msgtrans::spi::{
    event_channel, CloseReason, Connection, ConnectionEvents, ConnectionInfo, ConnectionWriter,
    EventSink, Packet, SessionId, TransportError, WriteCompletion,
};
use msgtrans::FramePolicy;
use std::sync::Arc;

/// The write half. Kept separate from the connection so the transport can clone
/// it under its connection lock and RELEASE the lock before awaiting the
/// enqueue — a saturated queue must not park reconnect/close/shutdown.
struct MyWriter {
    queue: tokio::sync::mpsc::Sender<(Packet, WriteCompletion)>,
}

#[async_trait::async_trait]
impl ConnectionWriter for MyWriter {
    async fn send_with_completion(
        &self,
        packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError> {
        // Hand BOTH the packet and its completion to the write loop. The
        // completion must only be resolved once the bytes have actually reached
        // the socket — resolving it here would be a false write confirmation,
        // and the whole point of `WriteCompletion` is that `Ok` is provable.
        //
        // If the queue is gone, DROP the completion: dropping reports failure
        // by construction, so there is no path that invents an `Ok`.
        self.queue
            .send((packet, completion))
            .await
            .map_err(|_| TransportError::connection_error("connection closed", false))
    }
}

/// The write loop that owns the socket. It is the only thing allowed to say a
/// write succeeded.
async fn write_loop(mut queue: tokio::sync::mpsc::Receiver<(Packet, WriteCompletion)>) {
    while let Some((packet, completion)) = queue.recv().await {
        let bytes = match packet.try_encode() {
            Ok(bytes) => bytes,
            Err(e) => {
                completion.complete(Err(TransportError::protocol_error("encode", e.to_string())));
                continue;
            }
        };
        // Replace with your real socket write; report exactly what it returned.
        let written: Result<(), TransportError> = my_socket_write(&bytes).await;
        completion.complete(written);
    }
}
# async fn my_socket_write(_bytes: &[u8]) -> Result<(), TransportError> { Ok(()) }

pub struct MyAdapter {
    session_id: SessionId,
    sink: EventSink,
    events: Option<ConnectionEvents>,
    queue: tokio::sync::mpsc::Sender<(Packet, WriteCompletion)>,
}

impl MyAdapter {
    pub fn new() -> Self {
        let (sink, events) = event_channel(std::num::NonZeroUsize::new(1024).unwrap());
        let (queue, rx) = tokio::sync::mpsc::channel(1024);
        tokio::spawn(write_loop(rx));
        Self { session_id: SessionId::new(0), sink, events: Some(events), queue }
    }

    /// Your read loop pushes inbound packets here. The pipe decompresses and
    /// normalizes them, so every consumer sees plaintext.
    pub async fn on_packet(&self, packet: Packet) -> bool {
        self.sink.message(packet).await
    }
}

#[async_trait::async_trait]
impl Connection for MyAdapter {
    fn writer(&self) -> Arc<dyn ConnectionWriter> {
        Arc::new(MyWriter { queue: self.queue.clone() })
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.sink.close(CloseReason::Normal);
        Ok(())
    }
    fn session_id(&self) -> SessionId { self.session_id }
    fn set_session_id(&mut self, session_id: SessionId) { self.session_id = session_id; }
    fn connection_info(&self) -> ConnectionInfo { ConnectionInfo::default() }
    fn is_connected(&self) -> bool { true }
    async fn flush(&mut self) -> Result<(), TransportError> { Ok(()) }
    fn take_event_pipe(&mut self) -> Option<ConnectionEvents> { self.events.take() }
    fn set_frame_policy(&self, _policy: FramePolicy) { /* honor Strict here */ }
}
```

## 📖 Usage Examples

### WebSocket Server

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    Responder, SessionHandler, SessionSender, TransportServerBuilder,
    WebSocketServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

struct Chat;

#[async_trait]
impl SessionHandler for Chat {
    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        let msg = String::from_utf8_lossy(packet.payload());
        let _ = sender.send_data(format!("You said: {msg}").into_bytes()).await;
    }

    async fn on_request(&self, _session: SessionId, request: Packet, responder: Responder) {
        let msg = String::from_utf8_lossy(request.payload()).to_string();
        let _ = responder.respond(format!("You asked: {msg}").into_bytes()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WebSocketServerConfig::new("127.0.0.1:8080")?.path("/chat");

    let server = TransportServerBuilder::new()
        .protocol(config)
        .max_connections(1000)
        .build(Arc::new(Chat))
        .await?;

    server.serve().await?;
    Ok(())
}
```

### QUIC Client

```rust,no_run
use msgtrans::{
    TransportClientBuilder,
    QuicClientConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Local / self-signed test server: skip certificate verification.
    // In production, drop danger_skip_verification() and configure a real
    // server name and CA instead.
    let config = QuicClientConfig::new("127.0.0.1:8003")?
        .server_name("localhost")
        .danger_skip_verification();

    let mut client = TransportClientBuilder::new()
        .protocol(config)
        .build()
        .await?;

    client.connect().await?;

    for i in 0..1000u32 {
        client.send(format!("message {i}").as_bytes()).await?;
    }
    println!("Done");
    Ok(())
}
```

## 🛠️ Configuration Options

### Server Configuration

```rust,no_run
use msgtrans::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig};
use std::time::Duration;

# fn f() -> Result<(), Box<dyn std::error::Error>> {
let tcp_config = TcpServerConfig::new("0.0.0.0:8001")?
    .keepalive(Some(Duration::from_secs(60)))
    .nodelay(true)
    .reuse_addr(true);

let ws_config = WebSocketServerConfig::new("0.0.0.0:8002")?
    .path("/api/ws");

let quic_config = QuicServerConfig::new("0.0.0.0:8003")?
    .cert_pem(std::fs::read_to_string("cert.pem")?)
    .key_pem(std::fs::read_to_string("key.pem")?)
    .max_concurrent_streams(1000);
# Ok(()) }
```

## 🔧 Advanced Features

### Statistics

```rust,no_run
# use msgtrans::TransportServer;
# async fn f(server: &TransportServer) {
let active = server.session_count().await;
println!("Active sessions: {active}");
# }
```

### Graceful Error Handling

```rust,no_run
# use msgtrans::{TransportClient, TransportError};
# async fn f(client: &mut TransportClient) -> Result<(), Box<dyn std::error::Error>> {
match client.send("Hello, World!".as_bytes()).await {
    Ok(result) => println!("Sent (ID: {})", result.message_id),
    Err(TransportError::Connection { .. }) => {
        println!("Connection lost, reconnecting");
        client.connect().await?;
    }
    Err(TransportError::Protocol { protocol, reason }) => {
        println!("Protocol error [{protocol}]: {reason}");
    }
    Err(e) => println!("Other error: {e}"),
}
# Ok(()) }
```

### Graceful Shutdown

```rust,no_run
use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, Packet, SessionId};
use std::sync::Arc;

# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
#     async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .build(Arc::new(H)).await?;

// ... later, stop accepting and wait for supervised teardown:
let report = server.shutdown().await;
assert!(report.clean, "server shutdown did not finish before the deadline");
# Ok(()) }
```

## Kotlin and Swift bindings

[`msgtrans-ffi`](https://github.com/zoujiaqing/msgtrans/tree/main/crates/msgtrans-ffi) brings the MsgTrans 2.0 **client** to Kotlin (Android, JVM and Kotlin Multiplatform) and Swift (iOS and macOS) through UniFFI. It supports TCP (including TLS), WebSocket and QUIC, with one-way messages, requests and responses, and replies to server-initiated requests. The Rust crate provides the server; the bindings do not.

The bindings build against the Rust crate in this workspace and use the same version. Distribution consists of native libraries and generated Kotlin/Swift bindings, not a separate crates.io package. The workspace uses Rust 1.99.0 to build these artifacts; the Rust `msgtrans` crate itself requires Rust 1.88 or newer.

From `crates/msgtrans-ffi`:

| Target | Build command | Output |
|---|---|---|
| Android | `ANDROID_NDK_HOME=/path/to/ndk scripts/build-android.sh` | `dist/android/jniLibs/arm64-v8a/libmsgtrans_ffi.so` by default |
| iOS | `scripts/build-ios.sh` | `dist/ios/MsgtransFFI.xcframework` for devices and Apple-silicon simulators |
| Kotlin and Swift bindings | `scripts/gen-bindings.sh` | Generated sources in `bindings/` |

See the [FFI build and integration guide](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans-ffi/README.md) for platform requirements, Kotlin and Swift examples, certificate verification, and the event-loop shutdown contract. Browser applications can use the separate [TypeScript WebSocket client](https://github.com/zoujiaqing/msgtrans-typescript).

## Upgrading to 2.0

Read the [1.x migration guide](https://github.com/zoujiaqing/msgtrans/blob/main/docs/MIGRATION_2.0.md), [2.0 API guide](https://github.com/zoujiaqing/msgtrans/blob/main/docs/API_2.0.md), and [wire format](https://github.com/zoujiaqing/msgtrans/blob/main/docs/WIRE_FORMAT.md). The packet layout is unchanged, but Rust APIs and connection rules have changed. Strict decoding is now the default, and QUIC peers that do not offer ALPN, including MsgTrans 1.0.10, are not supported.


## 📚 Documentation and Examples

The [`examples/`](https://github.com/zoujiaqing/msgtrans/tree/main/crates/msgtrans/examples/) directory contains complete, runnable programs:

- [`echo_server.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_server.rs) - multi-protocol echo server
- [`echo_client_tcp.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_tcp.rs) - TCP client
- [`echo_client_websocket.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_websocket.rs) - WebSocket client
- [`echo_client_quic.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_quic.rs) - QUIC client
- [`load_test.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/load_test.rs) / [`load_test_server.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/load_test_server.rs) - load testing
- [`packet.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/packet.rs) - packet serialization

```bash
# Start the multi-protocol echo server
cargo run -p msgtrans --example echo_server

# In another terminal, run a client
cargo run -p msgtrans --example echo_client_tcp
```

## 🏆 Use Cases

- **Game servers** - high-concurrency real-time communication
- **Chat systems** - multi-protocol instant messaging
- **Microservice communication** - efficient inter-service transport
- **Real-time data** - financial, monitoring and telemetry systems
- **IoT platforms** - large-scale device connection management
- **Protocol gateways** - multi-protocol conversion and proxying

## 📝 License

Licensed under the [Apache License 2.0](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE).

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 Contributing

Issues and Pull Requests are welcome.
