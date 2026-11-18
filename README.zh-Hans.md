# 🚀 MsgTrans - 现代化多协议通信框架

[![Rust](https://img.shields.io/badge/rust-1.88+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

[English](https://github.com/zoujiaqing/msgtrans/blob/main/README.md) | **简体中文** | [繁體中文](https://github.com/zoujiaqing/msgtrans/blob/main/README.zh-Hant.md) | [Français](https://github.com/zoujiaqing/msgtrans/blob/main/README.fr-FR.md) | [Deutsch](https://github.com/zoujiaqing/msgtrans/blob/main/README.de.md) | [Español](https://github.com/zoujiaqing/msgtrans/blob/main/README.es.md) | [Português (Brasil)](https://github.com/zoujiaqing/msgtrans/blob/main/README.pt-BR.md) | [Українська](https://github.com/zoujiaqing/msgtrans/blob/main/README.uk.md) | [Русский](https://github.com/zoujiaqing/msgtrans/blob/main/README.ru.md) | [日本語](https://github.com/zoujiaqing/msgtrans/blob/main/README.ja-JP.md) | [한국어](https://github.com/zoujiaqing/msgtrans/blob/main/README.ko-KR.md) | [Tiếng Việt](https://github.com/zoujiaqing/msgtrans/blob/main/README.vi-VN.md) | [Bahasa Indonesia](https://github.com/zoujiaqing/msgtrans/blob/main/README.id.md) | [العربية](https://github.com/zoujiaqing/msgtrans/blob/main/README.ar.md)

> **现代化多协议通信框架，用统一接口封装 TCP、WebSocket、QUIC**

## 🌟 核心特性

### 🏗️ 统一架构

- **三层架构**：应用层 → 传输层 → 协议层，职责清晰分离
- **业务逻辑协议无关**：一套代码，多协议部署
- **配置驱动**：切换协议只改配置，不动业务逻辑
- **可插拔适配器**：实现 `Connection` trait 即可接入新协议

### ⚡ 现代并发

- **按会话独立处理**：业务回调在各自的 actor 内运行，慢回调可能延迟自身会话中的后续工作
- **零拷贝报文**：`Packet` 承载 `Bytes` 载荷，尽可能直接交给网络层
- **事件驱动模型**：全异步、非阻塞
- **有界背压**：出站队列按连接有界，慢速对端既不会撑爆内存，也不会阻塞扇出循环

### 🔌 协议

- **TCP**：适合可直连的原生客户端和服务间通信，按需启用 TLS。
- **WebSocket**：适合浏览器和支持 WebSocket Upgrade 的 HTTP 网关，生产环境使用 WSS。
- **QUIC**：适合 UDP 可用、网络条件易变的原生客户端。协商 `msgtrans/2` ALPN 后，服务端可以先发，不增加应用层往返。
- **自定义协议** - 实现 `Connection` trait

### 🎯 极简 API

- **Builder 模式**：流畅、可读的配置
- **类型安全**：配置在编译期检查
- **合理默认值**：开箱即用，按需调优

## 🚀 快速开始

### 安装

Rust 库最低支持 Rust 1.88。运行下面的服务端示例时，还需直接依赖 `tokio` 和 `async-trait`。

```toml
[dependencies]
msgtrans = "2.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
```

### 创建多协议服务端

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    SessionHandler, SessionSender, TransportServerBuilder,
    TcpServerConfig, WebSocketServerConfig, QuicServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

// 业务逻辑写在 handler 里。每个连接有自己的 actor 来调用它，
// 所以处理慢只会拖慢它自己那条连接，而不会让所有人丢消息。
struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        // 原样回显 —— 协议无关。
        let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
        let _ = sender.send_data(response.into_bytes()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 配置多种协议 —— 同一套业务逻辑服务所有协议。
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

    // 一直运行直到服务器停止。
    server.serve().await?;
    Ok(())
}
```

### 创建客户端连接

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

    // 消费事件
    let mut events = client.events().await?;
    let event_task = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                ClientEvent::Message(msg) => {
                    println!("收到: {}", msg.as_text_lossy());
                }
                ClientEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
    });

    // 发送单向消息
    client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("消息已发送");

    // 发送请求并等待响应
    // `Ok` 即响应字节；任何失败（含超时）都是 `Err`。
    let response = client.request("What time is it?".as_bytes()).await?;
    println!("收到响应: {}", String::from_utf8_lossy(&response));

    client.shutdown().await?;
    event_task.await?;
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

**配置驱动** —— 同一套服务端代码可跑在任意协议上，只需改传给 `.protocol(..)` 的配置：

```rust,no_run
# use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, QuicServerConfig, Packet, SessionId};
# use std::sync::Arc;
# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
# let handler = Arc::new(H);
// TCP 服务端
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build(handler.clone()).await?;

// QUIC 服务端 —— 业务逻辑完全相同
let server = TransportServerBuilder::new()
    .protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build(handler).await?;
# Ok(()) }
```

### Handler 模型

服务端把每个会话的流量交给 `SessionHandler`；客户端仍然是事件流，
因为一个客户端只有一条连接。

```rust
use msgtrans::{
    ClientEvent,
    ConnectionInfo,
    TransportError, CloseReason,
    Packet,
    SessionHandler, SessionSender,
    SessionId,
};

// 服务端：实现 handler，只有 on_message 是必须的。
struct MyHandler;

#[async_trait::async_trait]
impl SessionHandler for MyHandler {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender) { /* ... */ }
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) { /* ... */ }
    async fn on_disconnected(&self, session_id: SessionId, reason: CloseReason) { /* ... */ }
    async fn on_error(&self, session_id: SessionId, error: TransportError) { /* ... */ }
}

// 客户端：事件
# fn _client_events(ev: ClientEvent) { match ev {
ClientEvent::Connected { info } => { /* ... */ }
ClientEvent::Message(msg) => { /* 单向消息 */ }
ClientEvent::Request(req) => { /* 请求：必须且只能应答一次 */ }
ClientEvent::MessageSent { message_id } => { /* ... */ }
ClientEvent::Disconnected { reason } => { /* ... */ }
ClientEvent::Error { error } => { /* ... */ }
# _ => {} } }
```

## ⚡ 使用模式

### 并发发送

`TransportServer` 可廉价克隆（内部通过 `Arc` 共享状态），因此可以 move 进 spawn
出的任务里做并发访问会话：

```rust,no_run
# use msgtrans::{TransportServer, SessionHandler, SessionSender, Packet, SessionId};
# use std::sync::Arc;
struct Echo { server: TransportServer }

#[async_trait::async_trait]
impl SessionHandler for Echo {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

    async fn on_message(&self, session_id: SessionId, packet: Packet, _tx: SessionSender) {
        // 丢给 spawn 出去的任务处理，让这个会话的 actor 能立刻去取下一条消息。
        let server = self.server.clone();
        tokio::spawn(async move {
            let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
            let _ = server.send(session_id, response.as_bytes()).await;
        });
    }
}
```

### 请求 / 响应

```rust,no_run
# use msgtrans::TransportClient;
# async fn f(client: &TransportClient) -> Result<(), Box<dyn std::error::Error>> {
// `Ok` 即响应字节；任何失败（含超时）都是 `Err`。
let response = client.request(b"Get user data").await?;
println!("收到 {} 字节", response.len());
# Ok(()) }
```

## 🔌 协议扩展

要接入新协议，为你的适配器实现 `Connection` trait，再配一个对应的 config 类型。
完整可用的参考见内置的 `adapters::{tcp, websocket, quic}`；下面是骨架示意：

```rust
use msgtrans::spi::{
    event_channel, CloseReason, Connection, ConnectionEvents, ConnectionInfo, ConnectionWriter,
    EventSink, Packet, SessionId, TransportError, WriteCompletion,
};
use msgtrans::FramePolicy;
use std::sync::Arc;

/// 写半边。与连接对象分开，是为了让传输层能在连接锁内克隆它、**先释放锁**再 await
/// 入队——队列打满时绝不能把 reconnect/close/shutdown 堵在后面。
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
        // 把**包和它的 completion 一起**交给写循环。completion 只能在字节
        // 真正写入 socket 之后才被 resolve——在这里 resolve 就是**伪造写入确认**，
        // 而 `WriteCompletion` 存在的全部意义就是让 `Ok` 可被证明。
        //
        // 队列没了就**丢弃** completion：丢弃本身即上报失败，
        // 所以不存在任何凭空造 `Ok` 的路径。
        self.queue
            .send((packet, completion))
            .await
            .map_err(|_| TransportError::connection_error("connection closed", false))
    }
}

