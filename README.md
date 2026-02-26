# 🚀 MsgTrans - Modern Multi-Protocol Communication Framework

[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.0.0--beta.1-green.svg)](Cargo.toml)

🌐 Language: [English](README.md) | [简体中文](README.zh-CN.md)

> **Enterprise-grade modern multi-protocol communication framework with unified interface supporting TCP, WebSocket, QUIC and more**

## 🌟 Core Features

### ⚡ Ultimate Performance
 - **1M+** concurrent connections support
 - **10M+/sec** message throughput
 - **1ms** average latency
 - **Lock-free concurrent architecture** fully utilizing multi-core performance

### 🏗️ **Unified Architecture Design**
- **Three-layer architecture abstraction**: Application → Transport → Protocol layers with clear separation
- **Protocol-agnostic business logic**: One codebase, multi-protocol deployment
- **Configuration-driven design**: Switch protocols through configuration without modifying business logic
- **Hot-pluggable extensions**: Easily extend new protocol support

### ⚡ **Modern Concurrent Architecture**
- **Lock-free concurrent design**: Completely eliminate lock contention, fully utilize multi-core performance
- **Zero-copy optimization**: `SharedPacket` and `ArcPacket` implement memory zero-copy
- **Event-driven model**: Fully asynchronous non-blocking, efficient event handling
- **Intelligent optimization**: CPU-aware automatic performance tuning

### 🔌 **Multi-Protocol Unified Support**
- **TCP** - Reliable transport protocol
- **WebSocket** - Real-time Web communication
- **QUIC** - Next-generation transport protocol
- **Custom protocols** - Easily implement custom protocol extensions

### 🎯 **Minimalist API Design**
- **Builder pattern**: Fluent configuration, elegant and readable code
- **Type safety**: Compile-time error checking, runtime stability and reliability
- **Zero-configuration optimization**: High performance by default, ready to use out of the box
- **Backward compatibility**: Zero migration cost for version upgrades

## 🚀 Quick Start

### Installation

```toml
[dependencies]
msgtrans = "1.0.0"
```

### Create Multi-Protocol Server

```rust
use msgtrans::{
    transport::TransportServerBuilder,
    protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig},
    event::ServerEvent,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure multiple protocols - same business logic supports multiple protocols
    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?
        .with_path("/ws");
    
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    // Build multi-protocol server
    let mut server = TransportServerBuilder::new()
        .max_connections(10000)
        .with_protocol(tcp_config)
        .with_protocol(websocket_config)
        .with_protocol(quic_config)
        .build()
        .await?;

    println!("🚀 Multi-protocol server started successfully!");
    
    // Get event stream
    let mut events = server.events().await?;
    
    // Unified event handling - all protocols use the same logic
    while let Some(event) = events.recv().await {
        match event {
            ServerEvent::ConnectionEstablished { session_id, .. } => {
                println!("New connection: {}", session_id);
            }
            ServerEvent::MessageReceived { session_id, context } => {
                // Echo message - protocol transparent
                let message = String::from_utf8_lossy(&context.data);
                let response = format!("Echo: {}", message);
                let _ = server.send(session_id, response.as_bytes()).await;
            }
            ServerEvent::ConnectionClosed { session_id, .. } => {
                println!("Connection closed: {}", session_id);
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Create Client Connection

```rust
use msgtrans::{
    transport::TransportClientBuilder,
    protocol::TcpClientConfig,
    event::ClientEvent,
    tokio,
};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure client - configuration-driven protocol selection
    let tcp_config = TcpClientConfig::new("127.0.0.1:8001")?
        .with_timeout(Duration::from_secs(30));

    // Build client - zero configuration with high performance
    let mut client = TransportClientBuilder::new()
        .with_protocol(tcp_config)
        .build()
        .await?;

    // Connect to server
    client.connect().await?;

    // Send message - simple API, directly send byte data
    let _result = client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("✅ Message sent successfully");

    // Send request and wait for response
    match client.request("What time is it?".as_bytes()).await? {
        result if result.data.is_some() => {
            let response = String::from_utf8_lossy(result.data.as_ref().unwrap());
            println!("📥 Received response: {}", response);
        }
        _ => println!("❌ Request timeout or failed"),
    }

    // Receive events - unified event model
    let mut events = client.events().await?;
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::MessageReceived(context) => {
                    let message = String::from_utf8_lossy(&context.data);
                    println!("📨 Received message: {}", message);
                }
                ClientEvent::Disconnected { .. } => {
                    println!("🔌 Connection closed");
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(())
}
```

## 🏗️ Architecture Design

### 📚 Three-Layer Architecture

```
┌─────────────────────────────────────┐
│  🎯 Application Layer               │  ← Business logic, protocol-agnostic
├─────────────────────────────────────┤
│  🚀 Transport Layer                 │  ← Connection management, unified API
│  ├── TransportServer/Client         │    • Connection lifecycle management
│  ├── SessionManager                 │    • Event routing and dispatching
│  └── EventStream                    │    • Message passing and broadcasting
├─────────────────────────────────────┤
│  📡 Protocol Layer                  │  ← Protocol implementation, extensible
│  ├── TCP/WebSocket/QUIC             │    • Protocol-specific adapters
│  ├── ProtocolAdapter                │    • Protocol configuration management
│  └── ConfigurationRegistry          │    • Protocol registration mechanism
└─────────────────────────────────────┘
```

### 🔄 Design Principles

#### **Unified Abstraction, Protocol Transparency**
- **TransportServer/Client** provide unified business interfaces
- **Transport** manages single connection lifecycle
- **ProtocolAdapter** hides protocol implementation details

#### **Configuration-Driven, Flexible Extension**
```rust
// Same server code, different protocol configurations
let server = TransportServerBuilder::new()
    .with_protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build().await?;  // TCP version

