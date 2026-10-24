# P1：后台任务查询能力缺失，长前台 sleep 未被拦截

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：错误 / 可观测性。
**创建日期**：2026-10-23。
**来源**：issue #274 历史统计 + 用户源码审查。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `68971133c`）。

## 现象

issue #274 统计全库 10,676 次 `sleep`（占 304,453 次命令调用的 3.5%）。其中：
- `no_info`（纯 sleep / `sleep; echo`）1,081 次；
- `after_background`（后台回执后仍 sleep）2,373 次；
- 前台长等待与连续轮询是主要浪费。

原 issue 的机制描述需纠正：前台默认 **15,000ms**、上限 **120,000ms**（`FOREGROUND_MAX_TIMEOUT_MS`）。`sleep 115` 只有在调用方显式抬高 `timeout` 时才会完整占用前台；默认参数下约 15 秒即触发超时转后台（`terminal.rs` 的前台 promote 分支）。因此修复重点是“主动抬高 timeout 的前台等待”，不是“所有调用都会卡满 120 秒”。

## 已确认事实

1. **参数说明引导长前台等待**：`run_in_background` 参数说明写“Only use for long-running servers, watchers, or daemons. For builds/installs/tests, prefer a longer timeout instead.”（`terminal.rs:117-122`）。`timeout` 参数说明同样建议“set a longer timeout ... or use run_in_background”（`terminal.rs:366`）。`descriptions/bash.md:14` 与两处后台回执（`terminal.rs:475-493`、`terminal.rs:690-700`、`workspace.rs:224-232`）与该建议不一致。
2. **没有面向具体任务的查询工具**：仓库中不存在 `TaskOutput`。模型只能读日志文件、看 Tasks 面板，或再次 `sleep` 轮询。`run_in_background` 回执只给 `task_id` 与日志路径。
3. **运行中任务没有保存输出句柄**：`spawn_owned_inner` 只保留 `handle.task_id`，丢弃 `handle.stdout_log/stderr_log`（`shell_tasks.rs:280-321`）；前台 promote 路径的 `OnBgStartedFn` 只传 id（`terminal.rs:76, 692-694`），日志路径只进入返回文本。
4. **公共 task_id 与内部 id 不同**：模型看到 `mcp-<sha256>`（`manager.rs:36-53`），Workspace 内部是 `shell-{uuid}`。host 侧没有可回查的 public → (server, raw, initiator) 映射；raw id 只存在于订阅闭包中（`subscription_task_binding.rs:173-235`）。因此查询必须经过可信映射与会话校验，不能截取前缀。
5. **完成投递已有单一机制**：`spawn_managed_task_subscription` 约每 2 秒查询远端 `tasks/get`，终态后投递一次（`subscription_tasks.rs`）；Workspace 终态全文保存在 `TaskPayload::Completed.result.structuredContent`（`shell_tasks.rs:192-210`），可通过 `tasks/get` 跨连接读取。
6. **无长 sleep 执行策略**：命令在任何策略检查前直接 spawn；`BashTool::execute` 与 `call_owned_bash` 都没有命令级等待识别。
7. **没有线程级 sleep 禁令**：issue 提议的“后台回执后禁止同线程 sleep”尚未实现，且该规则会误伤等待不同目标的合法短等待。

## 修复方案

范围限定为本问题；相邻问题（历史统计脚本、TUI 面板）不在本次修复内。

### 1. 统一工具说明与回执（P1）

- 改 `run_in_background` 参数说明：预计超过短前台窗口的构建、安装、测试优先后台；收到 task_id 后继续独立工作；需要诊断时用 `TaskOutput`；不要抬高 timeout 在前台等待。
- 同步 `timeout` 参数说明、`descriptions/bash.md`、显式后台回执、前台 promote 回执、host 侧 `task_started` 文本（`tool_execution.rs`）。三处以上文案必须表达同一行为规则。
- 服务就绪用健康检查，不等进程退出；CI 用有截止时间的后台 watch。

