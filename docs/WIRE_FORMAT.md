# msgtrans Wire Format 规范

本规范定义 msgtrans 在网络上传输的字节级别数据包格式。
它是 **Rust 实现** (`msgtrans` crate) 与 **任何其他语言客户端** (`@msgtrans/client` / 未来的 Go / Swift / Kotlin 等) 之间的协议契约。

> **本规范是协议层的唯一真相 (single source of truth)。**
> 任何实现 — 包括 Rust 自身 — 都必须以本文档为准；当 Rust 实现与本规范不一致时，应修复 Rust 实现，而不是改文档。

- **协议版本**：`1`
- **字节序**：所有多字节整数字段一律 **大端序 (big-endian / network byte order)**
- **状态**：稳定 (stable)；向后兼容策略见 §14

---

## 1. 适用范围

本规范覆盖：

- 数据包 (Packet) 的二进制布局
- 各字段的语义、取值范围、解析规则
- 数据包到底层 transport (WebSocket / TCP / QUIC) 帧的映射
- 请求/响应的配对约定
- 跨语言实现需要注意的整数与编码问题

本规范**不覆盖**：

- 心跳的 biz_type 取值（应用层自行约定）
- TLS / 证书 / 鉴权流程
- 应用层 RPC（如 `account/user/find` 路由）的编码规则
- 业务字段、JSON schema

---

## 2. 数据类型与字节序

| 类型 | 长度 | 说明 |
|---|---|---|
| `u8` | 1 字节 | 无符号 8 位整数 |
| `u16` | 2 字节 | 大端序无符号 16 位整数 |
| `u32` | 4 字节 | 大端序无符号 32 位整数 |
| `bytes(N)` | N 字节 | 原始字节序列 |

**字节序约束**：除 `u8` 外，所有多字节整数字段使用 **大端序**。
任何实现必须明确指定字节序（例如 TS 中 `DataView.getUint32(offset, /*littleEndian=*/false)`）。

---

## 3. Packet 总体结构

一个完整的 Packet 由三部分顺序拼接而成：

```
+-------------------------+
|  Fixed Header (16 字节)  |
+-------------------------+
|  Ext Header (可变长度)   |  长度 = ext_header_len, 可为 0
+-------------------------+
|  Payload (可变长度)      |  长度 = payload_len, 可为 0
+-------------------------+
```

- 总长度 = `16 + ext_header_len + payload_len`
- 三段顺序固定，无对齐填充
- 序列化即为这三段的直接 concat，无任何分隔符

---

## 4. 固定头 (Fixed Header) 布局

固定头**总长度 16 字节**，字段顺序如下：

```
 偏移  长度  字段              类型     说明
 ----  ----  ----------------  -------  -------------------------------
 0     1     version           u8       协议版本，当前固定为 1
 1     1     compression       u8       压缩算法（见 §5.2）
 2     1     packet_type       u8       数据包类型（见 §5.3）
 3     1     biz_type          u8       业务类型，0-255，应用层定义
 4-7   4     message_id        u32 BE   消息 ID（见 §5.5）
 8-9   2     ext_header_len    u16 BE   扩展头长度（字节数）
 10-13 4     payload_len       u32 BE   负载长度（字节数）
 14-15 2     reserved          u16 BE   保留标志位（见 §5.8）
```

### 直观图示

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   version    | compression  | packet_type  |   biz_type    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          message_id (u32)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     ext_header_len (u16)      |       payload_len (u32) ...   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               +
|        payload_len cont.      |       reserved (u16)          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

---

## 5. 字段语义

### 5.1 `version` (u8)

- 当前固定为 `1`
- 接收方收到 `version != 1` **必须** 拒绝该数据包并视为协议错误
- 升级策略见 §14

### 5.2 `compression` (u8)

| 值 | 名称 | 算法 | 说明 |
|---|---|---|---|
| 0 | `None` | 无 | payload 原样传输 |
| 1 | `Zstd` | zstd | 仅 payload 被压缩；ext_header 不压缩 |
| 2 | `Zlib` | zlib (RFC 1950) | 仅 payload 被压缩；ext_header 不压缩 |
| 其他 | 保留 | — | 接收方 **应** 拒绝未识别的压缩值 |

> **注意**：值 `1` 是 Zstd，`2` 是 Zlib（与字母顺序相反）。这是历史决定，不要写反。

### 5.3 `packet_type` (u8)

