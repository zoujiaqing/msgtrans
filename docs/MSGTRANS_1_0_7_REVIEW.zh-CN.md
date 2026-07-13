# msgtrans 1.0.7 进展评审与架构收敛路线图

> 状态：本文面向开发中的 `1.0.7`，不是 `1.0.8`。
> 目标：合并多轮评审中讨论过的稳定性、性能、标准化、灵活性问题，并把它们收敛成可执行优先级。

## 1. 版本纠偏

当前开发版本是 `1.0.7`。

之前外部评审里把止血项放到 `1.0.8`，这个版本切法不适合当前仓库状态。本文统一改为：

- `1.0.7-P0`：必须优先修复的 bugfix / 行为修正，不改公开 API 签名。
- `1.0.7-P1`：内部架构收敛 + 纯新增 API，保持向后兼容。
- `1.0.7-P2`：性能、测试、标准化增强，同样保持向后兼容。
- `post-1.0.7`：**且仅**需要破坏或删除现有公共 API 的方向。

### 1.1 版本号判定标准（唯一标准）

归属由「是否破坏现有公共 API」决定，**与内部改动量无关**：

- 纯内部重构（哪怕重写整个 adapter 层、抽出 Framer 内核、替换分发主干），只要现有 `pub` 签名和行为契约不变 → 留在 `1.0.x`。
- 新增 `pub` 方法 / 类型 / 配置项（旧路径保留可用）→ 留在 `1.0.x`。
- 删除、重命名、改签名、改 `pub` 字段类型、收窄可见性、移除 re-export → 才进 `post-1.0.7`（未来 `2.0`）。

> 注：严格 cargo semver 下纯新增公共 API 属 minor，但开发期统一在 `1.0.7` 迭代、发布切版时再区分 patch/minor 即可。唯一红线是不触碰 major（破坏 / 删除）——内部改动量再大也不构成跳大版本的理由。

## 2. 1.0.7 已完成的关键进展

`1.0.7` 已经不是单纯的小修版本，它已经包含一批重要的稳定性和性能方向改进：

- 请求生命周期从 `Drop` 推断升级为 `RequestRegistry` 显式状态机。
- `RequestRegistry` 使用 `RequestKey { session_id, request_id }`，具备会话隔离能力。
- 请求超时检测从单请求延迟任务转为分桶 tick 扫描，避免全量扫描热点。
- `TransportContext::respond()` 增加幂等状态检查，重复响应降级为低噪音 debug。
- 会话关闭时可以批量终止 pending request，避免把断线误判为业务未响应。
- `stop()` 已开始被监听循环读取，server shutdown 不再只是写标志位。
- QUIC server shutdown 主动 close endpoint 并等待 idle，改善 UDP 端口释放。
- TCP 首包协议头非法时会直接断开，避免 telnet/垃圾流量长期占连接。
- `flume` 已进入 actor/lockfree 热路径，actor 模式具备 per-session bounded mailbox。
- `load_test` 增加 `send` / `request` 模式，load server 支持 request echo。
- `docs/WIRE_FORMAT.md` 已成为 wire format 的更准确信息源。

## 3. 总体判断

`1.0.7` 的方向是对的，但还没有达到“通用严密网络库”的最终形态。

更准确的关键词不是“继续堆优化”，而是“收敛”：

- 收敛请求生命周期：client、server、legacy event、actor handler 应该共享同一套 request registry 和指标。
- 收敛消息分发：数据消息不能长期依赖 `broadcast` 作为主干道。
- 收敛协议行为：TCP、WebSocket、QUIC 对非法帧、超限帧、迟到响应、断线 pending 的行为应一致。
- 收敛公开 API：没有接入主链路的 lockfree/pool/plugin 等组件不应继续作为稳定 API 暴露。
- 收敛文档规范：wire format 只能有一个 truth source。

## 4. 结构性根因

### 4.1 legacy broadcast 和 actor 双世界并存

当前库同时存在两套分发模型：

- legacy：`subscribe_events()` + `ServerEvent` / `ClientEvent` broadcast。
- actor：`SessionHandler` + `SessionActor` + per-session flume mailbox。

