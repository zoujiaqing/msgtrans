# msgtrans v1.0.6 架构规范

msgtrans 是一个 **Rust 异步多协议传输库**，为应用层提供统一的 TCP / WebSocket / QUIC 传输接口。
协议通过 `DynServerConfig` / `DynClientConfig` trait 扩展，框架不硬编码任何协议名称。

## 设计原则

| 原则 | 落地方式 |
|------|---------|
| 零拷贝 | `Bytes` / `BytesMut` + `SharedPacket` / `ArcPacket` |
| 无锁优先 | `DashMap`、`flume`、原子类型封装的 `LockFreeHashMap` / `LockFreeQueue` / `LockFreeCounter` |
| 单锁热路径 | `Transport.send()` 仅一次 `Mutex::lock` |
| 单一连接 trait | 适配器直接实现 `Connection` trait，无中间 wrapper |
| Actor 模型 | 每连接一个 `SessionActor`，通过 `ActorMessage` 统一信箱，`SessionHandler` trait 提供业务回调 |
| 协议无关 | `Connection` / `Server` trait 抽象 + `DynServerConfig` / `DynClientConfig` 可扩展协议 |
| 依赖注入 | `TransportContext` 持有共享资源，构造时显式传入 |
| 缓冲区池化 | 适配器读/写缓冲区从 `OptimizedMemoryPool` 分配，连接关闭时回收 |

---

## 依赖矩阵

| 依赖 | 用途 |
|------|------|
| `tokio` 1.x | 异步运行时（rt-multi-thread, io, time, sync, macros） |
| `flume` 0.12 | 无锁 bounded/unbounded 通道 → `LockFreeQueue`、`SessionActor` 信箱 |
| `dashmap` 5.x | 并发哈希表 → `LockFreeHashMap` |
| `bytes` 1.x | 零拷贝缓冲区 |
| `quinn` 0.11 | QUIC 协议实现 |
| `tungstenite` + `tokio-tungstenite` | WebSocket 协议 |
| `rustls` 0.23 | TLS 层 |
| `rcgen` 0.13 | 开发环境自签证书 |
| `chrono`, `serde`, `serde_json` | 时间戳、序列化 |
| `tracing` | 结构化日志 |
| `anyhow`, `thiserror` | 错误处理 |

特性开关：`tcp`（默认）、`websocket`（默认）、`quic`（默认）

---

## 分层架构

```
┌──────────────────────────────────────────────────────┐
│                    应用层 (Application)                │
│  TransportClientBuilder / TransportServerBuilder      │
│  SessionHandler trait 回调                            │
├──────────────────────────────────────────────────────┤
│                   会话层 (Session)                     │
│  SessionActor — 每连接独立 actor                       │
│  ActorMessage — 统一信箱（入站事件 + 出站发送 + 控制）    │
│  SessionSender / Responder — 回写能力                  │
│  RequestTracker — 请求/响应匹配                        │
├──────────────────────────────────────────────────────┤
│                   传输层 (Transport)                   │
│  TransportContext — 共享资源上下文 (DI)                 │
│  Transport — 单连接抽象（单锁发送，同步构造）            │
│  TransportServer — 多会话管理                          │
│  TransportClient — 客户端连接管理                      │
├──────────────────────────────────────────────────────┤
│                   协议层 (Protocol)                    │
│  Connection / Server trait                            │
│  DynServerConfig / DynClientConfig — 可扩展协议配置    │
│  TcpAdapter / WebSocketAdapter / QuicAdapter          │
│  ProtocolRegistry + ProtocolFactory                   │
├──────────────────────────────────────────────────────┤
│                   基础设施 (Infra)                     │
│  LockFreeHashMap / LockFreeQueue / LockFreeCounter    │
│  LockFreeConnection                                   │
│  OptimizedMemoryPool（池化读写缓冲区）                  │
│  ConnectionPool（独立组件）                             │
│  Packet / FixedHeader / SharedPacket / ArcPacket      │
└──────────────────────────────────────────────────────┘
```

---

## 模块拓扑

