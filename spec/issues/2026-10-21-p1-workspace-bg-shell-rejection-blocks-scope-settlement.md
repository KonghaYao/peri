# P1：后台 Shell 并发拒绝使 task scope 永久未结算

状态：根因已核实，待修复。日期：2026-10-21。核查基线：`57d85257159592b3875c5c6aa046a16bbfd42408`（origin/main）。

## 故障与证据

现场任务 `datacurve/kgateway-consistent-hash-policy`，证据目录 `bugs/2026-10-21-peri-411-acp-shutdown/`（peri-test `f2d4c23d`）。

1. `15:15:03.288872` 后台 Shell 注册被并发上限拒绝：`failure_class="concurrent_limit"`，模型面回执 `background shell concurrency limit reached (shell 5/5)`（`evidence/peri-print.2026-10-20.log:29-30`，`evidence/peri.txt:270`）。该日志只有这一次启动失败。
2. `15:15:11-13` 已有 8 个 `success=false` 的 bg-shell 完成（同日志 33-40 行）；`15:23-26` 仍有 `success=true` 完成（48-52 行）。并发位后来已释放，但 scope 的未确认标记没有清除路径。
3. `15:26:52.254709` 采用规范快照；`15:27:12.343779` `session/close` 失败：`code=-32010`，`Session close incomplete: workspace task owner workspace close incomplete`（54-55 行）。间隔 20.09 s，与客户端 20 s 轮询预算吻合。
4. `15:27:12.345610` host shutdown 记录 `Incomplete { session_close_failures: 2 }`，随后 `ACP deployment shutdown did not complete: Incomplete`（56-59 行）；进程 exit 1，评测记录 `NonZeroAgentExitCodeError`（`result.json` → `exception_info`）。
5. `evidence/verifier-reward.json`：`reward=0`、`f2p 0/2`、`p2p 214/214`；`commit-status.txt=committed`。`peri.txt` 中 108 个 assistant turn、9,917,413 tokens（input 96,922 + cache_read 9,775,616 + output 44,875）。

版本差异：事故版本 `agent-v4.1.1` 的 `SHELL_LIMIT` 为 5；origin/main 已在 `ee2b491f5`（2026-10-19）调整为 20。`shell_tasks.rs` 在两者间无差异，缺陷不随上限调整消失，只会更少触发。

## 根因链

1. 注册表在 `current >= limit` 时立即返回 `BackgroundRegistryError::KindConcurrentLimit`，发生在任何进程创建与任务登记之前（`peri-agent/src/agent/async_tasks/registry.rs:563-581`）。
2. `spawn_scoped` 在启动前把 `ScopeAdmission.creation_unconfirmed` 置为 `true`，只在 `spawn_scoped_owned` 返回 `Ok` 时清零（`mcp-packages/workspace/src/shell_tasks.rs:275-292`）。并发拒绝返回 `Err`，标志保持 `true`。
3. `ScopeAdmission::drop` 把该 scope 写入 `failed_creation_unconfirmed`（`shell_tasks.rs:739-744`）。
4. `snapshot().resources_settled` 从此永久为 `false`（`shell_tasks.rs:512-513`）；`open_scope` 永久拒绝（`shell_tasks.rs:598-603`）。没有清除 `failed_creation_unconfirmed` 的路径。
5. 客户端 `taskClose` 轮询 20 s 后返回 `workspace task owner ... close incomplete`（`peri-middlewares/src/mcp/client/subscription_tasks.rs:204-233`），`session/close` 映射为 `-32010`（`peri-acp/src/host/requests/session_close.rs:110`），host shutdown 记为 `Incomplete`（`peri-acp/src/host/shutdown.rs:188`），print 模式按失败退出（`peri-tui/src/acp_client/deployment.rs:57-58`）。

## 上游归因更正

上游 issue（`aos-test/peri-test#2`）把 close 失败归因于缺少 task scope capability 与 `state_event_dropped`，均不成立：

1. `missing task scope capability` 只影响启动时 `listen` 的无 `_meta` 调用（`mcp-packages/workspace/src/workspace.rs:768-772`）。工具调用 scope 由客户端延迟签发；close 已解析到 owner，否则错误应为 `... scope ... unavailable`。
2. `state_event_dropped` 是 UI 状态通道背压（`peri-acp-types/src/event_v2/bus.rs:95-97`），事件枚举不含后台任务完成；close 走 `workspace/taskSnapshot` 轮询。日志中出现 3 次，均在 close 前后，不是触发器。

## 修复契约

1. 确定性准入拒绝（`KindConcurrentLimit`，拒绝时未创建进程）不得把 scope 标记为“创建未确认”。
2. 启动失败分类必须在 `spawn_scoped` 内以类型可见，不能依赖错误文本或错误码反推。
3. `WorkerPanicked`、`WorkerCancelled`、`WorkerStopped`、`Unclassified` 保留未确认语义：不新增通用清除后门，除非有独立证据证明无进程。
4. 回归测试：并发上限拒绝并清理占位任务后，`resources_settled == true`，`close_scope`/`open_scope` 成功；既有 panic/普通创建失败测试继续断言未结算。

## 附带问题（独立记录，不纳入本次修复）

1. print 模式把任何 cleanup 失败都映射为 exit 1，不区分“任务已完成、仅收尾失败”；源码无 `--strict-cleanup` 开关（`peri-tui/src/acp_client/deployment.rs:57-58`）。
2. `listen` 无条件要求 task-scope capability（`workspace.rs:768-772`）；本次未证明该错误有独立危害。
3. `--max-turns` 仅被接受、不生效（`peri-tui/src/cli_print.rs:103-110`）；本次 108 turn 超出 50。

## 验收清单

- [x] 并发上限拒绝后，占位任务全部终结时 `snapshot.resources_settled == true`（`concurrent_limit_rejection_is_reported_as_backpressure_not_launch_failure`）。
- [x] 上述状态下 `close_scope` + `open_scope` 成功（同用例断言 epoch 递增）。
- [x] panic/普通创建失败仍保持未结算（`failed_creation_without_task_row_cannot_report_scope_settled`、`panicked_creation_without_task_row_cannot_report_scope_settled`）。
- [x] `peri-mcp-workspace --lib` 478 passed；wire 用例通过；改动文件低于 1000 行（仓库仍有既有超阈值文件）。
