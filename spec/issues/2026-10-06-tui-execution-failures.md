# TUI 执行失败、后台结算与恢复缺口

状态：调查完成；并发 MCP 准入定向测试通过，真实四路 Read 待验收，其余问题待实施。

## 证据与边界

- 输入：`.tmp/agent-tui.2026-10-06`；日志包含多次启动及 E2E 会话，不能把整份日志视为同一次运行。
- 只读核对：本机 `~/.peri/threads/threads.db` 的 work state、invocation 与 mutation receipt；未修改用户会话、控制状态或执行注册表。
- 本文时间统一用 2026-10-06 UTC；北京时间为 UTC + 8。
- active issue 仅记录尚需验收或实施的工作，不替代设计与标准。

## 1. 并发 Read 的冗余准入写入（已修复，真实运行待验收）

会话 `01a10f9a-75d6-7231-a6fe-80381ae664a3` 在 05:07:21.555647 报 internal error。
持久 work `01a10f9b-eb57-70f3-b5e2-02b3f4307f88` 的 stage 为 `blocked`，reason 为
`MCP invocation mutation was rejected: StaleRevision`。

同一批包含四个 Read，输入分别是 SDK 的 `src/wasm.ts`、`src/wasm/loader.ts`、
`src/transport/wasm-transport.ts` 与 `src/sdk/index.ts`。
前两个 invocation 为 `dispatchAccepted`，后两个为 `outcomeUnknown`。
receipt 保存多条 `mcp-invocation:*` 的 `staleRevision` 拒绝。
这不是后两个文件不存在的证据；检查时两个文件均存在。

Agent 的 `CommitReasonResponseAndDispatchIntent` 与 `BeginDispatch` 已持久提交 intent 和 dispatch 准入。
`McpInvocation::prepare` 随后重新提交 `PrepareInvocation`，其 reducer 对相同 intent 只校验已存在记录，
但 mutation 仍占用全会话 revision。并发工具争用该 revision，三次重试耗尽会把本可执行的读取阻断。

本次修复删除重复 mutation，保留只读 intent、状态、生命周期和 control 校验。
不延长重试次数、不重发可能已产生副作用的调用、不删除现有 blocked work。

验收：

- 同一 invocation 多次及并发准入不改变持久 work state/revision。
- intent 身份、参数及 owner 不一致仍拒绝；未知结果仍禁止重复 RPC。
- MCP invocation 与 owner recovery 相关套件通过。
- 原四路真实 Read 需用新会话验证；旧会话的 `outcomeUnknown` 需原 owner reconciliation，不能自动视为未执行。

验证结果：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- invocation`
通过 12 个测试（含 16 路并发只读准入与 owner recovery）；修改文件的 rustfmt check 与 `git diff --check` 通过。

## 2. 后台终态投递 Pending 无持续结算（待实施）

05:02:46.977869 与 05:02:54.761193 的后台任务报
`terminal publication Pending: owner must retain and retry until durable receipt`。
04:32:32.703133 的另一任务同样停在 Pending，04:56:55 关闭仍报 `is completing`。

`session/bg_complete.rs` 的同步 callback 首次调用启动异步 durable delivery 并返回 Err。
`async_tasks/settlement.rs` 因此保留 `pending_deliveries`，这是正确的所有权保留。
调用方 `subagent/background.rs` 只记录错误并退出。
当前重试入口是新执行的 `session/exec/executor.rs` 与 TaskManager shutdown；
本次扫描未发现后台 receipt 确认后自行触发 registry 终态结算的入口。

风险：delivery 的异步写入可能已经成功，内存 registry 却一直显示未结算。
不能把首次 Pending 当投递失败，也不能未取得 receipt 就强制 completed。

验收：无新用户 prompt、无 shutdown 时，异步 durable receipt 确认后 registry 自动完成；
失败继续保留原 identity/result，取消及关闭不丢终态责任。

## 3. 子 Agent 恢复拒绝导致主会话 domainWorkBlocked（待实施）

会话 `01a10f92-82bf-7fd3-9185-b33a2dd027f5` 的 work
`01a10f97-d31a-79c0-b090-fbe55540727f` 为 blocked，reason 为：

`resume_subagent: failed to load messages for 01a10f97-9a9d-7db3-9d0f-2e3446359569: Blocked: saved child runtime identity or authorization unavailable`

05:03:14.869812 与 05:03:18.702678 的继续请求被 SDK 以 `domainWorkBlocked` 拒绝。
子会话本身另有 blocked work，reason 同样为 MCP `StaleRevision`。

`subagent/factory/resume.rs` 把版本、child identity、lifecycle、frozen digest、model name、
授权与 tool origin/ceiling 校验合并成同一错误。
持久 metadata 确实存在；不能从这条错误推断是 metadata 丢失，具体失败条件仍需逐项核对。

缓冲输入的等待现象至少存在服务端 work blocked 的证据，不能只用复位 TUI loading 修复。
截图与上述会话的精确对应关系尚未证实。

验收：恢复按冻结的 child 身份/模型/工具授权执行，无法恢复时有结构化 blocked 原因；
客户端呈现 blocked/recovery 状态而非冒充仍有活跃计算；重放工具卡状态与实时执行状态分离。

## 4. workspace 订阅缺少 task scope capability（待实施）

05:05:25.769220 等位置返回 `missing task scope capability`，随后订阅 listen 退避重建。
通用 subscription setup 与 reconnect 使用无 scope 的 `peer.listen(filter)`；
任务专用订阅路径另有 `task_scope_meta_for`。
需要核实通用配置与 task scope 订阅的能力边界，不能仅忽略错误或不断重连。

## 5. internal error 可观测性缺口（待实施）

`ExecutionFailure::from_agent_error` 对非模型错误输出安全通用提示。
`v2_execute.rs` 的日志再使用同一 public projection，导致 “Check logs” 的指引指向同一句泛化提示。
本次只能依靠持久 work reason 找到具体的 StaleRevision 与 child 恢复失败。

验收：保留用户侧脱敏消息，同时记录结构化、allowlist 的失败阶段与 rejection kind，
不把任意 error cause、模型内容或凭据直接写入日志。

## 非主因与操作约束

日志还有远端 MCP TCP timeout、Langfuse OTLP 提交失败和状态事件通道满。
这些是真实故障，但当前证据不能用它们解释上述 Read 的 StaleRevision。
禁止为解卡修改数据库、清空队列、取消未知结果或绕过 SDK 准入；修复代码也不会自动修复已持久化的 blocked 会话。