### 2. 新增 `TaskOutput(task_id, wait_ms=0, cursor?, max_bytes?)`（P1）

- Workspace 工具；`wait_ms` 上限 10,000ms，超过则收敛到上限并在回执中说明。
- 返回：状态、`wait_timed_out`、终态 `success/timed_out/duration_ms`、有界输出、下一 cursor。
- 等待到期返回 `running + wait_timed_out`；**不终止任务**，不把等待超时当任务失败。
- 等待复用 `ShellTasks.updates` 与现有完成状态，不新建完成通知。
- 输出从日志文件按字节 cursor 有界读取（新增 helper），完成后仍可查询。
- 显式后台与前台超时 promote 都在 `ShellState` 保存 `stdout_log/stderr_log`。
- 同任务连续 ≥3 次查询且状态/输出无变化时，返回 `no_progress` 诊断与 `consecutive_queries`（提醒，不是强制禁止）。
- 记录 `foreground_ms`（Bash 前台执行）与重复查询次数，便于后续统计。

### 3. 公共 id → raw id 的可信映射（P1）

- 在 host 侧任务登记成功后记录 `(session_id, public_id) → (server, raw_id)`；随 `SessionBindings::release` 清理。
- `McpToolBridge` 仅在 `TaskOutput` 调用时把 `task_id` 替换为 raw id；映射缺失、来源不匹配或会话不符时返回明确错误，不转发猜测值。
- Workspace 仍以 `peri/taskOwner` 校验任务归属，跨会话不可见。

### 4. 进程启动前拒绝可明确识别的长前台 sleep（P1）

- 门槛 60,000ms；覆盖 `;`、`&&`、`||`、`|`、换行、子 shell、`timeout`/`nohup` 前缀后的命令段，以及 `s/m/h/d` 后缀与 PowerShell `Start-Sleep`/`sleep` 形式。
- 跳过引号、注释、here-doc 与 `bash -c`/`sh -c` 脚本内容；不声称覆盖动态 shell。
- 纯长 sleep 在前台与后台都拒绝；包含其他命令的长 sleep 只在**前台**拒绝，并提示整体后台执行。
- 拒绝结果：`code = foreground_long_sleep`、`started = false`、检测到的时长、限制、替代操作（等待任务 → `TaskOutput`/完成通知；合法延迟测试 → 后台）。
- 检查发生在 `begin_external_execution`/spawn/日志创建之前，保证无副作用。

### 5. 提示词行为规则（P2）

按以下规则更新说明，不实现线程级 sleep 禁令：

> 预计超过短前台窗口的构建、安装、测试应后台执行。收到任务 ID 后继续其他独立工作；需要诊断时使用任务查询工具。任务仍运行且没有新信息时，交回现有完成通知机制，不用 sleep 或连续查询占用前台。只有获得实际终态后才能报告成功。服务就绪与进程完成分别检查。

## 验收测试

1. 长 sleep 策略：59s 允许、60s 拒绝；`1m`、`1.5m`、多参数求和；引号/注释/here-doc/`bash -c` 不误判；纯 sleep 与复合命令的前后台差异。
2. 拒绝前无副作用：不 spawn、不登记任务、不创建日志、不触发执行证据。
3. `TaskOutput`：显式后台可查；前台超时转后台可查；等待到期不杀任务；终态后可继续读增量；cursor 有界读取；`wait_ms` 上限。
4. 隔离：A 的公共 id 在 B 会话不可查；raw id 不通过前缀猜测；映射在会话释放后清理。
5. 竞争：查询与完成通知并发时不重复投递；连续 3 次无进展返回诊断。
6. 文案一致性：schema 说明、bash.md、两类后台回执、promote 回执都指向同一规则。
7. 现有工具清单/顺序/注册测试同步更新（7 → 8 个工具）。

## 非目标

- 不恢复旧 Agent 执行；不改变 `sleep < 60s` 的既有允许范围。
- 不实现线程级“启动过后台任务就禁止 sleep”。
- 不新增第二套完成通知或任务持久化。