```
src/
├── lib.rs                      # 公开 API 导出
├── packet.rs                   # Packet 协议 + 零拷贝变体
├── error.rs                    # TransportError
├── event.rs                    # TransportEvent / ServerEvent / ClientEvent
├── connection.rs               # Connection / Server / ConnectionFactory trait
├── plugin.rs                   # ProtocolPlugin / PluginManager
├── adapters/
│   ├── factories.rs            # ProtocolFactory 实现 + Server wrappers
│   ├── tcp.rs                  # TcpAdapter<C>: Connection（池化 OptimizedReadBuffer）
│   ├── websocket.rs            # WebSocketAdapter<C>: Connection
│   └── quic.rs                 # QuicAdapter<C>: Connection（池化写缓冲区）
├── protocol/
│   ├── adapter.rs              # DynServerConfig / DynClientConfig / ServerConfig / ClientConfig
│   ├── client_config.rs        # TcpClientConfig / WebSocketClientConfig / QuicClientConfig
│   └── server_config.rs        # TcpServerConfig / WebSocketServerConfig / QuicServerConfig
└── transport/
    ├── context.rs              # TransportContext（DI，与 shared pool 统一）
    ├── transport.rs            # Transport（同步构造，with_context）+ RequestTracker
    ├── transport_server.rs     # TransportServer
    ├── server.rs               # TransportServerBuilder
    ├── client.rs               # TransportClientBuilder / TransportClient
    ├── session_actor.rs        # SessionActor + SessionHandler + ActorMessage + SessionSender + Responder
    ├── request_manager.rs      # RequestManager（lock-free 请求管理器）
    ├── lockfree.rs             # LockFreeHashMap / LockFreeQueue / LockFreeCounter
    ├── lockfree_connection.rs  # LockFreeConnection
    ├── memory_pool.rs          # OptimizedMemoryPool + shared_memory_pool()
    ├── pool.rs                 # ConnectionPool（独立可选组件）
    ├── config.rs               # TransportConfig
    ├── mod.rs                  # TransportOptions + 统一 re-export
    └── connection_state.rs     # ConnectionStateManager
```

---

## 包协议（Packet Protocol）

### 固定头部（16 bytes, big-endian）

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       total_length (u32)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  ext_len(u16) |  pkt_type(u8) | compress(u8)  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       message_id (u32)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    biz_type (u16)   | reserved / flags (u16)                  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| 字段 | 大小 | 说明 |
|------|------|------|
| total_length | u32 | 整包长度（含头部） |
| extended_length | u16 | 扩展头部长度 |
| packet_type | u8 | Request=1, Response=2, OneWay=3 |
| compression | u8 | None=0, Gzip=1, Zstd=2, Lz4=3 |
| message_id | u32 | 请求/响应配对标识 |
| biz_type | u16 | 业务类型码 |
| reserved | u16 | 保留位（含 fragment / priority 标志） |

### 零拷贝变体

| 类型 | 用途 |
|------|------|
| `Packet` | 拥有所有权的标准包，`payload: Vec<u8>` |
| `SharedPacket` | 内部 `Bytes` 引用计数，适合广播 |
| `ArcPacket` | `Arc<Packet>`，跨 task 共享无需序列化 |
| `MessageIdManager` | 全局原子递增 ID 生成 |

---

## Connection trait

### 定义

```rust
#[async_trait]
pub trait Connection: Send + Sync + Any {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError>;
    async fn close(&mut self) -> Result<(), TransportError>;
    fn session_id(&self) -> SessionId;
    fn set_session_id(&mut self, session_id: SessionId);
    fn connection_info(&self) -> ConnectionInfo;
    fn is_connected(&self) -> bool;
    async fn flush(&mut self) -> Result<(), TransportError>;
    fn event_stream(&self) -> Option<broadcast::Receiver<TransportEvent>>;
}
```

### Server trait

```rust
#[async_trait]
pub trait Server: Send + Sync {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError>;
    fn local_addr(&self) -> Result<SocketAddr, TransportError>;
    async fn shutdown(&mut self) -> Result<(), TransportError>;
}
```

### 适配器实现

每个协议适配器用单个泛型 impl 覆盖客户端和服务端配置：

```rust
impl<C: Send + Sync + 'static> Connection for TcpAdapter<C> { ... }
impl<C: Send + Sync + 'static> Connection for WebSocketAdapter<C> { ... }
impl<C: Send + Sync + 'static> Connection for QuicAdapter<C> { ... }
```

---

## 可扩展协议配置