| 值 | 名称 | 语义 |
|---|---|---|
| 0 | `OneWay` | 单向消息，**不要求**响应 |
| 1 | `Request` | 请求消息，**期望**对方返回 packet_type=2 的响应，message_id 与请求一致 |
| 2 | `Response` | 响应消息，message_id **必须** 等于对应请求的 message_id |
| 其他 | 保留 | 接收方 **应** 拒绝 |

应用层心跳推荐使用 `OneWay`，由约定的 `biz_type` 区分（见 §11）。

### 5.4 `biz_type` (u8)

- 取值范围 `0-255`，由应用层定义
- msgtrans 传输层**不解释** `biz_type`，仅原样透传
- 推荐应用层维护一份枚举表（例如 `0` = 普通消息，`1` = 心跳，`100-199` = RPC 请求路由等）

### 5.5 `message_id` (u32 BE)

ID 的产生分两种情况：

| 包类型 | 分配规则 |
|---|---|
| `OneWay` | 发送方**分配新 ID**（从本地计数器取下一值） |
| `Request` | 发送方**分配新 ID**（从本地计数器取下一值） |
| `Response` | **不分配新 ID**，**必须**直接复用对应 Request 的 message_id（见 §10） |

通用约束：

- 由**发送方**分配，本地计数器单调递增
- 推荐起始值为 `1`，达到 `u32::MAX` 后回绕到 `1`（不使用 `0`，便于"未设置"语义）
- `OneWay` 与 `Request` 共享同一个 ID 空间（同一计数器），不要分两个计数器
- 每个端各自管理**自己发出**消息的 ID 空间：client 发的与 server 发的 ID 互相独立，可能重叠也无所谓（接收方靠"是 Request 还是 Response + 自己是否在等这个 ID"区分）
- 重连（reconnect）时**应该**重置计数器

### 5.6 `ext_header_len` (u16 BE)

- 扩展头字节数
- 取值范围 `0 - 65535`
- 大于 0 时，紧跟在固定头后面的 N 字节即为扩展头内容（见 §6）

### 5.7 `payload_len` (u32 BE)

- 负载字节数
- 取值范围 `0 - 4_294_967_295` (理论上限)
- **如果 `compression != None`**，该值为**压缩后**的字节数
- 实际可用上限取决于 transport 适配器配置（见 §12）

### 5.8 `reserved` (u16 BE) — 标志位

低位三个 bit 已定义；其余保留位：发送方**应**置 0，接收方**应**忽略未识别位（不要因未知位拒绝包，便于向前兼容）。

| Bit | 掩码 | 名称 | 语义 |
|---|---|---|---|
| 0 | `0x0001` | `FRAGMENTED` | 该包是分片之一（v1 暂未使用，预留） |
| 1 | `0x0002` | `HIGH_PRIORITY` | 高优先级，调度提示 |
| 2 | `0x0004` | `HAS_ROUTE_TAG` | 扩展头中含路由标签（语义由应用层定义） |
| 3-15 | — | 保留 | 必须置 0；接收方忽略 |

> **v1 实现注意**：`FRAGMENTED` 在 WebSocket transport 下不应出现（WebSocket 自身有分片机制）。客户端实现 v1 应**始终**发送 `FRAGMENTED=0`，并对收到的 `FRAGMENTED=1` 报错或忽略。

---

## 6. 扩展头 (Ext Header)

- 长度由 `ext_header_len` 指定
- 当 `ext_header_len == 0` 时整段不存在
- **不参与压缩**：即使 `compression != None`，扩展头也是明文
- 内容语义由应用层定义；msgtrans 传输层不解释
- 典型用途：路由标签、追踪 ID、应用层 metadata

---

## 7. 负载 (Payload)

- 长度由 `payload_len` 指定（如压缩则为压缩后长度）
- 当 `payload_len == 0` 时整段不存在
- 内容语义由应用层定义（通常是 JSON / Protobuf / 自定义二进制）

---

## 8. 压缩处理顺序

发送方：

```
1. 应用层准备 payload (raw bytes)
2. if compression != None:
       payload = compress(payload, algorithm)
3. payload_len = len(payload)   // 压缩后长度
4. 序列化 fixed_header + ext_header + payload
```

接收方：

```
1. 解析 fixed_header
2. 读取 ext_header_len, payload_len 字节
3. if compression != None:
       payload = decompress(payload, algorithm)
4. 上交应用层
```

**关键约束**：
- ext_header **永远不压缩**
- 压缩仅作用于 payload
- `payload_len` 字段记录的是 wire 上的实际字节数（即压缩后长度）