问题是两套模型没有共享完整的请求生命周期语义。`RequestRegistry` 主要覆盖 legacy server request 路径，actor 路径仍由 `SessionSender::respond()` 自己发送响应。

这会带来：

- 同一个库的两个推荐入口稳定性不同。
- actor 模式没有统一 request timeout / duplicate / session closed 指标。
- legacy 模式仍承受 broadcast fan-out、clone 和 lag 风险。

### 4.2 broadcast 被用于数据主干道

`tokio::sync::broadcast` 适合“可丢通知”，不适合作为可靠消息主干道。

当消费者落后时，broadcast 会返回 `Lagged(n)`，旧消息已经被覆盖。当前 `stream.rs` 里存在继续循环吞掉 lag 的路径，这意味着慢消费者可能导致消息静默丢失。

对 `msgtrans` 这种 message transport 库来说，数据消息静默丢失是高优先级稳定性问题。

### 4.3 零拷贝、lockfree、pool 尚未完全成为内核

代码中已经有 `SharedPacket`、`LockFreeConnection`、`ConnectionPool`、`OptimizedMemoryPool` 等设施，但主链路仍然大量使用 `Packet { payload: Vec<u8> }` 和 clone/to_vec。

这说明现在的性能结构更像“具备优化组件”，还不是“全链路优化内核”。

注意：这个根因的收益可以按 API 兼容性拆开——adapter 内部的分配优化（消灭 `vec![0u8; len]` 零填充、多余 `to_vec()`、`freeze()` 后拷弃）不触碰任何 `pub` 字段，属 `1.0.7`；只有 `Packet.payload` 字段类型从 `Vec<u8>` 改 `Bytes` 才破坏 API，属 `post-1.0.7`。详见 P2-1。

## 5. 1.0.7-P0：必须优先修复

P0 目标：不追求炫技，先消除安全、静默丢消息、跨会话错配和断线挂死。

### P0-1 QUIC 客户端证书验证默认值

当前 `QuicClientConfig::default()` 默认 `verify_certificate = false`，这对开发友好，但不符合安全默认原则。

建议：

- 默认改为验证证书。
- 保留显式 `danger_skip_verification` 或等价 API。
- 当用户主动跳过校验时输出 warn。

### P0-2 服务端主动 request 的 response 匹配必须加入 session 维度

当前部分 request tracker 只按 `message_id` 完成响应，缺少 `session_id` 校验。

风险：

- message_id 可预测。
- 不同 session 的 response 可能错配。
- 恶意连接可以伪造其他 session 的 response。

建议：

- request key 统一为 `(Option<SessionId>, message_id)`。
- server initiated request 完成时必须校验响应来源 session。
- 最终收敛到 `RequestRegistry`。

### P0-3 解压上限一致化

当前压缩解码策略不够一致，zlib 路径存在无上限 `read_to_end` 风险，zstd 路径又有固定 1MB 上限。

建议：

- 引入统一 `max_decompressed_size`。
- zlib 使用 `take(limit)` 或等价限流 reader。
- zstd / zlib 使用同一配置来源。
- 超限返回明确 protocol error。

### P0-4 断线立即失败 pending request

断线后 pending request 不应该继续等待固定超时。

建议：

- `ConnectionClosed` / `close_session` 路径调用 `request_tracker.fail_all(...)` 或 registry 等价接口。
- 所有等待中的 request 立即返回 connection closed。
- 避免 IM 场景里断网后请求统一卡 10 秒。

### P0-5 broadcast lag 必须可观测

在彻底替换 broadcast 数据主干前，至少必须让消息丢失可见。

建议：

- 所有 `Lagged(n)` 分支记录 warn 或 debug+counter。
- 增加 dropped/lagged event counters。
- 后续 P1/P2 再把数据消息迁出 broadcast。

### P0-6 错误类型修正

当前 `From<String> for TransportError` 会丢弃字符串内容，`From<std::io::Error>` 也会丢失 source 链和细粒度分类。

建议：