协议通过 trait 接入，不硬编码协议名称。内置 TCP / WebSocket / QUIC，用户可实现自定义协议。

### 核心 trait

```rust
/// 服务端协议配置（object-safe）
pub trait DynServerConfig: DynProtocolConfig {
    fn build_server_dyn(&self) -> Pin<Box<dyn Future<Output = Result<Box<dyn Server>, TransportError>> + Send + '_>>;
    fn get_bind_address(&self) -> SocketAddr;
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig>;
}

/// 客户端协议配置（object-safe）
pub trait DynClientConfig: DynProtocolConfig {
    fn build_connection_dyn(&self) -> Pin<Box<dyn Future<Output = Result<Box<dyn Connection>, TransportError>> + Send + '_>>;
    fn get_target_info(&self) -> String;
    fn clone_client_dyn(&self) -> Box<dyn DynClientConfig>;
}
```

### 扩展方式

实现 `DynServerConfig` / `DynClientConfig` 即可接入 Builder：

```rust
// 自定义协议
struct MyProtocolServerConfig { ... }
impl DynProtocolConfig for MyProtocolServerConfig { ... }
impl DynServerConfig for MyProtocolServerConfig { ... }

// 接入 Builder
TransportServerBuilder::new()
    .with_protocol(MyProtocolServerConfig { ... })
    .build().await?;
```

---

## TransportContext 依赖注入

### 结构

```rust
#[derive(Clone)]
pub struct TransportContext {
    pub(crate) protocol_registry: Arc<ProtocolRegistry>,
    pub(crate) memory_pool: Arc<OptimizedMemoryPool>,
}
```

### 构造

```rust
TransportContext::new()                     // 使用全局共享池
TransportContext::with_memory_pool(pool)    // 注册自定义池到全局后使用
```

### 资源传播路径

```
TransportServerBuilder.build()
        │
        ├── TransportContext::new().await?
        │       └── shared_memory_pool()  ← 全局唯一池
        │
        └── TransportServer { context: Arc<ctx> }
                │
                ├── add_session()
                │       └── Transport::with_context(config, &self.context)
                │
                └── start_protocol_tasks()
                        └── adapters event_loop 使用 shared_memory_pool()
                            → 同一个 pool 实例
```

---

## Transport 核心

### 结构

```rust
pub struct Transport {
    config: TransportConfig,
    protocol_registry: Arc<ProtocolRegistry>,
    memory_pool: Arc<OptimizedMemoryPool>,
    connection: Arc<Mutex<Option<Box<dyn Connection>>>>,
    session_id: Arc<Mutex<Option<SessionId>>>,
    state_manager: ConnectionStateManager,
    event_sender: broadcast::Sender<TransportEvent>,
    request_tracker: Arc<RequestTracker>,
}
```

### 构造

```rust
Transport::with_context(config: TransportConfig, ctx: &TransportContext) -> Self
```

### 发送路径（Hot Path）

在 **Actor 模式** 下，服务端发送经 actor 信箱序列化，避免全局锁竞争：

```
TransportServer.send_to_session(session_id, packet)
    │
    ├── [Actor 模式] session_handles.get() → handle.send_packet_with_reply(packet)
    │       │
    │       └── flume channel → SessionActor → Transport.send(packet)
    │                                              │
    │                                              ├── connection.lock().await  ← 每连接串行，无竞争
    │                                              └── conn.send(packet).await
    │
    └── [Legacy 模式] transports.get() → transport.send(packet)
            │
            ├── connection.lock().await
            └── conn.send(packet).await
```

### 连接设置

| 方法 | 适用场景 | 行为 |
|------|---------|------|
| `set_connection()` | TransportClient | 存储连接 + 启动事件消费循环 |
| `set_connection_no_consumer()` | TransportServer | 仅存储连接，服务端自管事件路由 |

---

## SessionActor 模型

### 架构

```
                Connection broadcast
                       │
                       ▼
              TransportServer event loop
               ┌───────┴────────┐
               │ request_tracker│          TransportServer.send_to_session()
               │    .complete() │                    │
               └───────┬────────┘                    │
                       │ (非 Response 或未匹配)        │
                       ▼                              ▼
                ┌──────────────────────────────────────┐
                │         flume::bounded channel        │
                │         (ActorMessage 信箱)           │
                └──────────────┬───────────────────────┘
                               │
                               ▼
                         SessionActor
                    (批量处理, BATCH_SIZE=64)
                               │
                     ┌─────────┼──────────┐
                     ▼         ▼          ▼
              InboundEvent    Send    SendWithReply    Close
                     │         │          │
                     ▼         ▼          ▼
              SessionHandler  Transport.send()
              回调
```

