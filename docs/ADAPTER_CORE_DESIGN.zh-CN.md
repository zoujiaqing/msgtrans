# Adapter 内核设计 (1.0.10)

> 状态：**已落地**。`adapters::core`（ConnState + TaskGroup）就位，三个 adapter
> 全部接入；QUIC 的孤儿 task 收尾已修。三协议 live load test 0 error，QUIC
> A/B（vs 1.0.9）稳态吞吐/延迟无回退。泛型 EventLoopAdapter 按结论未做。

## 目标

架构最优 + 性能最优地收敛 TCP / WebSocket / QUIC adapter 的公共逻辑，
**不为收敛而收敛**：只抽真正同构的部分，把协议差异留在协议里。

## 勘查结论（为什么不做泛型 EventLoopAdapter）

三个 adapter 共 2348 行。逐方法核对后：

- `send` 已在 1.0.9 收敛为 `outbound::send_bounded`。
- `flush` / `event_stream` / `session_id`：逐字一致，但各 1–3 行 trivial。
- `close` / `is_connected` / `set_session_id`：骨架相似，但**各用不同字段表达
  连接状态**（TCP 看 `connection_info.state`，WS 看 `AtomicBool`，QUIC 看
  `event_loop_handle.is_some()`）——伪重复，语义不同。
- 读循环（主体）：TCP 半包重组 / WS 消息流 / QUIC 双 task 长度前缀——**本质
  不同**，是各协议存在的理由。硬塞进一个泛型 `EventLoopAdapter` 会把三种不同
  的控制流揉成一个带分支的抽象，可读性下降、收益为负。

**决策**：不抽泛型读循环。抽两样真正有架构价值的东西：统一的连接状态、
统一的 task 生命周期管理。

## 设计

### 1. `ConnState`：单一连接状态真相源

新增 `adapters::core::ConnState`，内部 `Arc<AtomicU8>`（Connecting / Connected
/ Closed），`Clone` 廉价共享给读循环。

三个 adapter 的 struct 用它替换各自的状态字段：
- TCP：不再从 `connection_info.state` 读状态（该字段仅作元数据）。
- WS：替换 `is_connected: Arc<AtomicBool>`。
- QUIC：替换 `event_loop_handle.is_some()` 这个**不准确**的判断
  （handle 存在不等于连接活着）。

`Connection::is_connected` / `set_session_id` 统一为一份实现语义。性能：状态读
是一次 `Relaxed` 原子 load，与现状同量级或更快（QUIC 少一次 Option 判断）。

### 2. `TaskGroup`：有主的 task 生命周期

新增 `adapters::core::TaskGroup`，持有子 task 的 `JoinHandle` 并在 `abort_all`
/ `Drop` 时**主动 abort**。

修掉 QUIC 的真实 bug：当前 `select! { _ = read_task, _ = write_task }` 按值
move，先完成的一方使另一方的 `JoinHandle` 被 drop——而 drop handle **不取消
task**，孤儿 task 靠 `shutdown_flag` 轮询退出（泄漏窗口 + 多余轮询）。

改为：`select!` 只借 `&mut` 等待任一结束，收尾时 `TaskGroup::abort_all()`
确定性取消另一方。read/write 分离的吞吐架构不变（性能不回退），只是收尾从
"轮询自退"变成"owner 主动取消"。

### 3. trivial 样板

`flush` / `event_stream` 保持各自一行——不值得为省两行引入间接层。
`session_id` 读取走 `ConnState` 附带的 session id 原子，自然统一。

## 明确不做

- 泛型 `EventLoopAdapter<S, F>` / `Framer` trait：收益已被 1.0.9 的
  `send_bounded` 吃掉，剩余为负收益。
- 不碰任何 `pub` 配置 / 构造签名——保持 1.0.x 兼容。

## 执行顺序

1. `adapters::core`（`ConnState` + `TaskGroup`）+ 单元测试。
2. QUIC 接入（含 task 收尾修复）——worktree A/B 压测验证无回退。
3. TCP / WS 接入 `ConnState`。
4. 全协议 live load test + 更新本文件为"已落地"。

## 验收

- `cargo test` + doctest 全绿；三协议 live load test 0 error。
- QUIC：主动断开后无残留 task（`select!` 收尾走 `abort_all`）。
- A/B（worktree v1.0.9）吞吐/延迟无回退。