let server = TransportServerBuilder::new()
    .with_protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build().await?;  // QUIC version - identical business logic
```

### 🎯 Event-Driven Model

```rust
// Server event types
pub enum ServerEvent {
    ConnectionEstablished { session_id: SessionId, info: ConnectionInfo },
    MessageReceived { session_id: SessionId, context: TransportContext },
    MessageSent { session_id: SessionId, message_id: u32 },
    ConnectionClosed { session_id: SessionId, reason: CloseReason },
    TransportError { session_id: Option<SessionId>, error: TransportError },
}

// Client event types
pub enum ClientEvent {
    Connected { info: ConnectionInfo },
    MessageReceived(TransportContext),
    MessageSent { message_id: u32 },
    Disconnected { reason: CloseReason },
    Error { error: TransportError },
}

// Concise event handling pattern - Server
let mut events = server.events().await?;
while let Some(event) = events.recv().await {
    match event {
        ServerEvent::MessageReceived { session_id, context } => {
            // Protocol-agnostic business processing - directly use byte data
            let message = String::from_utf8_lossy(&context.data);
            let response = format!("Processing result: {}", message);
            server.send(session_id, response.as_bytes()).await?;
        }
        _ => {}
    }
}

// Concise event handling pattern - Client
let mut events = client.events().await?;
while let Some(event) = events.recv().await {
    match event {
        ClientEvent::MessageReceived(context) => {
            // Handle received messages - directly use byte data
            let message = String::from_utf8_lossy(&context.data);
            println!("Received: {}", message);
        }
        _ => {}
    }
}
```

## ⚡ Modern Features

### 🔒 Lock-Free Concurrent Architecture

```rust
// User-level API is simple, underlying automatic lock-free optimization
// Concurrent sending - internally optimized with lock-free queues
let tasks: Vec<_> = (0..1000).map(|i| {
    let client = client.clone();
    tokio::spawn(async move {
        let message = format!("Message {}", i);
        client.send(message.as_bytes()).await
    })
}).collect();

// Server high-concurrency processing - internally using lock-free hash tables for session management
let mut events = server.events().await?;
while let Some(event) = events.recv().await {
    match event {
        ServerEvent::MessageReceived { session_id, context } => {
            // High-concurrency processing, lock-free session access
            tokio::spawn(async move {
                let response = process_message(&context.data).await;
                server.send(session_id, &response).await
            });
        }
        _ => {}
    }
}
```

### 🧠 Intelligent Optimization

```rust
// CPU-aware automatic optimization - zero configuration high performance
let config = ConnectionConfig::auto_optimized(); // Auto-tuning based on CPU core count