### ActorMessage 枚举

Actor 信箱统一承载入站事件和出站命令，消除发送端对 Connection Mutex 的竞争：

```rust
#[derive(Debug)]
pub enum ActorMessage {
    /// 入站：连接读取任务产生的传输事件
    InboundEvent(TransportEvent),
    /// 出站：通过此连接发送数据包（fire-and-forget）
    Send(Packet),
    /// 出站：发送数据包并通知调用方结果
    SendWithReply {
        packet: Packet,
        reply: tokio::sync::oneshot::Sender<Result<(), TransportError>>,
    },
    /// 控制：优雅关闭连接
    Close,
}
```

### SessionHandler trait

业务层实现此 trait 接收消息。只有 `on_message` 是必须实现的，其余方法有默认空实现：

```rust
#[async_trait]
pub trait SessionHandler: Send + Sync + 'static {
    /// 收到消息时回调（必须实现）
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender);

    /// 连接建立时回调（可选）
    async fn on_connected(&self, session_id: SessionId) { }

    /// 连接断开时回调（可选）
    async fn on_disconnected(&self, session_id: SessionId, reason: CloseReason) { }

    /// 传输错误时回调（可选）
    async fn on_error(&self, session_id: SessionId, error: TransportError) { }
}
```

### SessionSender

随每次 `on_message` 回调传入，提供向当前会话回写消息的能力：

```rust
#[derive(Clone)]
pub struct SessionSender {
    session_id: SessionId,
    transport: Arc<Transport>,
}

impl SessionSender {
    pub async fn send(&self, packet: Packet) -> Result<(), TransportError>;
    pub async fn send_data(&self, data: Vec<u8>) -> Result<(), TransportError>;
    pub async fn respond(&self, message_id: u32, biz_type: u8, data: Vec<u8>) -> Result<(), TransportError>;
    pub fn session_id(&self) -> SessionId;
}
```

### Responder

用于请求-响应模式，确保每个请求只响应一次（`AtomicBool` 防重复）：

```rust
pub struct Responder {
    session_id: SessionId,
    message_id: u32,
    biz_type: u8,
    transport: Arc<Transport>,
    responded: AtomicBool,
}

impl Responder {
    pub async fn respond(self, data: Vec<u8>) -> Result<(), TransportError>;
    pub fn session_id(&self) -> SessionId;
    pub fn message_id(&self) -> u32;
}
```

### SessionHandle

`TransportServer` 持有的 actor 句柄，用于将事件和发送命令投递到 actor 信箱：

```rust
#[derive(Clone)]
pub struct SessionHandle {
    tx: Sender<ActorMessage>,
    transport: Arc<Transport>,
}

impl SessionHandle {
    /// 转发入站事件到 actor
    pub async fn send_event(&self, event: TransportEvent) -> Result<...>;
    /// 发送数据包（fire-and-forget）
    pub async fn send_packet(&self, packet: Packet) -> Result<(), TransportError>;
    /// 发送数据包并等待结果
    pub async fn send_packet_with_reply(&self, packet: Packet) -> Result<(), TransportError>;
}
```

### 批量处理

Actor 使用批量接收优化吞吐量：

```rust
const BATCH_SIZE: usize = 64;

// 等待第一条消息，然后非阻塞 drain 最多 BATCH_SIZE 条
match self.rx.recv_async().await {
    Ok(first) => batch.push(first),
    Err(_) => break,
}
while batch.len() < BATCH_SIZE {
    match self.rx.try_recv() {
        Ok(next) => batch.push(next),
        Err(_) => break,
    }
}
```

效果：
- 减少 await/wake 周期（最多 64x）
- 减少 channel 锁竞争
- 更好的 CPU cache 利用率

---

## 内存池

### 池结构