---

## 9. Transport 映射

### 9.1 WebSocket

- **一个 Packet ⇒ 一个 WebSocket Binary 帧**
- Binary 帧的 payload 即 `to_bytes(packet)` 的完整字节序列（含固定头 + 扩展头 + payload）
- 接收方收到 Binary 帧后**整体**解析为一个 Packet，**不需要** length-prefix 拆包
- `Text` / `Close` / `Ping` / `Pong` 帧的处理：

  | WS 帧类型 | 行为 |
  |---|---|
  | `Binary` | **唯一**承载 msgtrans Packet 的方式 |
  | `Text` | **不属于** msgtrans 协议；canonical 实现**不应**主动发送；接收实现可选择忽略或降级处理 |
  | `Close` | 映射为 transport 层 `ConnectionClosed` 事件 |
  | `Ping` / `Pong` | WebSocket 协议层心跳，由 transport 静默处理；**不**暴露给应用层 |

> **浏览器实现注意**：浏览器 JS `WebSocket` API **不暴露** `Ping`/`Pong` 控制帧的发送能力，因此浏览器侧应用层心跳**必须**通过 `OneWay` Packet 实现（见 §11）。

### 9.2 TCP

- TCP 是字节流，没有帧边界，**必须**做 length-prefix 拆包
- msgtrans Rust 实现的 TCP 适配器自动从 fixed header 读取 `ext_header_len + payload_len` 计算总长度
- 浏览器无法使用 TCP transport，本规范不展开

### 9.3 QUIC

- 连接建立后，使用**单条长期复用的双向 stream** (`open_bi` / `accept_bi`) 承载该连接上**所有**的 Packet，**不**为每个 Packet 创建新 stream（避免 stream 创建开销）
- 在该 stream 内使用 **length-prefix 帧格式**：

  ```
  +----------------+----------------------------------+
  | frame_len (u32 BE) |  Packet bytes (frame_len 字节) |
  +----------------+----------------------------------+
  ```

  - `frame_len` 是后面 Packet 字节数（含固定头 + 扩展头 + payload）
  - 字节序：大端 (BE)
  - **单帧上限**：16 MB（`16 * 1024 * 1024`）；超过此值接收方**必须**报错并断流
- 浏览器**无法**使用裸 QUIC API
- **WebTransport** 是浏览器侧的等价能力（基于 HTTP/3 + QUIC），将作为**独立** transport adapter 在未来版本引入；它与本节描述的 QUIC adapter 在握手与帧封装上**不兼容**，是平行存在的两个 adapter

---

## 10. 请求/响应配对

- **Request** 由 client 或 server 任意一端发起
- 发送方为该 Request 分配一个 `message_id`，记录到本地待回响表中
- 对端处理后**必须**返回一个 `Response` Packet，且 `message_id` **完全等于**原 Request 的 message_id
- 发起方收到 Response 后，按 message_id 在待回响表中找到对应的 future / promise / callback 并完成
- 超时未收到 Response 时，发起方**应**清理待回响条目并向应用层报错

**实现要点**：

| 要点 | 说明 |
|---|---|
| ID 分配 | 单调递增 `u32`，避免与并发 OneWay 共用 ID |
| 待回响表 | 推荐 `Map<u32, Pending>` 结构 |
| 超时 | 推荐默认 10 秒，可配置 |
| 重连 | 重连时**必须**清理所有 pending 请求并以错误方式 reject，重置 ID 计数器 |

---

## 11. 心跳

msgtrans 传输层**不**定义心跳 Packet。心跳由应用层实现，约定如下：

- 使用 `packet_type = OneWay`
- 由应用层选定一个固定 `biz_type`（例如 `0xFE`），并在双方之间约定
- payload 可为空或携带时间戳
- WebSocket transport 层的 `Ping`/`Pong` **仅用于** TCP/网络保活，**不**等同于应用心跳

> **浏览器侧**：因为 JS `WebSocket` API 无法发送 `Ping`，应用层心跳是**唯一**可行方案。
> 库可选地提供 `heartbeatPacketFactory` 钩子让应用注入心跳 Packet。

---

## 12. 大小限制

| 字段 | 协议上限 | 推荐实现上限 |
|---|---|---|
| `ext_header_len` | 65535 字节 (`u16::MAX`) | ≤ 4 KB |
| `payload_len` | 4 GB (`u32::MAX`) | 由 transport 限制 |
| 单 Packet 总长度 | ~4 GB | 取决于 transport |

