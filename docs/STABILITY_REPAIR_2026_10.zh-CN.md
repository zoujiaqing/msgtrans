# 稳定性修复与统一验收

## 目标与范围

修复 MsgTrans 与 privchat-server、privchat-sdk 接入中的资源生命周期和背压问题，不改线格式、不发布、不操作无关工作区。先用回归测试约束行为，全部修改完成后统一复审。性能目标是不以丢数据、取消写确认或扩大队列掩盖阻塞。

## 修复设计

1. TCP/WS 读写独立推进，由同一个连接任务拥有并取消；读端等待有界事件队列不能阻塞写端。控制关闭不排在数据队列之后。
2. actor 统一终结路径调用一次 on_disconnected；pump 的邮箱等待可取消。shutdown 完成必须覆盖回调与任务释放。
3. 服务端业务任务按 session 持有，断线先关闭任务入口并等待任务结束，再清认证/在线索引，防止晚到认证复活已关闭会话。SIGINT 使用可等待 shutdown。
4. SDK shutdown 使用独立拥有的任务，调用方取消不取消实际清理；所有调用方等待同一个完成条件。接入层保留不可重试错误分类。
5. 解压过程中执行连接 payload 上限；TCP connect_timeout 包含 TLS；退休 generation 从状态表移除。
6. 避免无日志需求时构造/解析 payload 日志。资源上限与仍需基准证明的优化分开记录，不宣称没有实测的性能收益。

## 复现与验收

| 场景 | 修复后必须满足 |
| --- | --- |
| TCP/WS pipe=1、mailbox=1，首个 RPC handler 暂停，后续帧填满队列 | 放行 handler 后应答可写出，shutdown 可收敛 |
| 满邮箱时取消 actor | on_disconnected 恰好一次，clean 不能早于回调完成 |
| 认证任务暂停后断线 | 清理不能先于业务任务终止；清理后不再重建认证绑定 |
| 取消 SDK shutdown 等待，再次调用 | 实际清理继续，后续调用等待同一完成状态 |
| 1 KiB 上限，压缩帧展开为 64 KiB | 不递交 handler，在受限解压中拒绝并关闭 |
| TCP 对端接受连接但不进行 TLS 握手 | 在配置的整体连接预算内返回错误 |
| 连续重连 | 退休代状态不累计，迟到事件不影响新代 |
| 不可重试传输错误 | SDK 不把它改成可重试字符串错误 |

最终运行核心单测与相关集成测试、feature 编译检查、两个下游的针对性测试和编译检查。记录实际执行结果；业务长稳和吞吐对比未执行时明确标记，不用历史绿灯替代当前证据。

## 已落地的资源与性能约束

- TCP/WS 使用同一个拥有者并发推进读写 future，不增加逐包任务。QUIC 保持既有读写任务结构。
- 新增每连接、每方向独立的字节预算，默认 64 MiB，可通过 `ServerLimits::buffer_bytes` / `ClientLimits::buffer_bytes` 设置，范围 64 KiB 至 1 GiB。容量是上限，不预分配对应内存。
- 字节许可随 payload/ext 的 `Bytes` 克隆存活，不在出队时提前释放。应用保留这些字节也会施加背压；单包超过预算立即拒绝。自定义 SPI 可以使用 `event_channel_with_limits` 继承相同约束。
- 预算约束保留的 payload/ext，不是进程 RSS 硬上限：当前解码、编码和 QUIC 批次缓冲，以及应用自行复制的内存另计。不能把“64 MiB 预算”宣传成“连接最多使用 64 MiB 内存”。
- 服务端 RequestContext 保留 `Bytes`，消除分发前整包复制；计数使用原子量，不逐包争抢统计写锁；统计采集不跨外部 await 持统计锁；日志只在级别启用时格式化，超过 4 KiB 或二进制内容只记录长度。
- 服务端断线先停止业务任务准入，再等待已接纳任务完成，再清身份绑定。不强行 abort 半途的认证操作。若业务任务永不结束，shutdown 必须报告不干净，不能虚报成功。
- SDK 的清理任务使用自身 RuntimeProvider，调用方取消等待不会取消真实清理；显式等待底层 client shutdown 后才报告完成。不可重试的建连错误停止协议回落和后台重试；已经建立的连接断开仍保留原来的网络重连语义。

## API 与接入影响

