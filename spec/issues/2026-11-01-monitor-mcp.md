# MonitorMCP：command source 监控实例

- 状态：P1 + P2 实施完成，待 PR 合并
- 日期：2026-11-01
- 来源：<https://github.com/KonghaYao/peri/issues/317>
- 范围：`mcp-packages/monitor`、`peri-acp-types`、`peri-middlewares`、`peri-tui` CLI 帮助、workspace builtin skill 与相关文档/测试
- 目标：新增独立 builtin MCP 实例 `monitor`，v1 只提供 command source：运行一条 shell 命令，把 stdout/stderr 转成可增量读取的事件；任务生命周期走标准 MCP Tasks。

## 背景

现有 builtin 实例没有「持续观察外部过程并增量取回」的能力。agent 监控长命令、日志 tail、目录变化或流式 feed 时只能反复执行一次性 `Bash` 快照：无法复用轮询进程，无法只读取新增行，也容易重复输出或丢失边界。

## 已确认决策

1. MonitorMCP 是独立 builtin MCP 实例，与 `workspace` / `cron` 平级；crate 为 `mcp-packages/monitor`，包名 `peri-mcp-monitor`。
2. v1 只做 command source。agent 自己编写 shell 轮询命令并负责业务 diff；MonitorMCP 只负责「命令输出 → 事件」。
3. 任务通信只使用标准 MCP Tasks：`tasks/get`、`tasks/cancel`。不新增自定义任务生命周期方法，不新增 ACP 方法。
4. 不持久化。任务状态纯内存；重启或实例替换后旧 id 一律 unknown；不恢复、不重放。
5. 事件存放在内存 ring buffer，`seq` 单调。`monitor_read` 的 `after_seq` 排他，`nextSeq` 是下一次读取应传入的游标。终态任务仍保留未读事件：读取循环以页级 `truncated` 为准，`truncated == true` 就继续，`truncated == false` 且终态才停止。
6. 单条事件 ≤ 16 KiB；单次读取 ≤ 500 条 / 128 KiB；超限标记 `truncated` / `dropped`。
7. deadline 默认 1h、最大 24h；超范围请求拒绝。`monitor_stop` 幂等；`tasks/cancel` 只 ack，进程清理状态单独报告。
8. 唤醒只由 alert 命中触发；任务成功/失败/取消的终态只更新任务投影（`tasks/get`、`monitor_list`、Tasks 面板），不投递 reminder。未命中 alert 的正常结束不会拉起模型。
9. 任务归属发起会话；请求经 `_meta["peri/taskOwner"]` 携带 owner，事件/任务访问按 owner 过滤；不做跨会话共享。
10. 命令需审批：`monitor_start` 加入默认敏感分支（按原始名归一）。一次工具审批覆盖后续 wake；wake 路径不调用任何交互审批。输出一律 untrusted；不执行输出内容；不做 WS/SSRF。
11. 附随 skill 走 workspace 静态资产路线（与 cron 同形），内容通用，不出现平台专有词。

## P2 alert 唤醒契约（已实现）

1. **alert-only**：`monitor_start` 增加 `alert`（线性时间正则，≤512 字符）。命中事件才写 wake outbox；未命中只进 ring buffer。
2. **owner 定向**：wake 只投递给 owner session，不广播；owner 无 inbox 时不投递、ack `offline` 并计数，不重投、不跨会话。
3. **节流与容量**：每 task 30 秒最多一条 wake（窗口内命中合并并累加 `matched`）；outbox 上限 256，淘汰最旧并计入 `wakeDropped`。
4. **正文边界**：wake 只含 raw task id、label、seq、matched；不含事件原文。正文由 agent 用 `monitor_read` 拉取，事件始终 untrusted。
5. **一次审批**：`monitor_start` 的工具审批覆盖后续 wake；wake 路径不经过 HITL。
6. **幂等**：`delivery_id = f(task_id, wake_id)`；ack 失败下一轮重试，重复投递复用同一 canonical id。
7. **接线**：`monitor_wakes` / `monitor_ack` 为 host-only 工具；宿主 wake 泵每 0.5–1 秒按 owner 轮询；复用既有 inbox `Defer` + continuation，不新增 ACP 方法。泵只跟踪启动过 monitor task 的 owner，并在 binding 释放后的宽限期后移除，避免集合无界增长。
8. **终态不唤醒**：monitor 任务的终态结算（成功/失败/取消）只更新任务投影，不投递任何 reminder；alert 命中后立即退出不会产生重复唤醒。
9. **不做**：poll/file/WS source、资源通知 hint、`notifications/resources/updated`、unknown task 快速收口、cron 改动、持久化、跨会话共享。

## 目标模型

```text
agent
  │  monitor_start { command, cwd?, label?, deadline_ms?, max_events?, max_bytes? }
  ▼
MonitorMCP (builtin instance "monitor")
  ├─ MonitorState: 内存任务表（owner / 状态 / deadline / 事件 buffer）
  ├─ shell command（ProcessTree 持有；stdout/stderr 行读取）
  ├─ EventBuffer: ring buffer，seq 单调，16 KiB/事件
  └─ MCP Tasks: monitor_start → CreateTaskResult；tasks/get / tasks/cancel
  ▲
  │  monitor_read { task_id, after_seq?, limit?, max_bytes? }
  │  monitor_stop { task_id, reason? }
  │  monitor_list { include_ended?, limit? }
agent
```