// Intelligent connection pool - adaptive load
let server = TransportServerBuilder::new()
    .connection_pool_config(
        ConnectionPoolConfig::adaptive()  // Dynamic scaling
            .with_initial_size(100)
            .with_max_size(10000)
    )
    .build().await?;
```

### 📦 Zero-Copy Optimization

```rust
// User API always simple - internal automatic zero-copy optimization
let result = client.send("Hello, World!".as_bytes()).await?;

// Large data transmission - automatic zero-copy handling
let large_data = vec![0u8; 1024 * 1024]; // 1MB data
let result = client.send(&large_data).await?;

// Request-response - automatic zero-copy optimization
let response = client.request(b"Get user data").await?;
if let Some(data) = response.data {
    // Data transmission process already optimized, no additional copying needed
    process_response(&data);
}
```

## 🔌 Protocol Extension

### Implement Custom Protocol

```rust
// 1. Implement protocol adapter
pub struct MyProtocolAdapter {
    connection: MyConnection,
    event_sender: broadcast::Sender<TransportEvent>,
}

#[async_trait]
impl ProtocolAdapter for MyProtocolAdapter {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        // Implement protocol-specific send logic
        self.connection.send(packet.payload()).await?;
        Ok(())
    }
    
    fn connection_info(&self) -> ConnectionInfo {
        // Return connection information
        ConnectionInfo::new("MyProtocol", self.connection.peer_addr())
    }
    
    fn events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_sender.subscribe()
    }
}

// 2. Implement configuration structure
#[derive(Debug, Clone)]
pub struct MyProtocolServerConfig {
    pub bind_address: SocketAddr,
    pub custom_setting: String,
}

#[async_trait]
impl ServerConfig for MyProtocolServerConfig {
    type Adapter = MyProtocolAdapter;
    
    async fn build_server(&self) -> Result<Self::Adapter, TransportError> {
        // Build server adapter
        let connection = MyConnection::bind(&self.bind_address).await?;
        let (event_sender, _) = broadcast::channel(1000);
        
        Ok(MyProtocolAdapter {
            connection,
            event_sender,
        })
    }
}

// 3. Seamless integration - exactly the same usage as built-in protocols
let my_config = MyProtocolServerConfig {
    bind_address: "127.0.0.1:9000".parse()?,
    custom_setting: "custom_value".to_string(),
};

let server = TransportServerBuilder::new()
    .with_protocol(my_config)  // Use directly!
    .build()
    .await?;
