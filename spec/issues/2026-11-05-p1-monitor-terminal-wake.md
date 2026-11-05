# P1：monitor 任务终态不唤醒发起会话

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误（功能缺口）。
**创建日期**：2026-11-05。
**来源**：用户报告。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `f73828f5c`）。

## 确认证据

- 终态投递被显式短路：`peri-middlewares/src/mcp/client/subscription_task_binding.rs:40-51`
  （`is_monitor_server` 命中直接 `return Ok(())`）。
- 订阅循环终态分支只 flush alert、随后结算：
  `peri-middlewares/src/mcp/client/subscription_tasks.rs:126-163`。
- 工具回执明确告知模型只有 alert 命中会唤醒：
  `peri-middlewares/src/mcp/tool_execution.rs:26-33`。
- 契约与测试固化了该行为：`docs/design/monitor-mcp.md:257-260`、
  `monitor_subscription_test.rs::cancelled_monitor_terminal_state_never_wakes_the_owner`、
  `deadline_monitor_terminal_state_never_wakes_the_owner`、
  `subscription_tasks_test.rs::monitor_terminal_settlement_never_wakes_the_owner`。
- 用户观察：被监控命令退出/崩溃后没有任何 Defer 投递，空闲 loop 不会被唤醒。

## 根因链

1. 被监控命令退出，monitor 状态机提交 `completed` / `failed` / `cancelled` 终态
   （`mcp-packages/monitor/src/state.rs:747-776`）。
2. 宿主订阅每 2 秒 `tasks/get` 观察到终态，先处理未报告 alert，再调用
   `deliver_managed_task_status`（`subscription_tasks.rs:126-163`）。
3. 结算回调 `deliver_task_reminder` 对 monitor 提前返回成功，不构造也不投递提醒
   （`subscription_task_binding.rs:49-51`）。
4. 用户可见后果：未配置 `alert` 或 alert 已消费时，模型永远不知道任务已结束；
   TUI 任务投影更新，但 loop 无唤醒。

## 影响面

- 用户影响：monitor 的核心异步通知在无 alert 命中时完全缺失；失败/崩溃/deadline
  同样静默。
- 受影响模块：`peri-middlewares` 任务订阅与终态投递、`peri-acp-types` monitor
  提醒契约、monitor 工具回执文案、相关设计/代码索引文档。
- 是否可规避：命令尾部输出固定标记并配置 `alert`；或模型主动轮询
  `monitor_list` / `monitor_read`。

## 修复方案

用户裁决（方案 B）：**全部终态唤醒**，并与 alert 命中合并去重。

- 任何 monitor 终态都向发起会话投递一条终态 Defer（completed / failed /
  cancelled / deadline）。
- 终态观测时若存在未报告的 alert 命中（`alertMatched > processed`），不再单独
  投递 alert 提醒，而是把累计命中数与最近命中 seq 并入终态提醒正文；终态轮次
  最多一条提醒。
- 运行中的 alert 提醒保持 30 秒合并窗口不变。
- 离线（无 canonical delivery 且无 inbox）时终态提醒 best-effort 丢弃，任务照常
  结算；不得因投递失败阻塞结算或无限重试。
- 幂等：结算重试复用既有 delivery id，不重复唤醒。

明确不做：不改 `CreateTaskResult` / `_meta["peri/monitor"]` 协议；不改
`monitor_read` / `monitor_stop` 工具面；不做事件正文透传；不改数据库 schema。

回归测试：

- 终态无 alert：completed / failed / cancelled / deadline 各投递一条。
- alert 命中后立即退出：只投递一条合并提醒，正文含命中计数且 delivery id 为终态
  投递 id（不是 alert id）。
- alert 已报告后退出：仅一条终态提醒。
- 离线发起会话：无提醒但任务结算成功。
- meta 缺失/畸形：仍投递终态提醒并结算。
- 结算失败重试：提醒不重复。
- 非 monitor 实例终态投递行为不变。

## 验收标准

1. `monitor_start` 启动的任意任务到达终态后，发起会话收到恰好一条 Defer；
   正文含终态与 monitor id，必要时含合并后的 alert 计数。
2. alert 命中后立即退出的场景不产生两条提醒。
3. 离线与结算重试路径不阻塞、不重复投递。
4. 目标测试、`./scripts/check-changed.sh`、clippy 通过。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client`
  → 163 passed, 0 failed（含终态唤醒、合并去重、离线与结算重试用例）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- monitor`
  → 11 passed, 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-monitor --lib`
  → 55 passed, 0 failed。
- `bash scripts/check-changed.sh` → exit 0（20 个 crate 的反向依赖闭包）；
  仅 `rust-objcopy` 缺 `libLLVM.dylib` 的既有环境告警。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-acp-types -p peri-middlewares
  -p peri-mcp-workspace --all-targets -- -D warnings` → exit 0。
- `cargo fmt --all -- --check` → 通过。
- `git diff --check` → exit 0。