- `From<String>` 至少保留原始 message。
- IO timeout 映射到 `TransportError::Timeout`。
- 删除靠 `to_string().contains("timeout")` 判断超时的路径。

## 6. 1.0.7-P1：架构收敛

P1 目标：让 1.0.7 内部形成一套稳定、可观测、可维护的主链路。

### P1-1 RequestRegistry 成为唯一请求生命周期内核

最终目标是取消多套 request tracker：

- legacy server request 使用 `RequestRegistry`。
- actor request/respond 使用 `RequestRegistry`。
- client request/response 使用同一语义的 registry 或薄封装。
- server initiated request 也使用带 session key 的 registry。

### P1-2 respond 语义从 fire-and-forget 收紧

当前 `TransportContext::respond(self, Vec<u8>) -> ()` 无法把实际发送失败反馈给调用方，因为内部 responder 是同步 `Fn`，实际发送靠 `tokio::spawn`。

在 1.0.7 内可以两步都做，且都不破坏 API：

- 内部状态补强：`Pending -> Responded` 扩展为区分 `Responding / ResponseSent / ResponseSendFailed`，发送失败计入 `response_send_failed_total` 并带 session/request 字段；late/duplicate 统一低噪音。
- **新增**（加法，非破坏）`respond_checked(self, Vec<u8>).await -> Result<(), TransportError>`，让需要确认送达的调用方拿到结果；旧 `respond()` 保留可用。

结论修正：让调用方「能感知响应发送失败」这个能力**不必等到 breaking**——新增一个 checked 方法即可在 1.0.7 交付。真正需要 `2.0` 的只有「删掉旧的 fire-and-forget `respond()`」这一步，而它未必值得做。

### P1-3 actor 模式接入 request timeout / session closed

actor 路径现在吞吐设计不错，但生命周期治理不完整。

建议：

- `SessionActor` 收到 request 时注册 registry。
- `SessionSender::respond()` 标记 registry 状态。
- actor close/disconnect 时批量 session closed。
- actor mailbox full / closed 计入指标。

### P1-4 EventStream session_filter 要么修正，要么废弃

当前 `TransportEvent::session_id()` 对核心事件返回 `None`，导致 session filter 对数据事件不可用。

两个方向：

- 在事件本体中携带 session_id。
- 明确废弃全局 transport event session filter，只保留 server/client 层事件过滤。

不要继续保留“看起来有功能、实际过滤不到核心事件”的 API。

### P1-5 三协议非法帧策略统一

当前 TCP 对非法首包严格断开；WebSocket / QUIC 更倾向降级为 one-way raw payload。

建议：

- 提供 `strict_protocol` 策略。
- 默认 strict：非法 msgtrans packet 视为协议错误并断开。
- lenient 只作为兼容选项。
- TCP / WebSocket / QUIC 使用一致的 frame size / packet size 限制。

## 7. 1.0.7-P2：性能和工程化

P2 目标：在稳定语义收敛后，把性能优化落到真实热路径。

### P2-1 零拷贝真正入链（可拆分，主体进 1.0.7）

零拷贝收益不是一个整体，按是否触碰 `pub` 字段拆开：

**属 1.0.7（内部路径优化，不碰 `Packet` 公共字段）：**

- QUIC 读包消除每帧 `vec![0u8; len]` 零填充（`quic.rs:515`），改用未初始化缓冲或从池借。
- WebSocket 发送消除 `Bytes -> to_vec()`（`websocket.rs:281`）。
- TCP 收包消除 `split_to().freeze()` 之后又 `to_vec()` 的拷弃（`tcp.rs:276` / `packet.rs:456`）。
- 让主链路优先走已存在但未接线的 `SharedPacket`（Bytes 版）路径。

**属 post-1.0.7（破坏 API）：**

- `Packet.payload: Vec<u8> -> Bytes` 是 `pub` 字段类型变更，破坏兼容，留到 `2.0`。在此之前用 `SharedPacket` 承载热路径、`Packet` 保持兼容即可拿到大部分收益。

### P2-2 QUIC 低延迟模式

当前 QUIC 单长 bidi stream 适合吞吐，但可能引入 stream 内队头阻塞。

