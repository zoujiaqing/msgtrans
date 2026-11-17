# MsgTrans 2.0.0 正式版候选验收

2026-11-18（Asia/Singapore）。候选 Rust 代码 `5453aae`，Kotlin `482d78a`，TypeScript `06981f7`。本轮只验证候选，未发布或打 tag。

## 阻断项

- 主动移除和关闭会话走独立取消信号，不受 actor 邮箱阻塞。pump 显式关闭连接并失败化 pending，即使应用仍持有 sender；supervisor 等任务和回调退出后才释放配额。
- 服务端 write-confirmed send/request 直接走绑定会话的 writer，回调无需等待自己的 actor 消费出站命令。回归覆盖同会话 send、反向 request 和应答。
- TCP/TLS 和 WebSocket 先 bind，再启动 accept pump。pump 持有并取消、等待所有在途握手；回归验证 `clean=true` 前半握手对端已关闭且端口可重绑。
- 使用 crates.io 真正的 `2.0.0-beta.3`，实测 beta client → GA server、GA client → beta server 均成功。兼容的是协商 `msgtrans/1` 的 2.0 beta，不承诺无 ALPN 的 1.x QUIC。两端协商 `msgtrans/2` 时，客户端单向写 9 字节前导，不等回复。TCP/WS 不使用此前导，Packet fixtures 字节未变。

## 验证

| 门禁 | 结果 |
| --- | --- |
| Rust | workspace 237 项通过；msgtrans 全特性 231 项通过（含 26 doctest）；Clippy 严格模式覆盖 9 组配置；rustdoc 通过，公开 API 快照无变化 |
| 真实 PrivChat | 从归档构建 Release Server/SDK；以 SPKI pin 验证证书并分别强制 QUIC 和 TLS/TCP。两轮各 44 实际通过、0 失败、5 选配跳过；Server clean shutdown。初次环境中，复制的数据库包含历史对象引用，缺对象文件导致附件 404；补回匹配的测试存储再跑通过。登录通知第一轮也有失败，复跑双绿，不以首轮证明其稳定 |
| Kotlin | macOS arm64 通过 Core/QUIC/WS 73 项（使用 Maven 依赖，不依赖同级源码）；与 Rust 候选的 QUIC 双向先收后发、双向 WS、Rust client 到 Kotlin WSS、错误 CA 拒绝全部通过。未执行 Android/iOS 真机门禁 |
| TypeScript | 178 单测、15 条真实 Rust WebSocket E2E；覆盖 subprotocol、最大合法帧和 zlib 三条链。16 个 fixture 文件逐字节一致；npm tarball 离线新目录安装和两处 export 导入通过。WebTransport 仍需真正的 HTTP/3 WebTransport 端点，不等同于原生 QUIC |
| 干净包装 | 从干净代码提交运行 `cargo package --locked -p msgtrans`；包内运行时代码与长稳归档逐文件一致，差异只有 Git 来源元数据和回归测试的无 TLS 编译门禁。独立消费者从最终归档复验 beta 双向互通、Rust 1.88 全特性编译通过。执行过 publish dry-run，未上传 |

## 服务端不重启长稳

Release、同一进程持续 **3,602.176 秒**，跨 TCP、WebSocket、QUIC，每轮 24 条连接，交替由服务端主动移除和客户端断开。**1,187 轮、28,488 条连接、113,952 次逐字节核对的请求均成功。** 每轮任务数归零、全部 24 个配额归还；最后 shutdown clean，并立即重绑 QUIC 端口。最慢一轮 2.045 秒，包含建连、请求和清理，不是产品级恢复 SLO。

同一个 PID 共 235 次采样，包含同进程发压客户端：RSS 峰值约 27.4 MiB，末次采样约 24.0 MiB；已采样 FD 为 12–26。此测试验证连接反复拆建，未重新注入 UDP 黑洞；带黑洞的真实业务恢复见前一份 11-17 报告。

最终归档 SHA-256：`4b819b3a54f7d7e940fd82fa892979ad7cef5c0228284b6c8a8cd1d735458404`。归档、校验和及日志保存于 `/tmp/msgtrans-ga-2/`。用 `scripts/ga/consume.sh <archive> <new-dir> beta` 和 `scripts/ga/consume.sh <archive> <new-dir> soak 3600` 可复跑 Rust 门禁。

**裁决：** 四个发布阻断已关闭，候选通过本轮门禁。未做性能优化、未发布。