任务状态机：

```text
working ──命令退出 0──▶ completed
   │
   ├──命令非零/信号──▶ failed
   ├──deadline 到期──▶ failed（命令被终止）
   └──stop / tasks/cancel──▶ cancelling（对外仍 working）──▶ cancelled
```

终态只提交一次；`monitor_stop` 对终态任务幂等；`tasks/cancel` 只确认请求，清理结果经后续 `tasks/get` 观察。

## 验收

1. `mcp-packages/monitor` 可构建、fmt、clippy；crate 内测试覆盖 ring buffer 增量/截断、状态机、stop 幂等、deadline 计算、未知 id 与 owner 隔离。
2. 注册表、策略键、dispatch、standalone、permission 敏感分支全部接线；宿主既有声明表一致性测试通过。
3. `peri mcp-start monitor --stdio` 可构造 handler。
4. `monitor_start` 经工具桥接返回 MCP Tasks 句柄，宿主既有 `tasks/get` 轮询能观察到终态；`tasks/cancel` 不新增方法。
5. 事件按 `after_seq` 排他、`nextSeq` 权威增量读取；超限标记 `truncated` / `dropped`；终态后仍按 `truncated` 继续读完存量事件。
6. 本地验证：fmt、workspace build、`peri-mcp-monitor` 测试、layer import / MCP data boundary / CI inventory 检查、clippy。
7. P2：alert 非法正则拒绝、未命中无 wake、命中一条 wake、30 秒合并、outbox 容量与 `wakeDropped`、ack 幂等、owner 隔离、wake 不含正文、`tasks/get` 计数。
8. P2：宿主泵 push 后 ack、ack 失败重试（同一 delivery id）、owner 无 inbox 丢弃计数、只投 owner、子会话 binding 释放后离线、monitor 缺失 no-op、pool shutdown 停止。
9. P2：终态不唤醒——无 alert 命中完成、失败/取消均不产生 reminder；alert 命中后立即完成只有一条 alert wake；非 monitor 实例终态投递不变。
10. 边界：`label` / stop `reason` ≤256 字节；`monitor_read` 的过小 `max_bytes` 仍返回一条完整事件并置 `truncated`；泵 owner 集合在 binding 释放后清理。

## 不做清单

- poll source、file source、WS source。
- 自定义任务生命周期方法或 ACP 方法。
- 任务持久化、重启恢复、跨实例重放。
- 跨会话共享与 owner 转移。
- （P2 已实现 alert 唤醒，移出不做清单。）
- 实例自发布 skill。
- WS / SSRF 相关能力。

## 验证记录

P1 基线验证（历史记录，worktree 分支 `feat/issue-317-monitor-mcp`，基线 `origin/main`）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `./scripts/cargo-rmcp-patched.sh build --locked --workspace` | 通过 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-monitor --lib` | 32 passed |
| `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-mcp-monitor --all-targets -- -D warnings` | 通过 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | 682 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib` | 471 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib` | 1840 passed；1 个既有网络用例 `plugin::marketplace::tests::test_fetch_url_cache_fallback` 因外部 HTTP 502 失败，与本变更无关 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- mcp_v4_wave2` | 9 passed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --bin peri -- cli_mcp_start` | 8 passed |
| `bash scripts/check-layer-imports.sh` | 通过（违规边 0） |
| `bash scripts/check-mcp-data-boundary.sh` | 通过 |
| `bash scripts/ci-test-inventory.sh --check` | 通过（11 个必跑套件） |

未运行：`scripts/check-file-size.sh`（本机缺 `rg`；新增文件均 < 1000 行）、`lefthook` 与 `cargo-nextest`（本机未安装）。

P2 与验证修复（B9/D17/D18/D19）追加验证（worktree 分支 `feat/issue-317-monitor-mcp`）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `./scripts/cargo-rmcp-patched.sh build --locked --workspace` | 通过 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-monitor --lib` | 50 passed（含 alert/wake/outbox/wire、非零退出、spawn 失败、读取进度与边界） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib` | 1853 passed；1 个既有网络用例 `plugin::marketplace::tests::test_fetch_url_cache_fallback` 因外部 HTTP 502 失败，与本变更无关 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::monitor_wake::tests` | 9 passed（含 bridge 自动接线、终态不重复唤醒、owner 集合清理） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::subscription::tasks::tests` | 17 passed（含 monitor 终态不唤醒与非 monitor 回归） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` | 1112 passed；1 ignored（含新增 Stop 抑制恢复用例） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | 689 passed |
| `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings` | 通过 |
| `bash scripts/check-layer-imports.sh` | 通过（违规边 0） |
| `bash scripts/check-mcp-data-boundary.sh` | 通过 |
| `bash scripts/ci-test-inventory.sh --check` | 通过（11 个必跑套件） |

未运行：`scripts/check-file-size.sh`（本机缺 `rg`；本次改动文件均 < 1000 行）、`lefthook` 与 `cargo-nextest`（本机未安装）。