建议：

- 保留单流吞吐模式。
- 增加多流低延迟模式。
- 后续支持优先级流或业务类型路由。

### P2-3 benchmark 基线

load example 已经能做基本回归，但还不是严肃 benchmark。

建议：

- 固定 release 模式、日志级别、连接预热和统计窗口。
- 输出 p50 / p95 / p99 latency。
- 区分 connect time 和 steady-state throughput。
- 加入 request/send、不同 payload、不同协议矩阵。

### P2-4 测试、fuzz、CI

网络库必须用测试证明稳定性，而不是只靠压测观察。

建议：

- TCP 半包/粘包/坏头/超限帧测试。
- WebSocket 非法 binary/text/close 行为测试。
- QUIC shutdown/task leak 测试。
- RequestRegistry race/timeout/session closed 测试。
- Wire format fixture 跨语言持续校验。
- GitHub Actions 或等价 CI 门禁。

### P2-5 共享 Framer / EventLoopAdapter 内核（纯内部重构，属 1.0.7）

TCP / WebSocket / QUIC 现在各自维护 framing、读写循环、select、shutdown、背压、事件发射，重复度极高（三份几乎逐字一致的 `send()` / `close()` / `event_stream()`，三份同构 error 枚举，三份 builder）。

抽象方案：

- `Framer` trait（`Decoder` / `Encoder`）：各协议只保留真正差异——TCP 坏头 resync（`tcp.rs:188-223`）、QUIC 单流长度前缀（`quic.rs:429-467`）、WebSocket 消息分类（`websocket.rs:340-391`）。
- 泛型 `EventLoopAdapter<S: AsyncRead + AsyncWrite, F: Framer>`：收敛读写循环、shutdown、背压、事件发射为单份。
- 顺带结构性修掉 QUIC read/write task 无 owner 的收尾问题——`select!{ read_exact, shutdown }` + `JoinSet`，而不是只补 `abort()`。

关键点：这一步无论改动多大，**只要 `TcpClientConfig` / `QuicServerConfig` 等公共构造入口签名不变，就完全不破坏 API**，因此属 `1.0.7` 而非 `post-1.0.7`。它是 1.0.7 内收益最大的一次收敛，也是 P1-5（三协议一致策略）和 P2-1（零拷贝）落地的地基。

## 8. post-1.0.7：仅限真正破坏公共 API 的方向

按 §1.1 判定标准，只有**删除 / 改签名 / 改 `pub` 字段类型 / 收窄可见性 / 移除 re-export** 才落到这里。注意：下面每一项的「能力」大多能在 `1.0.7` 内通过**新增 API** 先拿到，真正推迟到 `2.0` 的只是「删掉被替换下来的旧 API」。

### 8.1 移除 legacy broadcast 数据路径（能力已可在 1.0.7 内交付）

- **属 1.0.7**：把数据消息（`MessageReceived` / request）内部改走 actor 主干，broadcast 只留连接生命周期 / 统计 / 可丢诊断。只要 `subscribe_events()` 签名与 `ServerEvent` 变体不删，这是行为收敛，不破坏编译兼容（需 privchat-server 协同验证，见 §4.1）。
- **属 post-1.0.7**：真正删除 `subscribe_events()` 的数据语义、或移除 `ServerEvent::MessageReceived` 变体，才是 breaking。

### 8.2 公共 API 表面裁剪

真正的 major，必须留到 `2.0`；但 `1.0.7` 内可先做**不破坏的前置**——给这些类型加 `#[deprecated]` + `#[doc(hidden)]`，停止在文档 / 示例中引用，为 `2.0` 的删除铺路：

