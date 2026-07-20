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
use msgtrans::{
    transport::TransportServerBuilder,
    protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig},
    event::ServerEvent,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure multiple protocols - the same business logic serves all of them.
    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?.with_path("/ws");
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    let server = TransportServerBuilder::new()
        .max_connections(10000)
        .with_protocol(tcp_config)
        .with_protocol(websocket_config)
        .with_protocol(quic_config)
        .build()
        .await?;

    // Subscribe to events before serving so nothing is missed.
    let mut events = server.subscribe_events();

    // Drive the listeners. `serve()` runs until the server is stopped, so spawn
    // it and handle events on the main task.
    let server_for_events = server.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ServerEvent::ConnectionEstablished { session_id, .. } => {
                    println!("New connection: {session_id}");
                }
                ServerEvent::MessageReceived { session_id, context } => {
                    // Echo the message back - protocol transparent.
                    let response = format!("Echo: {}", String::from_utf8_lossy(&context.data));
                    let _ = server_for_events.send(session_id, response.as_bytes()).await;
                }
                ServerEvent::ConnectionClosed { session_id, .. } => {
                    println!("Connection closed: {session_id}");
                }
                _ => {}
            }
        }
    });

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
    tokio,
};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tcp_config = TcpClientConfig::new("127.0.0.1:8001")?
        .with_connect_timeout(Duration::from_secs(30));

    let mut client = TransportClientBuilder::new()
        .with_protocol(tcp_config)
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

    // Consume events.
    let mut events = client.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
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
config passed to `.with_protocol(..)` changes:

```rust,no_run
# use msgtrans::{transport::TransportServerBuilder, protocol::{TcpServerConfig, QuicServerConfig}};
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
// TCP server
let server = TransportServerBuilder::new()
    .with_protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build().await?;

// QUIC server - identical business logic
let server = TransportServerBuilder::new()
    .with_protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build().await?;
# Ok(()) }
```

### Event-Driven Model

```rust
use msgtrans::{
    event::{ServerEvent, ClientEvent},
    command::ConnectionInfo,
    error::{TransportError, CloseReason},
    SessionId, TransportContext,
};

// Server events
# fn _server_events(ev: ServerEvent) { match ev {
ServerEvent::ConnectionEstablished { session_id, info } => { /* ... */ }
ServerEvent::MessageReceived { session_id, context } => { /* ... */ }
ServerEvent::MessageSent { session_id, message_id } => { /* ... */ }
ServerEvent::ConnectionClosed { session_id, reason } => { /* ... */ }
ServerEvent::TransportError { session_id, error } => { /* ... */ }
# _ => {} } }

// Client events
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
# use msgtrans::{transport::TransportServer, event::ServerEvent};
# async fn f(server: TransportServer) -> Result<(), Box<dyn std::error::Error>> {
let mut events = server.subscribe_events();
while let Ok(event) = events.recv().await {
    if let ServerEvent::MessageReceived { session_id, context } = event {
        let server = server.clone();
        tokio::spawn(async move {
            let response = format!("Echo: {}", String::from_utf8_lossy(&context.data));
            let _ = server.send(session_id, response.as_bytes()).await;
        });
    }
}
# Ok(()) }
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
use msgtrans::{
    transport::TransportServerBuilder,
    protocol::WebSocketServerConfig,
    event::ServerEvent,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WebSocketServerConfig::new("127.0.0.1:8080")?.with_path("/chat");

    let server = TransportServerBuilder::new()
        .with_protocol(config)
        .max_connections(1000)
        .build()
        .await?;

    let mut events = server.subscribe_events();
    let server_for_events = server.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let ServerEvent::MessageReceived { session_id, context } = event {
                let msg = String::from_utf8_lossy(&context.data);
                let _ = server_for_events
                    .send(session_id, format!("You said: {msg}").as_bytes())
                    .await;
            }
        }
    });

    server.serve().await?;
    Ok(())
}
```

### QUIC Client

```rust,no_run
use msgtrans::{
    transport::TransportClientBuilder,
    protocol::QuicClientConfig,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Local / self-signed test server: skip certificate verification.
    // In production, drop danger_skip_verification() and configure a real
    // server name and CA instead.
    let config = QuicClientConfig::new("127.0.0.1:8003")?
        .with_server_name("localhost")
        .danger_skip_verification();

    let mut client = TransportClientBuilder::new()
        .with_protocol(config)
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
    .with_max_connections(10000)
    .with_keepalive(Some(Duration::from_secs(60)))
    .with_nodelay(true)
    .with_reuse_addr(true);

let ws_config = WebSocketServerConfig::new("0.0.0.0:8002")?
    .with_path("/api/ws")
    .with_max_frame_size(1024 * 1024)
    .with_max_connections(5000);

let quic_config = QuicServerConfig::new("0.0.0.0:8003")?
    .with_cert_pem(std::fs::read_to_string("cert.pem")?)
    .with_key_pem(std::fs::read_to_string("key.pem")?)
    .with_max_concurrent_streams(1000);
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
use msgtrans::{transport::TransportServerBuilder, protocol::TcpServerConfig};
use std::time::Duration;

# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .with_protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .graceful_shutdown(Some(Duration::from_secs(30)))
    .build().await?;

// ... later, drain active sessions using the configured timeout:
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
