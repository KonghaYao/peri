# transport 关闭后 is_loading 永久卡 true（spinner 与 heartbeat 空转，cancel 无法解卡）

**状态**：Open
**优先级**：高
**创建日期**：2026-08-05

## 问题描述

ACP transport 关闭/崩溃后，TUI 的事件 pump 随 transport 退出（`client.rs:312-315`），**不再有任何事件到达**，`ACP_STATE.is_loading` 永久停在 `true`：footer spinner 一直转、`entry.rs:241-271` 的 heartbeat 每 100ms 写一次空转。此时用户按 Ctrl+C 取消也**无法解卡**——cancel 链路（`CANCEL_TX → cancel_consumer → acp_client.cancel()`）只发 `session/cancel` notification，不写任何 atom，`is_loading` 没有任何兜底复位路径。无 loading 超时看门狗。

> **对抗验证升级（2026-08-05）**：实际后果比 issue 描述更重——transport 死亡后**整个 TUI 软锁死**：Ctrl+C 恒返回 Cancel（`event_handlers.rs:53-69` loading 判定，双击退出不可达）、`/exit`/`/clear`/keepgoing 全被 loading 门禁拦截（`input_area.rs:915-951`），唯一逃逸是终端层 kill。同根因旁路：`session/prompt` task panic（transport 存活）时 `send_response` 永不执行 → submit_consumer 的 `prompt()` await 挂死 → loading 同样永久卡死。P1 维持（升级表述见状态变更记录）。

来源：cancel 链路三方并行审查（Agent 2 确认，P1）。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| loading 永久卡 true | pump 退出（`client.rs:312-315`）后事件流中断；`is_loading` 置 false 的 7 条路径全部依赖事件到达（TurnDone/TurnInterrupted/TurnSuspended/Rewind//clear/thread_load/prompt 失败兜底） |
| spinner + heartbeat 永久空转 | `entry.rs:241-271` heartbeat 任务每 100ms 写一次（防 idle 休眠），卡死状态下持续 CPU 消耗 |
| cancel 无法解卡 | `spawn_cancel_consumer`（`submit_consumer.rs:398-427`）只调 `acp_client.cancel()`；cancel 路径全程不写 `ACP_STATE`，`clear_loading_state`（`submit_consumer.rs:383-387`）只在 prompt RPC 失败时调用 |

## 复现条件

- **复现频率**：进程内 server task 异常死亡时必现（触发面较窄但后果最重）
- **触发步骤**：
  1. agent 运行中（`is_loading=true`）
  2. ACP server 的 inline handler（`acp_server/mod.rs:145-198`）panic → 该 tokio task 死亡 → `MpscServerTransport` drop → `server_tx` drop → client 内部 pump 退出（`mpsc.rs:83-91`）→ TUI pump 退出（`client.rs:312-315`）
  3. 无事件再到达；notifier（`acp_notifier.rs:55-63`）/bridge（`acp_bridge.rs:105-107`）channel 关闭时均只 break，不复位 loading
  4. footer spinner 永久旋转；Ctrl+C、`/exit`、`/clear` 全部被 loading 门禁拦截（软锁死）
- **环境**：TUI 主进程（ACP server 是进程内 tokio task，`launch.rs:253-256`；mpsc 双通道，无协议级 transport close）

## 根因分析

1. **loading 状态完全事件驱动**：`is_loading` 置 false 的所有路径都要求"事件到达"；transport 死亡后事件源消失，状态机停在中间态。
2. **cancel 不参与状态复位**：cancel 只是"请求"，其成功/失败均不写 atom（成功依赖 ACP 回 `TurnEnded` 事件——transport 已死时永远等不到）。
3. **无看门狗**：无 loading 超时、无 transport 死亡检测联动（`is_loading` 复位与 transport 生命周期解耦）。

## 涉及文件

- `peri-tui/src/acp_client/client.rs:312-315` —— pump 随 transport 关闭退出
- `peri-tui/src/kit/atoms.rs:51` —— `ACP_STATE.is_loading`
- `peri-tui/src/kit/entry.rs:241-271` —— heartbeat 空转（每 100ms）
- `peri-tui/src/kit/submit_consumer.rs:383-427` —— `clear_loading_state`（仅 prompt 失败兜底）/ `spawn_cancel_consumer`（不写 atom）
- `peri-tui/src/kit/message_area/footer.rs` —— spinner 渲染（`was_loading` 状态依赖事件）

## 修复方向