- 未接入主路径的死类型：`RequestManager`、`ConnectionPool`、`LockFreeConnection` + `ConnectionFactory`、`plugin::*`、`expert_config::*`。→ 1.0.7 标 deprecated，2.0 删或收 `pub(crate)`。
- `pub use tokio;`（`lib.rs:120`）把 tokio semver 钉进 msgtrans 公共契约，tokio 一次 bump 就是 msgtrans 的 breaking。→ 2.0 移除。
- 协议专属事件 `TcpEvent` / `QuicEvent` / `WebSocketEvent` 挂在 crate 根，把内部 wire 事件变成公共契约。→ 2.0 收敛。
- feature flags 目前是装饰性的（全 src 无 `cfg(feature=...)`），关掉也不裁剪 quinn/tungstenite。→ 让 feature 真正 `cfg` 掉代码与依赖（加 `cfg` 是新增裁剪能力可提前；改变默认构建产物需谨慎评估）。
- 客户端 5 个吞掉不生效的 builder 配置项（`connect_timeout` / `connection_pool` / `load_balancer` / `circuit_breaker` / `enable_connection_monitoring`，`client.rs:167-200`）与每协议不生效的 `RetryConfig`。→ 要么 1.0.7 内补实现让其生效（加法），要么 2.0 删。

## 9. 当前推荐执行顺序

1. `1.0.7-P0`（bugfix / 行为修正，不改签名）：安全默认、session 维度 response 匹配、解压上限、断线 fail pending、broadcast lag 可观测、错误类型。
2. `1.0.7-P1`（内部收敛 + 新增 API）：RequestRegistry 统一覆盖 actor/legacy/client/server、respond 新增 checked 变体、session filter 修正、三协议严格模式（新增策略开关）。
3. `1.0.7-P2`（内部重构 + 新增 API）：共享 Framer/EventLoopAdapter 内核（含 QUIC task 收尾根治）、零拷贝内部路径优化、QUIC 低延迟模式、benchmark、fuzz/CI；同步给待删类型标 `#[deprecated]`。
4. `post-1.0.7`（`2.0`）：仅执行「删除旧 API」这一类动作——移除 legacy broadcast 数据路径、删死类型、去 `pub use tokio`、`Packet.payload` 换 `Bytes`。

一句话判断：

`1.0.7` 的下一步不是增加组件，而是把已有组件收敛成一条可证明稳定、可观测、无静默丢消息的主链路。**几乎所有收敛都不破坏 API——真正需要等 `2.0` 的只有「删掉被替换下来的旧 API」这一个动作。**

## 10. 实施进度（1.0.7 收口）

收口标准 1/2/3 已完成并 push 到 `origin/main`：

- **step 2（标准 1）**：`RequestRegistry` 成为请求生命周期唯一核心；`RequestTracker` 降级为其薄委托（保留 pub API + `next_id`）；client/server/actor/legacy 语义统一；actor inbound request 注册 registry + `SessionSender::respond()` 幂等标记。
- **step 3（标准 2）**：`TransportContext::respond_checked()` 加法，server + client 两侧都真实感知发送失败；旧 `respond()` 保留（内部 spawn future + debug 记失败）。
- **step 4（标准 3）**：`FramePolicy`（Strict/Lenient）+ WebSocket/QUIC adapter 接入 + `Transport{Server,Client}Builder::with_frame_policy()` 用户入口，经 `Connection::set_frame_policy`（trait default 方法）注入。

**API 红线全程守住**：`RequestTracker`/`RequestKey`/`create_session_actor`/`SessionActor::new`/`new_request`/`TransportServer::new*` 等公共签名一律未改，通过 `pub(crate) with_*` 注入、内部包装老 responder、trait default 方法达成。

**验收**：`cargo test` 48+6+3；`--features flate2,zstd` 50+6+3；`build --examples` 通过；TCP request load test（20 连接 / 5 秒）13.6k msg/s、0 error、请求-响应链路完整——无性能回退。

**收口边界**：step 1-4 构成 1.0.7 的稳定性架构大版本。step 5/6（P2-5 Framer 内核，重写三 adapter）与 step 7（P2-2 QUIC 多流）经评估属高风险 adapter 重写，挪到 `1.0.8` / `1.1.0` 单独、有 review 地做，不塞进 1.0.7 拖累稳定版。破坏性清理（删 `RequestTracker`、改 `RequestKey`、`Packet.payload -> Bytes`、删 legacy/`pub use tokio`）留 `2.0`。
