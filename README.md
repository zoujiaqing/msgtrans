# 🚀 MsgTrans - Modern Multi-Protocol Communication Framework

[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

🌐 Language: [English](README.md) | [简体中文](README.zh-CN.md)

> **Modern multi-protocol communication framework with a unified interface over TCP, WebSocket and QUIC**

## 🌟 Core Features

### 🏗️ Unified Architecture

- **Three-layer architecture**: Application → Transport → Protocol, with clear separation
- **Protocol-agnostic business logic**: one codebase, multi-protocol deployment
- **Configuration-driven**: switch protocols through configuration without changing business logic
- **Pluggable adapters**: implement the `Connection` trait to add a new protocol

### ⚡ Modern Concurrency

- **Lock-free internals**: per-session actors and lock-free maps avoid Mutex contention on the hot path
- **Zero-copy packets**: `Packet` carries a `Bytes` payload, handed straight to the wire where possible
- **Event-driven model**: fully asynchronous, non-blocking event handling
- **Bounded backpressure**: outbound queues are bounded per connection so a slow peer cannot exhaust memory or stall a fan-out loop

### 🔌 Protocols

- **TCP** - reliable stream transport
- **WebSocket** - real-time web communication
- **QUIC** - modern UDP-based transport
- **Custom protocols** - implement the `Connection` trait

### 🎯 Minimalist API

- **Builder pattern**: fluent, readable configuration
- **Type safety**: compile-time checked configuration
- **Sensible defaults**: works out of the box, tune only when needed

## 🚀 Quick Start

### Installation

```toml
[dependencies]
msgtrans = "1.0"
```

### Create a Multi-Protocol Server

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    transport::{SessionHandler, SessionSender, TransportServerBuilder},
    protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig},
    packet::Packet,
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
        // Echo the message back - protocol transparent.
        let response = format!("Echo: {}", String::from_utf8_lossy(&packet.payload));
        let _ = sender.send_data(response.into_bytes()).await;
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

    // Runs until the server is stopped.
    server.serve().await?;
    Ok(())
}
```

### Create a Client Connection

```rust,no_run
use msgtrans::{
    transport::TransportClientBuilder,
    protocol::TcpClientConfig,
    event::ClientEvent,
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

    // Send a one-way message.
    client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("Message sent");

    // Send a request and wait for the response.
    let result = client.request("What time is it?".as_bytes()).await?;
    if let Some(data) = result.data {
        println!("Received response: {}", String::from_utf8_lossy(&data));
    } else {
        println!("Request timed out");
    }

    // Consume events. The stream has a single consumer and is taken once.
    let mut events = client.events().await?;
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                ClientEvent::MessageReceived(context) => {
                    println!("Received: {}", String::from_utf8_lossy(&context.data));
                }
                ClientEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
    });

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
# use msgtrans::{transport::{TransportServerBuilder, SessionHandler, SessionSender}, protocol::{TcpServerConfig, QuicServerConfig}, packet::Packet, SessionId};
# use std::sync::Arc;
# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
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
    event::ClientEvent,
    command::ConnectionInfo,
    error::{TransportError, CloseReason},
    packet::Packet,
    transport::{SessionHandler, SessionSender},
    SessionId,
};

// Server side: implement the handler. Only `on_message` is required.
struct MyHandler;

#[async_trait::async_trait]
impl SessionHandler for MyHandler {
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender) { /* ... */ }
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) { /* ... */ }
    async fn on_disconnected(&self, session_id: SessionId, reason: CloseReason) { /* ... */ }
    async fn on_error(&self, session_id: SessionId, error: TransportError) { /* ... */ }
}

// Client side: events
# fn _client_events(ev: ClientEvent) { match ev {
ClientEvent::Connected { info } => { /* ... */ }
ClientEvent::MessageReceived(context) => { /* ... */ }
ClientEvent::MessageSent { message_id } => { /* ... */ }
ClientEvent::Disconnected { reason } => { /* ... */ }
ClientEvent::Error { error } => { /* ... */ }
# _ => {} } }
```

## ⚡ Usage Patterns

### Concurrent Sending

`TransportServer` is cheaply cloneable (it shares state via `Arc`), so it can be
moved into spawned tasks for concurrent, lock-free session access:

```rust,no_run
# use msgtrans::{transport::{TransportServer, SessionHandler, SessionSender}, packet::Packet, SessionId};
# use std::sync::Arc;
struct Echo { server: TransportServer }