```rust
pub struct OptimizedMemoryPool {
    small_buffers:  LockFreeQueue<BytesMut>,   // ≤ 1KB
    medium_buffers: LockFreeQueue<BytesMut>,   // ≤ 8KB
    large_buffers:  LockFreeQueue<BytesMut>,   // ≤ 64KB
    stats: MemoryPoolStats,
    event_sender: broadcast::Sender<MemoryPoolEvent>,
}
```

### 适配器集成

| 协议 | 缓冲区 | 池化方式 |
|------|--------|---------|
| TCP | `OptimizedReadBuffer` (8KB) | `new_with_pool()` 从池获取，`Drop` 自动归还 |
| QUIC | 写批量缓冲区 (64KB) | `get_buffer(Large)` 获取，loop 退出后 `return_buffer()` |
| WebSocket | tungstenite 内部管理 | 不涉及 |

### 全局池管理

```rust
init_shared_memory_pool(pool)    // 注册自定义池
shared_memory_pool()             // 获取全局池（lazy-init）
```

`TransportContext::new()` 调用 `shared_memory_pool()` 获取全局唯一实例。
`TransportContext::with_memory_pool()` 调用 `init_shared_memory_pool()` 注册自定义池。

---

## 事件系统

### 服务端事件流（Actor 模式）

```
Connection event_loop ──broadcast──► TransportServer event task
                                          │
                                    request_tracker.complete()
                                    (拦截 Response 完成服务端请求)
                                          │
                                    SessionHandle.send_event()
                                          │
                                    flume::bounded channel
                                          │
                                    SessionActor.run()
                                          │
                                    SessionHandler 回调
                                    ├── on_connected()
                                    ├── on_message(session_id, packet, sender)
                                    ├── on_disconnected(session_id, reason)
                                    └── on_error(session_id, error)
```

### 服务端事件流（Legacy 模式）

```
Connection event_loop ──broadcast──► TransportServer event task
                                          │
                                    handle_transport_event()
                                          │
                                    ServerEvent broadcast
                                    ├── ConnectionEstablished
                                    ├── MessageReceived { session_id, context }
                                    ├── ConnectionClosed
                                    └── TransportError
```

### 客户端事件流

```
Connection event_loop ──broadcast──► Transport.on_event()
                                          │
                                    request_tracker.complete()
                                          │
                                    event_sender.send()
                                          │
                                    TransportClient forwarding
                                          │
                                    ClientEvent broadcast
```

---

## 无锁基础设施

| 组件 | 底层 | API |
|------|------|-----|
| `LockFreeHashMap<K,V>` | `DashMap<K,V>` | insert, get, remove, iter, len |
| `LockFreeQueue<T>` | `flume` MPMC | push, pop |
| `LockFreeCounter` | `AtomicUsize` | increment, decrement, get, reset |
| `LockFreeConnection` | flume + AtomicU8 | 消息传递代替锁 |

---

## 通道类型选择

| 通道 | 使用位置 | 理由 |
|------|---------|------|
| `flume::bounded` | `SessionActor` 信箱（`ActorMessage`）、`LockFreeQueue`、`LockFreeConnection` | 无锁 MPMC，sync/async 双模，~50ns push |
| `tokio::sync::mpsc` | 适配器 send_queue, shutdown | 与 `tokio::select!` 原生集成 |
| `tokio::sync::broadcast` | 事件广播（TransportEvent, ServerEvent, ClientEvent） | 1-to-N pub/sub |
| `tokio::sync::oneshot` | `SendWithReply` 回执、`RequestTracker` 请求/响应 | 一次性通知 |

---

## 多协议支持

| 协议 | 适配器 | 缓冲区池化 |
|------|--------|-----------|
| TCP | `TcpAdapter<C>: Connection` | 读缓冲区池化 (Medium 8KB) |
| WebSocket | `WebSocketAdapter<C>: Connection` | N/A |
| QUIC | `QuicAdapter<C>: Connection` | 写缓冲区池化 (Large 64KB) |

---

## Builder API

### 服务端

Builder 使用 `.with_protocol()` 泛型方法接入协议，支持任意实现了 `DynServerConfig` 的类型：