/// 持有 socket 的写循环。只有它有资格声称一次写入成功。
async fn write_loop(mut queue: tokio::sync::mpsc::Receiver<(Packet, WriteCompletion)>) {
    while let Some((packet, completion)) = queue.recv().await {
        let bytes = match packet.try_encode() {
            Ok(bytes) => bytes,
            Err(e) => {
                completion.complete(Err(TransportError::protocol_error("encode", e.to_string())));
                continue;
            }
        };
        // 换成你真实的 socket 写；如实上报它返回了什么。
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

    /// 你的读循环把入站包推到这里。管道会统一解压/规范化，
    /// 所以每个消费者拿到的都是明文。
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
    fn set_frame_policy(&self, _policy: FramePolicy) { /* 在这里落实 Strict */ }
}
```

## 📖 使用示例

### WebSocket 服务端

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    SessionHandler, SessionSender, TransportServerBuilder,
    WebSocketServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

struct Chat;

#[async_trait]
impl SessionHandler for Chat {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        let msg = String::from_utf8_lossy(packet.payload());
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

### QUIC 客户端

```rust,no_run
use msgtrans::{
    TransportClientBuilder,
    QuicClientConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 本地 / 自签名测试服务端：跳过证书校验。
    // 生产环境请去掉 danger_skip_verification()，改为配置真实的
    // server name 和 CA。
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
    println!("完成");
    Ok(())
}
```

## 🛠️ 配置选项

### 服务端配置

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

## 🔧 进阶功能

### 统计信息

```rust,no_run
# use msgtrans::TransportServer;
# async fn f(server: &TransportServer) {
let active = server.session_count().await;
println!("活跃会话: {active}");
# }
```

### 错误处理

```rust,no_run
# use msgtrans::{TransportClient, TransportError};
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
use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, Packet, SessionId};
use std::sync::Arc;

# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
    // `on_request` 是必需方法：请求带着「必须应答一次」的义务，
    // 只能通过消费式 `Responder` 应答。
    async fn on_request(&self, _s: SessionId, req: Packet, responder: msgtrans::Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }

#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .build(Arc::new(H)).await?;

// ... 稍后，停止接入并等待受监管的任务退出：
let report = server.shutdown().await;
assert!(report.clean, "server shutdown did not finish before the deadline");
# Ok(()) }
```

## Kotlin 和 Swift 原生绑定

[`msgtrans-ffi`](https://github.com/zoujiaqing/msgtrans/tree/main/crates/msgtrans-ffi) 基于 UniFFI，把 MsgTrans 2.0 **客户端**提供给 Kotlin（Android、JVM、Kotlin Multiplatform）和 Swift（iOS、macOS）。支持 TCP（含 TLS）、WebSocket、QUIC，以及单向消息、请求与响应、应答服务端主动发起的请求。服务端由 Rust 库提供，不在 FFI 绑定范围内。

绑定直接使用同一工作区的 Rust crate，与其版本一致。交付物是原生库和生成的 Kotlin／Swift 绑定，不单独发布到 crates.io。工作区用 Rust 1.99.0 构建这些产物；Rust `msgtrans` 库本身最低支持 Rust 1.88。

在 `crates/msgtrans-ffi` 下执行：

| 目标 | 构建命令 | 产物 |
|---|---|---|
| Android | `ANDROID_NDK_HOME=/path/to/ndk scripts/build-android.sh` | 默认生成 `dist/android/jniLibs/arm64-v8a/libmsgtrans_ffi.so` |
| iOS | `scripts/build-ios.sh` | `dist/ios/MsgtransFFI.xcframework`，支持真机和 Apple 芯片模拟器 |
| Kotlin 和 Swift 绑定 | `scripts/gen-bindings.sh` | 生成的源码位于 `bindings/` |

平台要求、Kotlin 和 Swift 示例、证书验证及事件循环关闭方式，见 [FFI 构建与接入指南](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans-ffi/README.md)。浏览器应用可使用独立的 [TypeScript WebSocket 客户端](https://github.com/zoujiaqing/msgtrans-typescript)。

## 升级到 2.0

参阅 [1.x 迁移指南](https://github.com/zoujiaqing/msgtrans/blob/main/docs/MIGRATION_2.0.md)、[2.0 API 指南](https://github.com/zoujiaqing/msgtrans/blob/main/docs/API_2.0.md)和[线协议格式](https://github.com/zoujiaqing/msgtrans/blob/main/docs/WIRE_FORMAT.md)。报文布局不变，但 Rust API 和建连规则有更新。默认严格解码；不支持未提供 ALPN 的 QUIC 对端，例如 MsgTrans 1.0.10。


## 📚 文档与示例

[`examples/`](https://github.com/zoujiaqing/msgtrans/tree/main/crates/msgtrans/examples/) 目录包含完整可运行的程序：

- [`echo_server.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_server.rs) - 多协议回显服务端
- [`echo_client_tcp.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_tcp.rs) - TCP 客户端
- [`echo_client_websocket.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_websocket.rs) - WebSocket 客户端
- [`echo_client_quic.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/echo_client_quic.rs) - QUIC 客户端
- [`load_test.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/load_test.rs) / [`load_test_server.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/load_test_server.rs) - 压力测试
- [`packet.rs`](https://github.com/zoujiaqing/msgtrans/blob/main/crates/msgtrans/examples/packet.rs) - 报文序列化

```bash
# 启动多协议回显服务端
cargo run -p msgtrans --example echo_server

# 另开一个终端，运行客户端
cargo run -p msgtrans --example echo_client_tcp
```

## 🏆 适用场景

- **游戏服务器** - 高并发实时通信
- **聊天系统** - 多协议即时消息
- **微服务通信** - 高效的服务间传输
- **实时数据** - 金融、监控、遥测等系统
- **IoT 平台** - 大规模设备连接管理
- **协议网关** - 多协议转换与代理

## 📝 许可

基于 [Apache License 2.0](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE) 授权。

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 贡献

欢迎提交 Issue 和 Pull Request。
