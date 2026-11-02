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
10. 命令需审批：`monitor_start` 加入默认敏感分支（按原始名归一）。一次工具审批覆盖后续 alert 提醒；提醒路径不调用任何交互审批。输出一律 untrusted；不执行输出内容；不做 WS/SSRF。
11. 附随 skill 走 workspace 静态资产路线（与 cron 同形），内容通用，不出现平台专有词。

## P2 alert 提醒契约（已实现）

1. **alert-only**：`monitor_start` 增加 `alert`（线性时间正则，≤512 字符）。命中事件累计计数并更新 `lastAlertSeq`；未命中只进 ring buffer。
2. **`_meta` 投影**：`tasks/get` 顶层 `_meta["peri/monitor"] = { alertMatched, lastAlertSeq, label }`（camelCase；`lastAlertSeq` 未命中为 `null`；不含事件正文）。只在 `tasks/get` 写；`CreateTaskResult` 现有字段不变。
3. **owner 定向**：提醒只投递给发起 session（订阅参数 `session_id`，子 agent 即子 session id），不广播；无 inbox 时丢弃、推进 processed、记录 warn，不重投、不跨会话。
4. **节流与首轮**：宿主每任务内存 `processed = 0`、`next_allowed: Option<Instant>`。首轮 `alertMatched > 0` 立即投递（包含启动以来的全部命中，不以首轮值做基线）；非终态投递后 30 秒内抑制且 `processed` 不推进；窗口结束后以累计值投递。服务端无 outbox / draft / ack。
5. **正文边界**：提醒只含 raw monitor id、label、`alert_matched`、`last_alert_seq`；不含事件原文。正文是这四项的纯函数，不含时间 / delta / 终态标记 / pending 数；事件由 agent 用 `monitor_read` 拉取，始终 untrusted。`lastAlertSeq` 只是提示，读取游标由调用方用 `after_seq` 维护。
6. **幂等与失败**：`delivery_id = f(raw_monitor_id, alert_matched)`；同一 `(id, hits)` 正文逐字相同；reminder 构造失败是确定性失败，推进并 warn。
7. **接线**：复用既有 `spawn_managed_task_subscription` 的 `tasks/get` 轮询（2 秒）读取顶层 `_meta`，不新增 ACP 方法或任务生命周期方法；没有独立 wake 泵、outbox 或 host-only 工具。
8. **终态 flush**：观察到终态且 `alertMatched > processed` 时，在 `deliver_managed_task_status` / `settle_external` 之前无条件立即投递最终摘要（绕过 30 秒）；`alertMatched == processed` 时静默结算。终态投递失败或无 inbox 不得阻塞结算。
9. **meta 缺失**：`_meta` 缺失或字段类型错误时非终态不推进计数、每任务记录一次 warn、下一轮重试；终态观测遇到该情况只跳过告警投递，仍照常结算（`deliver_managed_task_status` / `settle_external`），不阻塞任务结算。身份判定以 `ConfigSource::Builtin { instance: "monitor" }` 为权威，名称回退兜底。
10. **不做**：poll/file/WS source、资源通知 hint、`notifications/resources/updated`、unknown task 快速收口、cron 改动、持久化、跨会话共享。

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
7. P2：alert 非法正则拒绝、未命中无提醒、命中产生提醒、30 秒合并、终态 flush、`tasks/get` `_meta` 的 camelCase 与 `lastAlertSeq` 更新。
8. P2：首轮命中即投、窗口内抑制且不推进、窗口后投累计值、离线丢弃并推进、`_meta` 缺失不推进、同一 `(id, hits)` 正文逐字相同。
9. P2：只投发起会话；终态无未报告命中不提醒；alert 命中后立即完成只有一条对应提醒；非 monitor 实例终态投递不变、实例别名同样抑制；取消与 deadline 终态不唤醒（等待 `has_unsettled_external() == false`）。
10. 边界：`label` / stop `reason` ≤256 字节；`monitor_read` 的过小 `max_bytes` 仍返回一条完整事件并置 `truncated`；`mcp-` host id 调 monitor 工具返回 `unknown task`，`monitor-` id 驱动 read/list/stop。

## 不做清单

- poll source、file source、WS source。
- 自定义任务生命周期方法或 ACP 方法。
- 任务持久化、重启恢复、跨实例重放。
- 跨会话共享与 owner 转移。
- （P2 已实现 alert 提醒，移出不做清单。）
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

P2 架构收缩（alert 改用任务轮询 `_meta` 驱动）最终验证（worktree 分支 `feat/issue-317-monitor-mcp`）：

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` | 通过 |
| `./scripts/cargo-rmcp-patched.sh build --locked --workspace` | 通过 |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-monitor --lib` | 46 passed（含 `_meta` camelCase、`lastAlertSeq`、wire 往返与工具面收缩） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib` | 1860 passed；1 failed（既有网络用例 `plugin::marketplace::tests::test_fetch_url_cache_fallback` 因外部 HTTP 502，与本变更无关）；2 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::monitor_alert::tests` | 9 passed（首轮即时、30 秒抑制、累计值、终态绕过、delta=0、离线推进、meta 缺失、正文逐字相同） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- monitor_subscription` | 7 passed（id 契约、取消/deadline、只投发起会话、终态 flush、离线结算） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::client::subscription::tasks::tests` | 17 passed（含 monitor 终态短路、非 monitor 回归与实例别名） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | 691 passed |
| `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings` | 通过 |
| `bash scripts/check-layer-imports.sh` | 通过（违规边 0） |
| `bash scripts/check-mcp-data-boundary.sh` | 通过 |
| `bash scripts/ci-test-inventory.sh --check` | 通过（11 个必跑套件） |

未运行：`scripts/check-file-size.sh`（本机缺 `rg`；本次改动文件均 < 1000 行）、`lefthook` 与 `cargo-nextest`（本机未安装）。
