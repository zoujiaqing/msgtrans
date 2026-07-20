# 🚀 MsgTrans - 现代化多协议通信框架

[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

🌐 语言：[English](README.md) | [简体中文](README.zh-CN.md)

> **现代化多协议通信框架，用统一接口封装 TCP、WebSocket、QUIC**

## 🌟 核心特性

### 🏗️ 统一架构

- **三层架构**：应用层 → 传输层 → 协议层，职责清晰分离
- **业务逻辑协议无关**：一套代码，多协议部署
- **配置驱动**：切换协议只改配置，不动业务逻辑
- **可插拔适配器**：实现 `Connection` trait 即可接入新协议

### ⚡ 现代并发

- **无锁内核**：按会话的 actor + 无锁 map，热路径上避免 Mutex 竞争
- **零拷贝报文**：`Packet` 承载 `Bytes` 载荷，尽可能直接交给网络层
- **事件驱动模型**：全异步、非阻塞
- **有界背压**：出站队列按连接有界，慢速对端既不会撑爆内存，也不会阻塞扇出循环

### 🔌 协议

- **TCP** - 可靠流式传输
- **WebSocket** - 实时 Web 通信
- **QUIC** - 基于 UDP 的现代传输
- **自定义协议** - 实现 `Connection` trait

### 🎯 极简 API

- **Builder 模式**：流畅、可读的配置
- **类型安全**：配置在编译期检查
- **合理默认值**：开箱即用，按需调优

## 🚀 快速开始

### 安装

```toml
[dependencies]
msgtrans = "1.0"
```

### 创建多协议服务端

```rust,no_run
use msgtrans::{
    transport::TransportServerBuilder,
    protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig},
    event::ServerEvent,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 配置多个协议 —— 同一套业务逻辑服务所有协议
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

    // 先订阅事件，避免遗漏
    let mut events = server.subscribe_events();

    // 启动监听。serve() 会一直运行到服务停止，因此把它 spawn 出去，
    // 在主任务里处理事件。
    let server_for_events = server.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ServerEvent::ConnectionEstablished { session_id, .. } => {
                    println!("新连接: {session_id}");
                }
                ServerEvent::MessageReceived { session_id, context } => {
                    // 回显 —— 协议透明
                    let response = format!("Echo: {}", String::from_utf8_lossy(&context.data));
                    let _ = server_for_events.send(session_id, response.as_bytes()).await;
                }
                ServerEvent::ConnectionClosed { session_id, .. } => {
                    println!("连接关闭: {session_id}");
                }
                _ => {}
            }
        }
    });

    server.serve().await?;
    Ok(())
}
```

### 创建客户端连接

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

    // 发送单向消息
    client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("消息已发送");

    // 发送请求并等待响应
    let result = client.request("What time is it?".as_bytes()).await?;
    if let Some(data) = result.data {
        println!("收到响应: {}", String::from_utf8_lossy(&data));
    } else {
        println!("请求超时");
    }

    // 消费事件
    let mut events = client.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ClientEvent::MessageReceived(context) => {
                    println!("收到: {}", String::from_utf8_lossy(&context.data));
                }
                ClientEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
    });

    Ok(())
}
```

## 🏗️ 架构设计

### 三层架构

```text
+-------------------------------------+
|  应用层                             |  <- 业务逻辑，协议无关
+-------------------------------------+
|  传输层                             |  <- 连接管理，统一 API
|  - TransportServer / TransportClient|     - 连接生命周期
|  - SessionActor                     |     - 事件路由
|  - RequestRegistry                  |     - 请求/响应生命周期
+-------------------------------------+
|  协议层                             |  <- 协议实现
|  - TCP / WebSocket / QUIC 适配器    |     - Connection trait
|  - 协议配置                         |     - 协议注册
+-------------------------------------+
```

### 设计原则

**统一抽象、协议透明** —— `TransportServer`/`TransportClient` 暴露一套业务接口；
每个适配器实现 `Connection` trait，隐藏协议细节。

**配置驱动** —— 同一套服务端代码可跑在任意协议上，只需改传给 `.with_protocol(..)` 的配置：

```rust,no_run
# use msgtrans::{transport::TransportServerBuilder, protocol::{TcpServerConfig, QuicServerConfig}};
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
// TCP 服务端
let server = TransportServerBuilder::new()
    .with_protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build().await?;

// QUIC 服务端 —— 业务逻辑完全相同
let server = TransportServerBuilder::new()
    .with_protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build().await?;
# Ok(()) }
```

### 事件驱动模型

```rust
use msgtrans::{
    event::{ServerEvent, ClientEvent},
    command::ConnectionInfo,
    error::{TransportError, CloseReason},
    SessionId, TransportContext,
};