1. **cancel 兜底复位**：`spawn_cancel_consumer` 收到信号时先本地复位 `is_loading=false`（与服务端事件幂等兼容），或在 `acp_client.cancel()` 失败分支调用 `clear_loading_state`。**注意**：复位后 Ctrl+C 双击退出路径即恢复可用（`event_handlers.rs:53-69`），修复验收应显式加入"transport 死亡后 Ctrl+C 可退出"断言。
2. **事件流中断联动**（对抗验证修正落点）：notifier 的 channel-close 分支（`acp_notifier.rs:58-61`，kit 层已有 atom 访问权）复位 `is_loading=false` + 清 `INPUT_BUFFER` + 提示断连；比在 `client.rs` 注入 atom 依赖更干净。检测条件应为**事件流静默中断**（而非仅 transport 关闭），以覆盖 prompt task panic 旁路。
3. **loading 看门狗**：`is_loading=true` 超过阈值（如 60s）无事件刷新时告警并复位（需区分长任务与卡死，可与 keepalive/heartbeat 对齐）。
4. 补测试：transport 关闭后 `is_loading` 复位、cancel 在 transport 死亡时的行为。注意 `event_handlers_test.rs:24-37` 现断言"loading 状态下恒返回 Cancel"，修复后需同步更新。
5. 清理死文案：`app-agent-disconnected`（locales/en/main.ftl:276、zh-CN/main.ftl:275）在 Rust 代码中零引用。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（来源：cancel 链路三方并行审查，P1；待对抗验证） |
| 2026-08-05 | Open | Open | agent | 对抗验证：**证实，P1 维持且后果升级为 TUI 软锁死**（Ctrl+C 退出、/exit、/clear、keepgoing 全被 loading 门禁拦截；唯一逃逸为终端层 kill）。确认：is_loading=false 全部写入点（事件驱动 3 条 + 用户动作 2 条 + 失败兜底 1 条）在 transport 死亡时全部不可达；cancel 链路确认不写 atom；全仓库无看门狗。触发条件修正：进程内 server task panic → Arc drop → 通道关闭（非协议级 transport_close）。补充旁路：prompt task panic（transport 存活）同样产生永久 loading。修复落点修正：notifier channel-close 分支 |
| 2026-08-05 | Open | Open | agent | 修复 v1（notifier channel-close 兜底复位 is_loading + 清 INPUT_BUFFER + 断连通知 + heartbeat；cancel 兜底先 clear_loading_state 再 cancel().await；+2 测试；peri-tui 839 passed） |
| 2026-08-05 | Open | Open | agent | 修复验证：**有条件通过**。cancel 兜底复位路径真实打通验收核心（transport 死亡后 Ctrl+C 可解卡、prompt task panic 旁路可解卡）。但 notifier channel-close 兜底是**死代码**：生产 wiring 下 notification_tx 有多个永活 sender（ACP_CLIENT_HANDLE OnceLock、entry.rs client 变量、各 consumer 克隆），recv() 永不返回 None，acp_notifier.rs:66-72 永不触发；对应新测试为假阳性（单 sender channel）。验收断言"transport 死亡后 Ctrl+C 可退出"无链路测试。另发现预先存在的测试竞态（test_handle_clear_submit_resets_popup_todo_history_atoms，非本修复引入）。待跟进：重接触发机制（pump 独占 sender 或显式断连信号）或改看门狗 |
| 2026-08-05 | Open | Open | agent | 修复 v2（方案 A：pump 独占 sender）：删除 AcpTuiClient.notification_tx 字段（struct 加"勿加回"注释）；new() 返回 (Self, tx, rx) 三元组；spawn_pump 按值移交 sender（pump 退出即 drop → channel 关闭 → notifier v1 兜底原样复活，acp_notifier.rs 零改动）。launch.rs wiring 两行；5 个 consumer 测试机械解构。+2 验收级测试：test_real_wiring_transport_death_resets_loading（真 wiring，v1 下必失败/A 下通过，防回归守卫）、test_disconnected_recovery_makes_ctrl_c_quit_reachable（Ctrl+C 链路）。peri-tui 841 passed；v1 两个"假阳性"测试在 A 下语义转为有效 |
| 2026-08-05 | Open | Open | agent | 修复验证（workflow review）：**有条件通过**。方案 A 真实落地：struct 仅剩 transport/session_id 两字段（client.rs:68-75）、spawn_pump 按值 move（client.rs:123-129）、pump None 分支 break 即关闭（client.rs:333-336）、launch.rs:258-259 接线、notifier 兜底保留（acp_notifier.rs:58-74）、v1 cancel 兜底保留。真 wiring 测试断言 is_loading 复位/INPUT_BUFFER 清空/断连通知/heartbeat。Ctrl+C 退出链路测试存在。未满足项仅：workspace clippy -D warnings 未全绿（panels/workflow.rs 预存违规，非本修复引入）。待处理：合并评审（与 issue 1 的 request_id 改动共存验证） |