**transport 实现限制**（仅供参考，实际以配置为准）：

- WebSocket：默认 `max_message_size = 64 KB`，可配置
- TCP：理论无限，受内存约束
- 浏览器 `WebSocket` 无显式限制，但巨型消息会触发缓冲区压力

---

## 13. 跨语言整数处理

### 13.1 Wire 字段

| 字段 | 类型 | TS 表示 | 说明 |
|---|---|---|---|
| `version` | u8 | `number` | 0-255 |
| `compression` | u8 | `number` (枚举) | |
| `packet_type` | u8 | `number` (枚举) | |
| `biz_type` | u8 | `number` | 0-255 |
| `message_id` | u32 | `number` | u32 ≤ 2^32 < 2^53，JS number 安全 |
| `ext_header_len` | u16 | `number` | |
| `payload_len` | u32 | `number` | 同上 |
| `reserved` | u16 | `number` (位掩码) | |

### 13.2 应用层业务 ID

业务层若使用 64 位整数（`user_id` / `group_id` / `conversation_id` / 应用层 message_id），**必须**注意：

- JS `number` 安全整数上限是 `2^53 - 1`，**不能**直接表示 u64
- **推荐方案**：业务层 JSON 中用 **string** 表示 u64 ID
  ```json
  { "user_id": "10000000001234567890" }
  ```
- 备选方案：使用 TS `bigint`，但 JSON 序列化需自定义处理

> **本规范的范围内**所有整数（u8/u16/u32）都可安全用 JS `number` 表示。
> u64 仅在应用层出现，**不属于** wire format 规范。

---

## 14. 未来兼容性策略

按"是否需要协商"分三类。

### 14.1 向后兼容（无破坏改动，无需协商）

以下改动**不**升版本号、**不**需要双方提前协商：

- 在 `reserved` 中启用未使用的 bit（接收方按 §5.8 忽略未识别位，发送方可单方面启用）
- 增加新的 `biz_type` 取值（属应用层范畴，msgtrans 传输层不解释）

### 14.2 需能力协商的改动（不升 version，但需双方支持）

以下改动**不**升版本号，但**必须**通过应用层或带外的方式协商双方都支持后才能启用，否则会触发 §5.2 的"接收方拒绝未识别 compression 值"导致协议错误：

- 新增 `compression` 算法值（如增加 `Brotli=3` / `Lz4=4`）
- 新增 `packet_type` 取值（如增加流式响应 `Stream`）
- 在 `ext_header` 内引入新的应用层共识结构（属应用层范畴）

> **建议的协商机制**：连接建立后，双方通过一个固定 `biz_type` 的 `Request` 交换支持的 compression / packet_type 列表，取交集后才能使用新算法。

### 14.3 破坏性改动（必须升 version）

以下改动**必须**升 version 号：

- 修改任何固定头字段的偏移、长度或字节序
- **重新定义**已分配的 `packet_type` / `compression` 值（与"新增"区分）
- 修改 `ext_header` / `payload` 的拼接顺序
- 修改压缩处理顺序（如改为同时压缩 ext_header）
- 任何会导致 v1 实现 silently 误读的改动

升级时：
1. 新版本同时支持 `version=1` 和 `version=2`（过渡期）
2. 双方协商最高公共版本（可通过握手包，或在 `Connect` 类应用层消息中声明）
3. 旧实现遇到未来 version **应**报错并断连，不要 silently 误读

---

## 15. 一致性测试 fixture

为防止跨语言实现漂移，msgtrans 维护一组**二进制 fixture**，所有实现必须同时通过 `encode(fixture.input) == fixture.bytes` 与 `decode(fixture.bytes) == fixture.input`。

**计划中的 fixture 列表**（位于 `msgtrans/tests/fixtures/`，待生成）：

#### 第一批 — 基础 wire format（必做）

涵盖固定头、扩展头、reserved flags、request/response 配对。**无压缩**，可逐字节比对，跨实现完全确定。

| 文件 | 描述 |
|---|---|
| `01_oneway_empty.bin` | OneWay, message_id=1, biz_type=0, 空 ext_header, 空 payload |
| `02_oneway_text.bin` | OneWay, message_id=42, biz_type=5, payload="hello world" (utf-8) |
| `03_request_minimal.bin` | Request, message_id=100, biz_type=10, payload="ping" |
| `04_response_matching.bin` | Response, message_id=100 (匹配 03), biz_type=10, payload="pong" |
| `05_with_ext_header.bin` | OneWay, message_id=7, ext_header="route:abc", payload="data" |
| `06_max_biz_type.bin` | OneWay, message_id=0xDEADBEEF, biz_type=255, payload=空 |
| `07_high_priority_flag.bin` | OneWay, reserved 设置 HIGH_PRIORITY (0x0002) |
| `08_route_tag_flag.bin` | OneWay, reserved 设置 HAS_ROUTE_TAG (0x0004) |