#[async_trait::async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, session_id: SessionId, packet: Packet, _tx: SessionSender) {
        // Handing work to a spawned task keeps this session's actor free to
        // pick up the next message.
        let server = self.server.clone();
        tokio::spawn(async move {
            let response = format!("Echo: {}", String::from_utf8_lossy(&packet.payload));
            let _ = server.send(session_id, response.as_bytes()).await;
        });
    }
}
```

### Request / Response

```rust,no_run
# use msgtrans::transport::TransportClient;
# async fn f(client: &TransportClient) -> Result<(), Box<dyn std::error::Error>> {
let response = client.request(b"Get user data").await?;
if let Some(data) = response.data {
    println!("Got {} bytes", data.len());
} else {
    println!("Request timed out");
}
# Ok(()) }
```

## 🔌 Protocol Extension

To add a protocol, implement the `Connection` trait for your adapter and a
matching config type. See the built-in `adapters::{tcp, websocket, quic}` for
complete, working references; the outline below shows the shape:

```rust,ignore
use msgtrans::{connection::Connection, packet::Packet, error::TransportError};

pub struct MyAdapter { /* protocol-specific state */ }

#[async_trait::async_trait]
impl Connection for MyAdapter {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> { /* ... */ }
    // ... remaining Connection methods
}
```

## 📖 Usage Examples

### WebSocket Server

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    transport::{SessionHandler, SessionSender, TransportServerBuilder},
    protocol::WebSocketServerConfig,
    packet::Packet,
    SessionId,
};
use std::sync::Arc;

struct Chat;

#[async_trait]
impl SessionHandler for Chat {
    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        let msg = String::from_utf8_lossy(&packet.payload);
        let _ = sender.send_data(format!("You said: {msg}").into_bytes()).await;
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
    transport::TransportClientBuilder,
    protocol::QuicClientConfig,
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
use msgtrans::protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig};
use std::time::Duration;

# fn f() -> Result<(), Box<dyn std::error::Error>> {
let tcp_config = TcpServerConfig::new("0.0.0.0:8001")?
    .keepalive(Some(Duration::from_secs(60)))
    .nodelay(true)
    .reuse_addr(true);

let ws_config = WebSocketServerConfig::new("0.0.0.0:8002")?
    .path("/api/ws")
    .max_frame_size(1024 * 1024);

let quic_config = QuicServerConfig::new("0.0.0.0:8003")?
    .cert_pem(std::fs::read_to_string("cert.pem")?)
    .key_pem(std::fs::read_to_string("key.pem")?)
    .max_concurrent_streams(1000);
# Ok(()) }
```

## 🔧 Advanced Features

### Statistics

```rust,no_run
# use msgtrans::transport::TransportServer;
# async fn f(server: &TransportServer) {
let active = server.session_count().await;
println!("Active sessions: {active}");
# }
```

### Graceful Error Handling

```rust,no_run
# use msgtrans::{transport::TransportClient, error::TransportError};
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
use msgtrans::{transport::{TransportServerBuilder, SessionHandler, SessionSender}, protocol::TcpServerConfig, packet::Packet, SessionId};
use std::sync::Arc;

# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .build(Arc::new(H)).await?;

// ... later, drain active sessions (TransportConfig::graceful_timeout):
server.stop().await;
# Ok(()) }
```

## 📚 Documentation and Examples

The [`examples/`](examples/) directory contains complete, runnable programs:

- [`echo_server.rs`](examples/echo_server.rs) - multi-protocol echo server
- [`echo_client_tcp.rs`](examples/echo_client_tcp.rs) - TCP client
- [`echo_client_websocket.rs`](examples/echo_client_websocket.rs) - WebSocket client
- [`echo_client_quic.rs`](examples/echo_client_quic.rs) - QUIC client
- [`load_test.rs`](examples/load_test.rs) / [`load_test_server.rs`](examples/load_test_server.rs) - load testing
- [`packet.rs`](examples/packet.rs) - packet serialization

```bash
# Start the multi-protocol echo server
cargo run --example echo_server

# In another terminal, run a client
cargo run --example echo_client_tcp
```

## 🏆 Use Cases

- **Game servers** - high-concurrency real-time communication
- **Chat systems** - multi-protocol instant messaging
- **Microservice communication** - efficient inter-service transport
- **Real-time data** - financial, monitoring and telemetry systems
- **IoT platforms** - large-scale device connection management
- **Protocol gateways** - multi-protocol conversion and proxying

## 📝 License

Licensed under the [Apache License 2.0](LICENSE).

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 Contributing

Issues and Pull Requests are welcome.
