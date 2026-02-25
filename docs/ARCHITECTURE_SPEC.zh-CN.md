# msgtrans 架构规范

msgtrans 是一个 **Rust 异步多协议传输库**，为应用层提供统一的 TCP / WebSocket / QUIC 传输接口。

## 设计原则

| 原则 | 落地方式 |
|------|---------|
| 零拷贝 | `Bytes` / `BytesMut` + `SharedPacket` / `ArcPacket` |
| 无锁优先 | `DashMap`、`flume`、原子类型封装的 `LockFreeHashMap` / `LockFreeQueue` / `LockFreeCounter` |
| 单锁热路径 | `Transport.send()` 仅一次 `Mutex::lock` |
| 单一连接 trait | 适配器直接实现 `Connection` trait，无中间 wrapper |
| Actor 模型 | 每连接一个 `SessionActor`，`SessionHandler` trait 提供业务回调 |
| 协议无关 | `Connection` / `Server` trait 抽象，TCP / WS / QUIC 各自实现 |
| 依赖注入 | `TransportContext` 持有共享资源，构造时显式传入 |
| 缓冲区池化 | 适配器读/写缓冲区从 `OptimizedMemoryPool` 分配，连接关闭时回收 |

---

## 依赖矩阵

| 依赖 | 用途 |
|------|------|
| `tokio` 1.x | 异步运行时（rt-multi-thread, io, time, sync, macros） |
| `flume` 0.12 | 无锁 bounded/unbounded 通道 → `LockFreeQueue` |
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
│  TcpAdapter: Connection                               │
│  WebSocketAdapter: Connection                         │
│  QuicAdapter: Connection                              │
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
│   ├── adapter.rs              # ServerConfig / ClientConfig trait
│   ├── client_config.rs        # TcpClientConfig / WebSocketClientConfig / QuicClientConfig
│   └── server_config.rs        # TcpServerConfig / WebSocketServerConfig / QuicServerConfig
└── transport/
    ├── context.rs              # TransportContext（DI，与 shared pool 统一）
    ├── transport.rs            # Transport（同步构造，with_context）
    ├── transport_server.rs     # TransportServer
    ├── server.rs               # TransportServerBuilder
    ├── client.rs               # TransportClientBuilder / TransportClient
    ├── session_actor.rs        # SessionActor + SessionHandler
    ├── lockfree.rs             # LockFreeHashMap / LockFreeQueue / LockFreeCounter
    ├── lockfree_connection.rs  # LockFreeConnection
    ├── memory_pool.rs          # OptimizedMemoryPool + shared_memory_pool()
    ├── pool.rs                 # ConnectionPool（独立可选组件）
    ├── config.rs               # TransportConfig / TransportOptions
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

### 适配器实现

每个协议适配器用单个泛型 impl 覆盖客户端和服务端配置：

```rust
impl<C: Send + Sync + 'static> Connection for TcpAdapter<C> { ... }
impl<C: Send + Sync + 'static> Connection for WebSocketAdapter<C> { ... }
impl<C: Send + Sync + 'static> Connection for QuicAdapter<C> { ... }
```

工厂直接返回适配器实例：

```rust
async fn create_connection(...) -> Result<Box<dyn Connection>, TransportError> {
    let adapter = TcpAdapter::connect(addr, config).await?;
    Ok(Box::new(adapter))
}
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

```
Transport.send(packet)
    │
    ├── self.connection.lock().await     ← 唯一一次锁
    │       ↓
    └── conn.send(packet).await          ← 直达底层协议
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
               │ request_tracker│
               │    .complete() │
               └───────┬────────┘
                       │ (非 Response 或未匹配)
                       ▼
                  SessionActor
                       │
                  mpsc::bounded
                       │
                       ▼
              SessionHandler 回调
              ├── on_connect()
              ├── on_message(session_id, packet)
              ├── on_disconnect()
              └── on_error()
```

### SessionHandler trait

```rust
#[async_trait]
pub trait SessionHandler: Send + Sync + 'static {
    async fn on_connect(&self, session_id: SessionId);
    async fn on_message(&self, session_id: SessionId, packet: Packet);
    async fn on_disconnect(&self, session_id: SessionId);
    async fn on_error(&self, session_id: SessionId, error: TransportError);
}
```

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

### 服务端事件流

```
Connection event_loop ──broadcast──► TransportServer event task
                                          │
                                    ┌─────┴──────┐
                                    │ Actor mode  │  Legacy mode
                                    ▼             ▼
                              SessionActor   handle_transport_event()
                                    │             │
                                    ▼             ▼
                              SessionHandler  ServerEvent broadcast
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
| `tokio::sync::mpsc` | 适配器 send_queue, shutdown | 与 `tokio::select!` 原生集成 |
| `flume` | `LockFreeQueue`, `SessionActor`, `LockFreeConnection` | 无锁 MPMC，sync/async 双模 |
| `tokio::sync::broadcast` | 事件广播（TransportEvent, ServerEvent, ClientEvent） | 1-to-N pub/sub |

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

```rust
let server = TransportServerBuilder::new()
    .with_config(config)
    .with_tcp("0.0.0.0:9001")
    .with_websocket("0.0.0.0:9002")
    .with_quic("0.0.0.0:9003")
    .with_handler(MyHandler)
    .with_backpressure(BackpressureStrategy::Block, 1024)
    .with_graceful_shutdown(Duration::from_secs(30))
    .build()
    .await?;

server.start().await?;
```

### 客户端

```rust
let client = TransportClientBuilder::new()
    .with_config(config)
    .connect_tcp("127.0.0.1:9001")
    .await?;

client.send(packet).await?;
let response = client.request(request_packet).await?;
```

---

## 性能特性

| 维度 | 现状 |
|------|------|
| Transport.send() 锁次数 | 1 |
| 服务端事件中转层数 | 1 |
| Transport 构造方式 | 同步（`with_context`） |
| 连接接口层数 | 1（Adapter 直接实现 Connection） |
| Wrapper 类型 | 0 |
| 全局单例 | 1（shared_memory_pool，与 TransportContext 统一） |
| TCP 读缓冲区 | 池化复用 |
| QUIC 写缓冲区 | 池化复用 |