#### 第二批 — 压缩（基础 fixture 稳定后再做）

压缩 fixture 涉及 zlib/zstd 库版本差异（同样输入不同库版本可能产出不同字节流），不适合做强字节比对。建议第一批稳定后单独引入，并以"解压后字节相等"作为校验标准（而非"压缩后字节序列完全相等"）。

| 文件 | 描述 |
|---|---|
| `09_compressed_zlib.bin` | OneWay, compression=2 (Zlib), payload 为 1KB 重复字符的压缩结果 |
| `10_compressed_zstd.bin` | OneWay, compression=1 (Zstd), payload 同上的 zstd 压缩 |

每个 `.bin` 文件配套一个 `.json` 描述输入：

```json
{
  "version": 1,
  "compression": 0,
  "packet_type": 0,
  "biz_type": 5,
  "message_id": 42,
  "ext_header_hex": "",
  "payload_hex": "68656c6c6f20776f726c64",
  "reserved": 0
}
```

---

## 附录 A：Rust ↔ TypeScript 类型对照

| Rust | TypeScript | 备注 |
|---|---|---|
| `Packet` | `Packet` (interface 或 class) | |
| `FixedHeader` | `FixedHeader` (interface) | |
| `PacketType::OneWay = 0` | `PacketType.OneWay = 0` | const enum |
| `PacketType::Request = 1` | `PacketType.Request = 1` | |
| `PacketType::Response = 2` | `PacketType.Response = 2` | |
| `CompressionType::None = 0` | `CompressionType.None = 0` | |
| `CompressionType::Zstd = 1` | `CompressionType.Zstd = 1` | |
| `CompressionType::Zlib = 2` | `CompressionType.Zlib = 2` | |
| `ReservedFlags` | `ReservedFlags` (number 位掩码) | bit 操作 `& \|` |
| `Vec<u8>` | `Uint8Array` | |
| `Bytes` | `Uint8Array` (浏览器无零拷贝等价) | |
| `u32` | `number` | |
| `u64` (业务层) | `string` 或 `bigint` | 见 §13.2 |

---

## 附录 B：编/解码参考伪代码

### 编码 (encode)

```
fn encode(packet) -> bytes:
    payload = packet.payload
    if packet.compression != None:
        payload = compress(payload, packet.compression)

    buf = new ByteBuffer(16 + len(packet.ext_header) + len(payload))
    buf.put_u8(packet.version)             // = 1
    buf.put_u8(packet.compression)
    buf.put_u8(packet.packet_type)
    buf.put_u8(packet.biz_type)
    buf.put_u32_be(packet.message_id)
    buf.put_u16_be(len(packet.ext_header))
    buf.put_u32_be(len(payload))
    buf.put_u16_be(packet.reserved)
    buf.put_bytes(packet.ext_header)
    buf.put_bytes(payload)
    return buf.freeze()
```

### 解码 (decode)

```
fn decode(bytes) -> Packet:
    if len(bytes) < 16: error("header too short")
    version = bytes[0]
    if version != 1: error("unsupported version")
    compression = bytes[1]
    packet_type = bytes[2]
    biz_type = bytes[3]
    message_id = read_u32_be(bytes, 4)
    ext_header_len = read_u16_be(bytes, 8)
    payload_len = read_u32_be(bytes, 10)
    reserved = read_u16_be(bytes, 14)

    expected = 16 + ext_header_len + payload_len
    if len(bytes) < expected: error("packet incomplete")

    ext_header = bytes[16 .. 16 + ext_header_len]
    payload = bytes[16 + ext_header_len .. expected]

    if compression != None:
        payload = decompress(payload, compression)

    return Packet { version, compression, packet_type, biz_type,
                    message_id, ext_header, payload, reserved }
```

---

## 文档元信息

- **当前协议版本**：1
- **本文档版本**：1.0
- **最后更新**：2026-05-01
- **维护**：msgtrans 核心团队
- **变更**：任何对本文档的修改必须同步更新所有实现并扩展 §15 的 fixture 集
