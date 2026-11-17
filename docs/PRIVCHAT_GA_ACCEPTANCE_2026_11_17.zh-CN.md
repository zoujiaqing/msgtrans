# MsgTrans 2.0.0 正式版验收：PrivChat 真实链路

日期：2026-11-17（Asia/Singapore）。结论：**核心 QUIC 故障切换用例通过，不签发正式版**。本轮只做验证和记录，未修改产品代码、版本号、tag 或发布包。

## 环境与口径

- 源码：MsgTrans `5b73d32`、privchat-server `6b8dde8`、privchat-sdk `ec2104b`、本地 privchat-protocol `213b99c`。服务端按自身 lockfile 构建；SDK 走本地源码。测试中服务端/SDK 分别使用 Quinn 0.11.11 / 0.11.9。
- macOS 本机 Debug 构建。独立 PostgreSQL 16、Redis、临时 JWT 与 TLS 证书，SDK 使用其真实 SPKI pin。没有关闭证书验证，没有对开发或生产库执行迁移。
- 使用 SDK `examples/accounts` 的临时副本，注入证书 pin、10 秒预算、独立端口和测试用传输选择。纯 TCP 对照去掉 QUIC/WS 备用；QUIC-only 长测没有备用。持续测试通过注册、认证、加好友、私聊及真实 `sync/submit` 发消息，以接收方 SDK 本地库的 canonical server message ID 和消息内容作为成功条件。
- UDP 中继用独立环回端口，固定随机种子，单向延迟 35–55 ms、独立随机丢包 3%；黑洞期间双向丢弃 UDP，TLS/TCP 备用不受影响。未修改系统防火墙。不是移动网络实测或吞吐基准。

## 结果

| 场景 | 结果 |
| --- | --- |
| 直连 QUIC 与纯 TLS/TCP 各跑 49 阶段 | 每轮 **24 实过、20 失败、5 跳过**，失败集合逐项相同。账号 harness 会把跳过显示成 PASS，本表已剔除。 |
| 冷启动：UDP 黑洞 → TLS/TCP | 3 个 SDK 均在 QUIC 超时后拨通 TLS/TCP；认证、好友、私聊、离线差量 **4/4**。 |
| 冷启动：UDP 黑洞 + TCP 拒绝 → WebSocket | 3 个 SDK 均拨通 WebSocket；认证、好友、私聊 **3/3**。 |
| 真实 QUIC 证书 pin 错误 | `TransportRejected`，未拨 TCP 或 WebSocket，符合 fail-closed。 |
| 300 秒持续业务：弱网 → UDP 黑洞 → 恢复 UDP | **259 条确认落库、2 次操作失败**；弱网 27/27，黑洞窗口 54 成功/2 失败，恢复后 178/178。3 个 SDK 在第一次自动重连均回落 TLS/TCP 并重新认证。 |
| 180 秒持续弱网 QUIC-only | **140 条确认落库、0 失败**；送达耗时 p50 243 ms / p95 505 ms / max 969 ms，含业务 RPC 和本地库轮询，不是网络 RTT。 |
| 服务端库测试（独立测试数据库） | **665 通过、0 失败、12 ignored**。未使用跳过数据库的环境开关。 |
| 服务端 `cargo check --locked --all-targets` | 通过。 |
| 文本 Outbox 定向隔离重跑 | **4/4**；文本发送 3/3、队列残留 0；模拟应用重启后离线排队 2/2 发出，重放的 server ID 保持稳定。全量套件的 Outbox 红项仍保留。 |
| 服务端真实进程关闭 | 所有 SDK 退出后在线会话归零；SIGINT 报告 `clean=true, remaining=0, infra=true, permits=true`，进程退出后立即成功重绑原 QUIC UDP 端口。 |

300 秒测试在第 35 秒开始黑洞，第 45 秒的请求超时，第 47 秒会话仍是 New，第 60 秒第一条消息恢复落库，即约 **25 秒业务中断**。第 115 秒恢复 UDP，已有 TLS/TCP 会话不主动升级；随后新拨的 QUIC-only 客户端重新通过 QUIC 建连并完成弱网业务。这里分别验证了故障自动回落、恢复后的新拨号，不把它们混称为自动升回 QUIC。

每 10 秒抽样。服务端 RSS 约 37–46 MiB，FD 36–50；两套长测的 SDK 稳态均为 34 FD。服务端同时承载了业务对照与长测，不能把这些区间当作泄漏证明；样本没有覆盖连接风暴、长时间 idle 或生产峰值。首次预跑的 3 秒注册预算低于 Debug 服务端的实际注册耗时；另一次预跑误用本地 ID API 查 canonical ID，两者均保留原始日志、不计入上表。

## 统一评审与放行条件

1. **业务门禁仍红。** 缺附件密钥、指标端点未监听，以及黑名单、订阅/房间权限、会话恢复、Outbox、资料及已读等红项均在 QUIC 和 TCP 重现。共同失败只能排除“仅在 QUIC 发生”，不能自动判定每一项都是测试问题。先补齐独立环境的附件、metrics、应用联动，校正夹具的前置/收尾；对剩余红项逐条隔离复现并修复，或记录明确批准的豁免。全量跑不能用默认跳过抵消失败。
2. **切换成功不等于恢复速度达标。** 25 秒是本次 10 秒预算下的单次测量。正式验收先定义可接受的业务恢复 SLO，分开统计掉线识别、QUIC 尝试、备用协议建连、重认证与差量恢复；Release 构建至少复验多档延迟/乱序/突发丢包和反复黑洞，报告恢复用时分布及是否丢消息、重复、乱序。
3. **发布级长稳尚未完成。** 用 Release 与实际部署配置至少跑 1 小时，再做按业务规模定的多客户端重连风暴；内容至少包括并发消息、空闲/唤醒、UDP 抖动及服务端滚动重启。采样 RSS、FD、活跃会话、pending 请求、队列积压和尾延迟；故障恢复后积压应可消化，排空后对象及端口必须回收，不接受持续增长或崩溃。
4. **核对待发布物。** 正式候选提交重跑协议 feature、wire fixtures、双向旧版互通、严格 Clippy 和文档测试；从实际打包产物干净安装到 server/SDK 再做定向互通。优先补集成门禁，不把 TCP 批量写等可选性能改动塞进本次验证。

本机原始材料：`/tmp/privchat-ga.MVqJmO/logs/`（`accounts-e2e.log`、`accounts-tcp-control.log`、`accounts-cold-fallback.log`、`accounts-ws-fallback.log`、`soak.log`、`soak-quic-only.log`、`relay.log` 和资源 CSV）。同一临时目录保留中继、业务驱动及汇总脚本，不提交原始日志、密钥、数据库或构建产物。