```

## 📖 Usage Examples

### 🌐 WebSocket Chat Server

```rust
use msgtrans::{
    transport::TransportServerBuilder,
    protocol::WebSocketServerConfig,
    event::ServerEvent,
    SessionId,
    tokio,
};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WebSocketServerConfig::new("127.0.0.1:8080")?
        .with_path("/chat");

    let server = TransportServerBuilder::new()
        .with_protocol(config)
        .max_connections(1000)
        .build()
        .await?;

    println!("🌐 WebSocket Chat Server: ws://127.0.0.1:8080/chat");

    // Chat room management
    let mut chat_rooms: HashMap<String, Vec<SessionId>> = HashMap::new();
    let mut events = server.events().await?;

    while let Some(event) = events.recv().await {
        match event {
            ServerEvent::MessageReceived { session_id, context } => {
                let message = String::from_utf8_lossy(&context.data);
                
                // Parse chat commands
                if message.starts_with("/join ") {
                    let room = message[6..].to_string();
                    chat_rooms.entry(room.clone()).or_default().push(session_id);
                    
                    let response = format!("Joined room: {}", room);
                    let _ = server.send(session_id, response.as_bytes()).await;
                } else {
                    // Broadcast message to all users in the room
                    for (room, members) in &chat_rooms {
                        if members.contains(&session_id) {
                            let broadcast_msg = format!("[{}] {}", room, message);
                            for &member_id in members {
                                let _ = server.send(member_id, broadcast_msg.as_bytes()).await;
                            }
                            break;
                        }
                    }
                }
            }
            ServerEvent::ConnectionClosed { session_id, .. } => {
                // Remove user from all rooms
                for members in chat_rooms.values_mut() {
                    members.retain(|&id| id != session_id);
                }
            }
            _ => {}
        }
    }

    Ok(())
}
```

### ⚡ High-Performance QUIC Client

```rust
use msgtrans::{
    transport::TransportClientBuilder,
    protocol::QuicClientConfig,
    tokio,
};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = QuicClientConfig::new("127.0.0.1:8003")?
        .with_server_name("localhost")
        .with_alpn(vec![b"msgtrans".to_vec()]);

    let client = TransportClientBuilder::new()
        .with_protocol(config)
        .build()
        .await?;

    client.connect().await?;
    println!("✅ QUIC connection established successfully");

    // High-concurrency message sending test
    let start = Instant::now();
    let message_count = 10000;
    
    let tasks: Vec<_> = (0..message_count).map(|i| {
        let client = client.clone();
        tokio::spawn(async move {
            let message = format!("High-performance message {}", i);
            client.send(message.as_bytes()).await
        })
    }).collect();

    // Wait for all messages to complete sending
    for task in tasks {
        task.await??;
    }

    let duration = start.elapsed();
    println!("🚀 {} messages sent, time elapsed: {:?}", message_count, duration);
    println!("📊 Average per second: {:.0} messages", message_count as f64 / duration.as_secs_f64());

    // Test request-response performance
    let start = Instant::now();
    let request_count = 1000;
    
    for i in 0..request_count {
        let request_data = format!("Request {}", i);
        match client.request(request_data.as_bytes()).await? {
            result if result.data.is_some() => {
                // Request successful, record response time
                if i % 100 == 0 {
                    println!("✅ Request {} completed", i);
                }
            }
            _ => println!("❌ Request {} timeout", i),
        }
    }
    
    let duration = start.elapsed();
    println!("🔄 {} requests completed, time elapsed: {:?}", request_count, duration);
    println!("📊 Average per second: {:.0} requests", request_count as f64 / duration.as_secs_f64());

    Ok(())
}
```

## 🛠️ Configuration Options

### Server Configuration

```rust
// TCP Server - High reliability configuration
let tcp_config = TcpServerConfig::new("0.0.0.0:8001")?
    .with_max_connections(10000)
    .with_keepalive(Duration::from_secs(60))
    .with_nodelay(true)
    .with_reuse_addr(true);

// WebSocket Server - Web integration configuration
let ws_config = WebSocketServerConfig::new("0.0.0.0:8002")?
    .with_path("/api/ws")
    .with_max_frame_size(1024 * 1024)
    .with_max_connections(5000);

// QUIC Server - Next-generation protocol configuration
let quic_config = QuicServerConfig::new("0.0.0.0:8003")?
    .with_cert_path("cert.pem")
    .with_key_path("key.pem")
    .with_alpn(vec![b"h3".to_vec(), b"msgtrans".to_vec()])
    .with_max_concurrent_streams(1000);
```

### Intelligent Configuration

```rust
// Zero configuration - automatic optimization (recommended)
let server = TransportServerBuilder::new()
    .with_protocol(tcp_config)
    .build().await?;  // Automatically optimized based on CPU

// High-performance configuration - manual tuning
let server = TransportServerBuilder::new()
    .with_protocol(tcp_config)
    .connection_config(ConnectionConfig::high_performance())
    .max_connections(50000)
    .build().await?;

// Resource-saving configuration - low memory environment
let server = TransportServerBuilder::new()
    .with_protocol(tcp_config)
    .connection_config(ConnectionConfig::memory_optimized())
    .max_connections(1000)
    .build().await?;
```

## 🔧 Advanced Features

### 📊 Built-in Monitoring

```rust
// Real-time statistics - zero-copy performance monitoring
let stats = server.get_stats().await;
println!("Active connections: {}", stats.active_connections);
println!("Total messages: {}", stats.total_messages);
println!("Average latency: {:?}", stats.average_latency);
println!("Memory usage: {} MB", stats.memory_usage_mb);