```rust
use msgtrans::protocol::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig};

let server = TransportServerBuilder::new()
    .transport_config(config)
    .with_protocol(TcpServerConfig::new("0.0.0.0:9001"))
    .with_protocol(WebSocketServerConfig::new("0.0.0.0:9002"))
    .with_protocol(QuicServerConfig::new("0.0.0.0:9003"))
    .with_handler(Arc::new(MyHandler))         // Actor 模式（推荐）
    .actor_buffer_size(4096)                    // 可选，默认 2048
    .max_connections(10000)
    .backpressure_strategy(BackpressureStrategy::Block)
    .graceful_shutdown(Some(Duration::from_secs(30)))
    .build()
    .await?;

server.serve().await?;
```

### 客户端

```rust
use msgtrans::protocol::TcpClientConfig;

let mut client = TransportClientBuilder::new()
    .transport_config(config)
    .with_protocol(TcpClientConfig::new("127.0.0.1:9001"))
    .build()
    .await?;

client.connect().await?;

// 发送单向消息
client.send(b"hello").await?;

// 请求-响应
let result = client.request(b"ping").await?;

// 带选项发送
client.send_with_options(data, TransportOptions::new()
    .with_biz_type(1)
    .with_compression(CompressionType::Gzip)
).await?;
```

---

## TransportServer 双模架构

TransportServer 支持两种模式：

| 特性 | Actor 模式 | Legacy 模式 |
|------|-----------|-------------|
| 启用方式 | `with_handler(handler)` | 不设置 handler |
| 消息分发 | `SessionHandler` 回调 | `ServerEvent` broadcast |
| 发送路径 | `SessionHandle` → actor 信箱 → `Transport.send()` | 直接 `Transport.send()` |
| 并发模型 | 每连接独立 actor，无全局锁竞争 | 共享 broadcast channel |
| 适用场景 | 高吞吐生产环境 | 简单场景、向后兼容 |

### TransportServer 核心结构

```rust
pub struct TransportServer {
    config: TransportConfig,
    context: Arc<TransportContext>,
    transports: Arc<LockFreeHashMap<SessionId, Arc<Transport>>>,
    session_handles: Arc<LockFreeHashMap<SessionId, SessionHandle>>,
    session_id_generator: Arc<AtomicU64>,
    event_sender: broadcast::Sender<ServerEvent>,
    is_running: Arc<AtomicBool>,
    protocol_configs: HashMap<String, Box<dyn DynServerConfig>>,
    state_manager: ConnectionStateManager,
    request_tracker: Arc<RequestTracker>,
    session_handler: Option<Arc<dyn SessionHandler>>,
    actor_buffer_size: usize,
}
```

### 核心 API

```rust
impl TransportServer {
    pub async fn serve(&self) -> Result<(), TransportError>;
    pub async fn stop(&self);

    // 会话管理
    pub async fn add_session(&self, connection: Box<dyn Connection>) -> SessionId;
    pub async fn remove_session(&self, session_id: SessionId) -> Result<(), TransportError>;
    pub async fn close_session(&self, session_id: SessionId) -> Result<(), TransportError>;
    pub async fn close_all_sessions(&self) -> Result<(), TransportError>;
    pub async fn active_sessions(&self) -> Vec<SessionId>;
    pub async fn session_count(&self) -> usize;

    // 消息发送
    pub async fn send_to_session(&self, session_id: SessionId, packet: Packet) -> Result<(), TransportError>;
    pub async fn send(&self, session_id: SessionId, data: &[u8]) -> Result<TransportResult, TransportError>;
    pub async fn request(&self, session_id: SessionId, data: &[u8]) -> Result<TransportResult, TransportError>;
    pub async fn broadcast(&self, packet: Packet) -> Result<(), TransportError>;

    // 事件订阅（Legacy 模式）
    pub fn subscribe_events(&self) -> broadcast::Receiver<ServerEvent>;
}
```

---

## 性能特性

| 维度 | 现状 |
|------|------|
| Transport.send() 锁次数 | 1（Actor 模式下每连接串行，无竞争） |
| 服务端事件中转层数 | 1 |
| Transport 构造方式 | 同步（`with_context`） |
| 连接接口层数 | 1（Adapter 直接实现 Connection） |
| Wrapper 类型 | 0 |
| 全局单例 | 1（shared_memory_pool，与 TransportContext 统一） |
| TCP 读缓冲区 | 池化复用 |
| QUIC 写缓冲区 | 池化复用 |
| Actor 批量处理 | 每次最多 64 条消息 |
| Actor 信箱容量 | 默认 2048（可配置） |