- 线格式及 fixtures 不变，无版本号修改、发布或 push。
- 核心新增上述预算接口；`TransportClient::shutdown(&mut self)` 放宽为 `shutdown(&self)`，支持通过 Arc 显式等待收尾。已有普通调用仍可用；公共 API 快照已用固定 nightly 工具重新生成。
- 服务端 RequestContext.data 改为 Bytes；构造器接受 Into<Bytes>，仍兼容原 Vec 输入。
- SDK 增加 `Error::TransportRejected` 保留永久错误分类；下游穷尽匹配 Error 时需覆盖该变体。SDK workspace 的 all-targets 检查已通过。

## 本次验收结果

基线：msgtrans `b41eedc`、privchat-server `24aedea`、privchat-sdk `9f5de68`。以下均为本次实际执行，不引用历史 CI 结果。

| 门禁 | 结果 |
| --- | --- |
| 五个原始复现：TCP/WS 饱和、TLS 超时、解压限额、满邮箱关闭回调 | 隔离导出的旧基线 0/5 通过；修复后 5/5 通过 |
| 核心新增回调完成屏障、字节许可克隆寿命、退休代清理等测试 | 通过；stability_regressions 共 6 项 |
| 核心 all-features 全套 | 197 单测/集成 + 26 doctest，共 223 项通过 |
| 核心 8 个协议 feature 组合 all-targets clippy | 全部通过，-D warnings |
| 核心 all-features all-targets clippy、rustdoc | -D warnings 通过 |
| 格式、API 快照、线格式 | 格式通过，API 快照已更新，fixtures 无改动 |
| SDK 完整库测试 | 371 通过 |
| SDK workspace all-targets | 通过，保留既有警告，不宣称下游零警告 |
| 服务端全库测试 | 616 通过、25 因缺测试数据库配置失败、12 ignored；随后新增共享 payload 测试单独通过 |
| 服务端生产目标 lib/bins/examples | 编译通过，保留既有警告 |
| 服务端 all-targets | 未通过：未修改的 assistant_direct_test.rs:192 缺 MessagePayloadEnvelope.links，不能算本轮绿灯 |

回归位置：核心 `crates/msgtrans/tests/stability_regressions.rs`；字节预算 `adapters/budget.rs`；服务端 `src/infra/session_tasks.rs`、`src/context.rs`；SDK lib.rs 的 shutdown 与永久建连错误测试。

### 相同参数的旧新对照压测

Debug 构建，100 连接，1 KiB request，预热 1 秒、计量 5 秒；每协议两轮，执行顺序交叉（旧→新、新→旧），压测期间无编译器竞争。12 次运行全部 0 error、发送数等于接收数。

| 协议 | 旧版两轮 RX msg/s | 新版两轮 RX msg/s | 两轮均值观察 |
| --- | --- | --- | --- |
| TCP | 42,020 / 65,861 | 57,799 / 48,972 | 约 -1.0%，基本持平 |
| WebSocket | 38,319 / 45,387 | 30,925 / 53,730 | 约 +1.1%，基本持平 |
| QUIC | 23,983 / 26,262 | 26,422 / 32,717 | 样本较好，但不足以承诺稳定增益 |

短窗口波动明显；这些数据只用于排查明显回退，不能证明 release 性能提升或长时间稳定。原始日志保存在本机 `/tmp/msgtrans-repair-bench/`，旧版复现日志为 `/tmp/msgtrans-repair-baseline-repros.log`。

## 统一复审与发布边界

本次代码、回归测试和生产编译覆盖了读写互阻、关闭回调遗漏、业务任务越过断线清理、SDK 取消安全、配置限额失效、退休代驻留及热路径复制/锁竞争。没有靠扩大队列、删除写确认或放松测试解决问题。

仍未完成的发布验收：配置独立测试数据库后重跑服务端数据库测试；处理既有 integration fixture 编译漂移后重跑 all-targets；release 构建在固定负载下做小时级业务 soak，观察 RSS、FD、会话任务、pending 请求和重连恢复。特别是服务端真实认证与数据库事务的断线交错，目前用任务所有权测试证明清理顺序，未冒充真实数据库集成验证。

结论：本轮修复已实现并通过上述可执行门禁，但尚不能据此宣称“所有业务测试全绿”“长期稳定已证明”或“可以直接发正式版”。