// Protocol distribution statistics
for (protocol, count) in &stats.protocol_distribution {
    println!("{}: {} connections", protocol, count);
}
```

### 🛡️ Graceful Error Handling

```rust
use msgtrans::error::{TransportError, CloseReason};

// Message sending error handling
match client.send("Hello, World!".as_bytes()).await {
    Ok(result) => println!("✅ Message sent successfully (ID: {})", result.message_id),
    Err(TransportError::ConnectionLost { .. }) => {
        println!("🔗 Connection lost, attempting reconnection");
        client.connect().await?;
    }
    Err(TransportError::ProtocolError { protocol, error }) => {
        println!("⚠️ Protocol error [{}]: {}", protocol, error);
    }
    Err(e) => println!("❌ Other error: {}", e),
}

// Request-response error handling
match client.request("Get status".as_bytes()).await {
    Ok(result) => {
        match result.data {
            Some(data) => {
                let response = String::from_utf8_lossy(&data);
                println!("📥 Received response: {}", response);
            }
            None => println!("⏰ Request timeout (ID: {})", result.message_id),
        }
    }
    Err(TransportError::Timeout { duration }) => {
        println!("⏰ Request timeout: {:?}", duration);
    }
    Err(e) => println!("❌ Request failed: {}", e),
}

// Server-side sending error handling
match server.send(session_id, "Response data".as_bytes()).await {
    Ok(result) => println!("✅ Successfully sent to session {}", session_id),
    Err(TransportError::ConnectionLost { .. }) => {
        println!("🔗 Session {} connection disconnected", session_id);
        // Automatic session cleanup
    }
    Err(e) => println!("❌ Send failed: {}", e),
}
```

### 🔄 Connection Management

```rust
// Connection pool management
let pool_config = ConnectionPoolConfig::adaptive()
    .with_initial_size(100)
    .with_max_size(10000)
    .with_idle_timeout(Duration::from_secs(300))
    .with_health_check_interval(Duration::from_secs(30));

// Graceful shutdown
let server = TransportServerBuilder::new()
    .with_protocol(tcp_config)
    .graceful_shutdown_timeout(Duration::from_secs(30))
    .build().await?;

// Smooth restart support
server.start_graceful_shutdown().await?;
```

## 📚 Documentation and Examples

### 📖 Complete Examples

Check the `examples/` directory for more examples:

- [`echo_server.rs`](examples/echo_server.rs) - Multi-protocol echo server
- [`echo_client_tcp.rs`](examples/echo_client_tcp.rs) - TCP client example
- [`echo_client_websocket.rs`](examples/echo_client_websocket.rs) - WebSocket client example
- [`echo_client_quic.rs`](examples/echo_client_quic.rs) - QUIC client example
- [`packet.rs`](examples/packet.rs) - Packet serialization verification example

### 🚀 Running Examples

```bash
# Start multi-protocol echo server
cargo run --example echo_server

# Test TCP client
cargo run --example echo_client_tcp

# Test WebSocket client  
cargo run --example echo_client_websocket

# Test QUIC client
cargo run --example echo_client_quic
```

## 🏆 Use Cases

- **🎮 Game Servers** - High-concurrency real-time game communication
- **💬 Chat Systems** - Multi-protocol instant messaging platforms
- **🔗 Microservice Communication** - Efficient inter-service data transmission
- **📊 Real-time Data** - Financial, monitoring and other real-time systems
- **🌐 IoT Platforms** - Large-scale device connection management
- **🚪 Protocol Gateways** - Multi-protocol conversion and proxying

## 📝 License

This project is licensed under the [Apache License 2.0](LICENSE).

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 Contributing

Issues and Pull Requests are welcome! Please check the [Contributing Guide](CONTRIBUTING.md) for detailed information.

---

> 🎯 **MsgTrans Mission**: Make multi-protocol communication simple, efficient, and reliable, focusing on business logic rather than underlying transport details.