// 服务端事件
# fn _server_events(ev: ServerEvent) { match ev {
ServerEvent::ConnectionEstablished { session_id, info } => { /* ... */ }
ServerEvent::MessageReceived { session_id, context } => { /* ... */ }
ServerEvent::MessageSent { session_id, message_id } => { /* ... */ }
ServerEvent::ConnectionClosed { session_id, reason } => { /* ... */ }
ServerEvent::TransportError { session_id, error } => { /* ... */ }
# _ => {} } }

// 客户端事件
# fn _client_events(ev: ClientEvent) { match ev {
ClientEvent::Connected { info } => { /* ... */ }
ClientEvent::MessageReceived(context) => { /* ... */ }
ClientEvent::MessageSent { message_id } => { /* ... */ }
ClientEvent::Disconnected { reason } => { /* ... */ }
ClientEvent::Error { error } => { /* ... */ }
# _ => {} } }
```

## ⚡ 使用模式

### 并发发送

`TransportServer` 可廉价克隆（内部通过 `Arc` 共享状态），因此可以 move 进 spawn
出的任务里做并发、无锁的会话访问：

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

### 请求 / 响应

```rust,no_run
# use msgtrans::transport::TransportClient;
# async fn f(client: &TransportClient) -> Result<(), Box<dyn std::error::Error>> {
let response = client.request(b"Get user data").await?;
if let Some(data) = response.data {
    println!("收到 {} 字节", data.len());
} else {
    println!("请求超时");
}
# Ok(()) }
```

## 🔌 协议扩展

要接入新协议，为你的适配器实现 `Connection` trait，再配一个对应的 config 类型。
完整可用的参考见内置的 `adapters::{tcp, websocket, quic}`；下面是骨架示意：

```rust,ignore
use msgtrans::{connection::Connection, packet::Packet, error::TransportError};

pub struct MyAdapter { /* 协议特有状态 */ }

#[async_trait::async_trait]
impl Connection for MyAdapter {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> { /* ... */ }
    // ... 其余 Connection 方法
}
```

## 📖 使用示例

### WebSocket 服务端

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

### QUIC 客户端

```rust,no_run
use msgtrans::{
    transport::TransportClientBuilder,
    protocol::QuicClientConfig,
    tokio,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 本地 / 自签名测试服务端：跳过证书校验。
    // 生产环境请去掉 danger_skip_verification()，改为配置真实的
    // server name 和 CA。
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
    println!("完成");
    Ok(())
}
```

## 🛠️ 配置选项

### 服务端配置

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

## 🔧 进阶功能

### 统计信息

```rust,no_run
# use msgtrans::transport::TransportServer;
# async fn f(server: &TransportServer) {
let active = server.session_count().await;
println!("活跃会话: {active}");
# }
```

### 错误处理

```rust,no_run
# use msgtrans::{transport::TransportClient, error::TransportError};
# async fn f(client: &mut TransportClient) -> Result<(), Box<dyn std::error::Error>> {
match client.send("Hello, World!".as_bytes()).await {
    Ok(result) => println!("已发送 (ID: {})", result.message_id),
    Err(TransportError::Connection { .. }) => {
        println!("连接丢失，重连中");
        client.connect().await?;
    }
    Err(TransportError::Protocol { protocol, reason }) => {
        println!("协议错误 [{protocol}]: {reason}");
    }
    Err(e) => println!("其他错误: {e}"),
}
# Ok(()) }
```

### 优雅关闭

```rust,no_run
use msgtrans::{transport::TransportServerBuilder, protocol::TcpServerConfig};
use std::time::Duration;

# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .with_protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .graceful_shutdown(Some(Duration::from_secs(30)))
    .build().await?;

// ... 稍后，按配置的超时排空活跃会话：
server.stop().await;
# Ok(()) }
```

## 📚 文档与示例

[`examples/`](examples/) 目录包含完整可运行的程序：

- [`echo_server.rs`](examples/echo_server.rs) - 多协议回显服务端
- [`echo_client_tcp.rs`](examples/echo_client_tcp.rs) - TCP 客户端
- [`echo_client_websocket.rs`](examples/echo_client_websocket.rs) - WebSocket 客户端
- [`echo_client_quic.rs`](examples/echo_client_quic.rs) - QUIC 客户端
- [`load_test.rs`](examples/load_test.rs) / [`load_test_server.rs`](examples/load_test_server.rs) - 压力测试
- [`packet.rs`](examples/packet.rs) - 报文序列化

```bash
# 启动多协议回显服务端
cargo run --example echo_server

# 另开一个终端，运行客户端
cargo run --example echo_client_tcp
```

## 🏆 适用场景

- **游戏服务器** - 高并发实时通信
- **聊天系统** - 多协议即时消息
- **微服务通信** - 高效的服务间传输
- **实时数据** - 金融、监控、遥测等系统
- **IoT 平台** - 大规模设备连接管理
- **协议网关** - 多协议转换与代理

## 📝 许可

基于 [Apache License 2.0](LICENSE) 授权。

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 贡献

欢迎提交 Issue 和 Pull Request。
