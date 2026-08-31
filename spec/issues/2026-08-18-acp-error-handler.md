# ACP 错误信息传递改造计划

**状态**：Ready for implementation
**优先级**：高
**类型**：协议兼容性 / 错误边界重构
**创建日期**：2026-08-18

## Problem Statement

Peri 当前对执行失败使用了两套不完整的表达：

1. 工具失败已经映射为标准 ACP `ToolCallStatus::Failed`，但错误文本只写入 `rawOutput`，没有写入标准客户端用于展示的 `ToolCallUpdateFields.output`；session replay 也存在同样问题。
2. 主 turn 的致命执行错误会发出私有 `peri/agent_event` 中的 `AgentExecutionFailed`，但 `session/prompt` 请求最终仍返回成功的 `PromptResponse`。未协商私有 capability 的标准 ACP 客户端因此无法从请求响应判断 turn 失败。

这不是 transport 编解码缺失：统一 host 已经把 `dispatch_prompt_turn` 返回的 `Result<Value, AcpError>` 交给 `AcpTransport::send_response`，mpsc 与 stdio 都能发送标准 JSON-RPC error response。真正的信息丢失发生在更早的 Agent→ACP 结果边界：`LoopResult::Error(AgentError)` 被压缩为 `ExecOutcome { ok: false, stop_reason }`，原始错误类别与脱敏后的用户消息没有进入最终 `PromptResult`；ACP 后处理只能把失败继续降成成功 `PromptResponse`。

### 当前代码事实

- `build_and_execute_agent_v2` 将致命 `LoopResult::Error` 记为 `ok=false`，同时发送 `AgentExecutionFailed { message: e.user_facing_message() }`，但返回的 `ExecOutcome` 不携带错误。
- cancel 与 `MaxIterationsExceeded` 也会得到 `ok=false`，但它们已有标准 `StopReason::Cancelled` / `StopReason::MaxTurnRequests`，不应误转为 JSON-RPC error。
- `run_prompt` 会根据 `result.ok` 决定历史持久化策略，却无条件把 `result.stop_reason` 构造成成功 `PromptResponse`。
- 统一 server loop 已经直接调用 `send_response(id, result)`；因此只要 `run_prompt` 在 fatal failure 时返回 `Err(AcpError)`，mpsc 与 stdio 会共用同一标准错误路径。
- `AgentError::user_facing_message()` 已为内部错误、LLM 错误和序列化错误提供脱敏消息，可作为 public message 的现有安全基线。
- live event mapper 与 session replay 都只设置 `status + raw_output`，没有构造标准 `output` content block。
- 标准 ACP `SessionUpdate` 没有 `turn_error` / `turn_complete` 变体；turn 失败应由 `session/prompt` 的 JSON-RPC error response 表达。

## Goals

1. 标准 ACP 客户端仅依赖协议标准字段，即可得到工具失败状态、非空错误文本和 turn 失败结果。
2. 保留现有取消、最大轮数和历史持久化语义，不把非 fatal 终止误报为请求失败。
3. mpsc 与 stdio 走同一错误映射，不新增 transport 特例或第二条事件链。
4. 对外错误文本统一使用脱敏消息，不泄露 provider body、stack、secret、文件内容或内部 debug chain。
5. 私有 `AgentExecutionFailed` 在兼容期继续服务已协商 capability 的 TUI；标准 JSON-RPC error 是 turn failure 的 canonical wire 结果。

## Non-goals

- 不新增私有 `turn_error` / `session_error` wire frame。
- 不给标准 `SessionUpdate` 扩展不存在的错误变体。
- 不重构全部 `AgentError`、provider error 或工具执行模型。
- 不改变单个工具失败后 Agent 是否继续推理的控制流；本次只修正失败信息的协议投影。
- 不在本仓库实现 RCS 的 TypeScript 接收逻辑；RCS 对齐作为独立交付依赖。
- 不移除 `rawOutput`，避免破坏依赖原始输出的现有客户端。

## Decision Document

### D1. 在 Agent→ACP 结果契约保留“终止类别 + public message”

为 `ExecOutcome` 及其下游 `PromptResult` 增加显式的可选 fatal failure，而不是复用 `ok` 或从 `stop_reason` 反推。该 failure 至少包含：

- 稳定的内部类别，用于 ACP 边界选择协议错误码和 allowlist `data.kind`；
- 非空、已脱敏且限长的 public message；LLM/provider 错误保留原始诊断含义，不再压成固定语义文案；
- LLM HTTP 错误额外保留 `status`，但不携带完整 provider payload 或 cause chain。

推荐使用窄 DTO（例如 `PromptFailure` / `ExecutionFailure`），不要把完整 `AgentError`、`anyhow::Error` 或 provider response 直接跨层传递。这样既保留结构化分类，也能阻止内部 cause chain 意外序列化到 wire。

分类只覆盖当前客户端诊断需要的稳定 taxonomy：

- `Internal`：非 LLM 的内部执行失败；
- `Llm`：无 HTTP status 的 LLM/provider 失败；
- `LlmHttp`：带 HTTP status 的 LLM/provider 失败。

完整原始 error 只进入受控 `tracing`；wire message 对 provider 原文执行凭据遮蔽、URL query 清洗和 Unicode 安全限长。

`Interrupted` 与 `MaxIterationsExceeded` 不写入 fatal failure：前者继续返回成功 `PromptResponse(Cancelled)`，后者继续返回成功 `PromptResponse(MaxTurnRequests)`。

### D2. ACP 边界负责映射 JSON-RPC error

`run_prompt` 必须先完成当前失败路径需要的历史保存、session state 清理、cancel token 清理和 recall 策略，再根据 `PromptResult.failure` 决定响应：

- `failure=None`：返回现有 `PromptResponse`；
- `failure=Some(...)`：返回 `Err(AcpError)`，由统一 host 和 transport 生成标准 JSON-RPC error response。

不要在 `run_acp_server`、`MpscTransport` 或 `StdioTransport` 中识别业务错误；这些层只发送 `Result`。

使用稳定 JSON-RPC server error code `-32000`（agent turn execution failed）。`message` 使用 failure 的非空、脱敏限长原文；`data` 是版本受控的 allowlist：始终包含 `kind`，`LlmHttp` 额外包含数值 `status`。不得将 provider payload、header、请求内容或 cause chain 放入 `data`。

### D3. 私有失败事件暂时双发，但不再是事实源

fatal failure 在兼容期同时产生：

1. canonical：`session/prompt` JSON-RPC error response；
2. compatibility projection：已协商 `agent_event` capability 时发送 `AgentExecutionFailed`。

私有事件继续使用同一 public message，避免两个通道展示不同内容。未协商 capability 时，标准 error response 仍必须存在。后续只有在所有内置客户端都消费标准响应且完成版本迁移后，才能另立 issue 评估移除私有事件。

双通道可能导致支持两者的客户端重复展示。客户端应以 request error 结束 turn/loading，并对同一 request/turn 的私有事件去重；本仓库内置客户端如同时消费两者，应在实现阶段补对应去重或只保留一种可见提示。

### D4. 工具失败写入标准 `output`，`rawOutput` 保持兼容

live mapper 与 session replay 对 tool result 使用同一投影规则：

- 始终保留当前 `status`：失败为 `failed`，成功为 `completed`；
- 将可展示文本构造成 `ToolCallContent::Content(ContentBlock::Text(...))` 并写入 `ToolCallUpdateFields.output`；
- 继续保留 `rawOutput` 的当前 JSON/string 表达，维持兼容和机器消费能力；
- 对失败 output 保证至少有一个非空、安全的文本块。若底层 output 为空，使用稳定通用文案，不允许前端因空串静默丢弃；
- replay 与 live 映射必须一致，避免恢复会话后错误内容消失或形态变化。

本次不把工具错误重新归类为 turn fatal failure。工具是否终止 turn 仍由现有 Agent 控制流决定。

### D5. 安全边界

- 对外 turn message 使用窄 allowlist formatter：LLM/provider 原文只在凭据遮蔽、URL query 清洗和 Unicode 安全限长后进入 wire；其他内部错误继续使用 `user_facing_message()`。
- 内部完整错误只进入 `tracing`，且字段仍须遵守 secret 规则；不得把完整 provider response body、认证 header、请求 payload、stack/backtrace 放入 `AcpError.message/data`。
- 工具 output 本身可能包含用户文件内容或命令输出，这是现有 tool result 语义；本改造只把已经允许传给客户端的 tool result 投影到标准字段，不额外拼接 debug error chain。
- 所有 fallback public message 必须非空，并在测试中断言不包含代表性 secret/cause 文本。

### D6. RCS 是独立但阻塞端到端验收的接收侧任务

RCS 不在当前 workspace，本计划不假定其文件可直接修改。需在对应仓库建立关联任务，至少完成：

1. 将标准 `ToolCallStatus` 的 `"failed"` 映射为 `tool_call_failed`；可继续兼容旧 `"error"`，但标准值优先。
2. 从 `session/prompt` JSON-RPC error response 读取 `{ code, message }`，映射为 turn failure，并可靠结束 loading。
3. 如果仍消费私有 `session_error` / `AgentExecutionFailed`，按 request/turn 去重，避免与标准 response 重复展示。
4. 对空 message 提供客户端 fallback，但不得因此放宽 sender 的非空契约。

Peri 合并条件可以只要求 sender 侧协议测试通过；跨仓库发布/上线条件必须包含 RCS 联调通过。

## Commits

以下每个提交都应保持 workspace 可编译，并能通过该步骤新增或受影响的目标测试。

### Commit 1 — 给执行结果增加显式 fatal failure 契约

- 在 Agent 执行结果边界增加窄的 failure DTO，并把它从 `ExecOutcome` 传入最终 `PromptResult`。
- `LoopResult::Completed`、用户 cancel、`Interrupted`、`MaxIterationsExceeded` 的 failure 均为 `None`。
- 其他 `LoopResult::Error` 写入稳定类别和 `user_facing_message()`。
- 保留当前 `ok`、`stop_reason` 与 `AgentExecutionFailed` 发射行为，避免第一步同时改变 wire。
- 增加 Agent 层单元测试，证明 fatal、cancel、max-iterations 三类不会混淆，且 public message 非空并脱敏。

**Verify**：`cargo test -p peri-agent --lib executor_helpers`

### Commit 2 — ACP 将 fatal failure 返回为标准 JSON-RPC error

- 在 ACP 协议边界定义具名 turn-failure error code 与单一映射函数。
- 保持 `run_prompt` 的历史持久化、session state 更新和清理顺序；全部后处理结束后，fatal failure 返回 `Err(AcpError)`。
- cancel 和 max-turn-requests 继续返回成功 `PromptResponse`，不回归现有终止语义。
- `AcpError.data` 首版保持 `None`。
- 增加 host 级测试，直接验证 fatal failure → `{code, non-empty safe message}`，cancel/max-iterations → successful prompt response。

**Verify**：`cargo test -p peri-acp --lib prompt`

### Commit 3 — 锁定 mpsc/stdio 统一 wire 行为

- 扩充统一 host/transport 的契约测试：同一 `session/prompt` fatal failure 在 mpsc 与 stdio 都返回 JSON-RPC `error`，保留原 RequestId，不出现同时含 `result` 与 `error` 的非法响应。
- 断言私有 capability 未开启时仍能收到标准 response error。
- 断言响应完成后 session info 收尾通知和 loading 终止所需时序不回归；测试不要依赖通知与 response 之外的内部实现细节。
- 如果现有测试夹具难以稳定触发 LLM failure，只增加最窄的可注入 seam，不引入 transport 专属业务分支。

**Verify**：`cargo test -p peri-acp --lib host` 与 stdio unified-host 集成测试

### Commit 4 — live tool result 补标准 `output`

- 提取或局部实现从 tool result text 构造 ACP `ToolCallContent` 的投影。
- `ToolEnd` 的成功和失败更新都写入 `output`，并继续写 `rawOutput`。
- 失败空文本使用稳定非空 fallback。
- 新增 mapper P0 测试，分别断言失败状态为 `failed`、错误文本位于 `output`、`rawOutput` 仍存在、空失败文本有 fallback；成功路径也要确认 output 结构。

**Verify**：`cargo test -p peri-acp --lib mapper`

### Commit 5 — replay tool result 与 live mapper 对齐

- session replay 的 tool result 同样写入标准 `output`，保留 replay meta 与 `rawOutput`。
- 复用与 live mapper 一致的文本/fallback 规则，或用共享纯函数消除协议形态漂移；仅在确实被两处使用时提取 helper。
- 增加 replay 测试，断言失败状态、output 文本、rawOutput 和 replay meta 同时存在。

**Verify**：`cargo test -p peri-acp --lib session_replay`

### Commit 6 — 内置客户端兼容与双通道去重

- 检查本仓库 TUI 对 `session/prompt` error response 的处理；确保收到 error 后退出 loading，并显示一次 public message。
- 保留 capability-gated `AgentExecutionFailed`，但对同一 turn 的标准 response error 与私有事件避免重复可见提示。
- 不增加第二套 canonical event，不让 TUI 直接依赖 Agent 层。
- 增加客户端状态测试：fatal failure 结束 loading；私有事件 + response error 只产生一次用户可见错误；cancel 仍显示取消而非失败。

**Verify**：`cargo test -p peri-tui --lib` 与受影响 ACP client 测试

### Commit 7 — 同步稳定契约与代码索引

- 若实现确认“turn fatal failure 由 prompt JSON-RPC error 承载，私有事件仅兼容”是新的跨模块稳定不变量，在 architecture contract 的事件边界规则中补充该约束及验证命令。
- 更新 `peri-agent`、`peri-acp` code index 中 execution result / prompt response 的入口和关键逻辑。
- 只更新受影响的单一事实源，不把 issue narrative 复制进根路由或模块说明。

**Verify**：`git diff --check`，并按 `DOC-UPDATE-001` 人工核对事实源

## Testing Decisions

### 测试原则

- 测外部协议行为和层边界契约，不断言私有局部变量或具体函数拆分。
- 事件/消息映射和 JSON-RPC 编解码属于 P0/P1；每个测试只覆盖一个错误场景。
- 错误测试同时验证分类、非空 public message、脱敏和终止状态，不能只断言 `is_err()`。
- mpsc 与 stdio 的业务结果应由共享 host 契约测试证明一致，避免复制两套实现测试。

### 必测矩阵

| 场景 | response / update | 私有事件 | loading / 终态 |
| --- | --- | --- | --- |
| 主 turn 正常完成 | `PromptResponse(EndTurn)` | 无 failure | 正常结束 |
| 用户 cancel / `Interrupted` | `PromptResponse(Cancelled)` | 无 fatal failure | cancelled |
| 最大轮数 | `PromptResponse(MaxTurnRequests)` | 无 fatal failure | 非请求错误 |
| LLM / middleware / serialization fatal | JSON-RPC error，稳定 code + 脱敏非空 message | capability 开启时兼容发送 | failed，退出 loading |
| capability 未协商的 fatal | 同上 | 不发送 | 仍可完整感知失败 |
| tool success | `completed` + standard output + rawOutput | 不适用 | 不变 |
| tool failure | `failed` + 非空 standard output + rawOutput | 不适用 | tool failed |
| replay tool failure | 与 live 同形态 + replay meta | 不适用 | 恢复后仍显示失败 |
| 内部错误含模拟 secret/cause | wire 不包含原始文本 | 日志策略单独审查 | 安全失败 |

### 完成前命令

```bash
cargo test -p peri-agent --lib executor_helpers
cargo test -p peri-acp --lib mapper
cargo test -p peri-acp --lib
cargo test -p peri-tui --lib
cargo check -p peri-agent
cargo check -p peri-acp
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

如修改 public API doc example，再运行对应 crate 的 doc tests。

## Acceptance Criteria

1. fatal `session/prompt` 在 mpsc 与 stdio wire 上均返回合法 JSON-RPC error response，并保留 RequestId。
2. fatal response 的 `message` 非空、可展示、已脱敏；`data` 不携带内部错误细节。
3. cancel 与 max-turn-requests 仍是成功 `PromptResponse`，不会被错误升级为 JSON-RPC failure。
4. 标准 capability 未协商时，客户端仍可仅凭 prompt response 判断 turn failure。
5. tool failure 的标准 update 同时包含 `status="failed"` 与可展示 `output`；`rawOutput` 保持兼容。
6. replay 后 tool failure 与 live 更新保持同一标准字段语义。
7. 内置 TUI 在 fatal failure 后退出 loading，且双通道不会重复展示同一错误。
8. RCS 联调能识别标准 `failed` 和 prompt JSON-RPC error，并结束 turn loading。
9. 目标测试、workspace clippy 和 `git diff --check` 通过。

## Risks and Mitigations

- **双通道重复提示**：compatibility event 与 request error 可能各显示一次。通过 request/turn 关联去重，并明确标准 response 为 canonical。
- **错误分类丢失或过度公开**：只跨层传窄 failure DTO，不透传 `AgentError` debug chain；首版 `data=None`。
- **失败后历史丢失**：JSON-RPC error 必须在现有失败历史保存与 state 清理之后返回，增加“失败但已有进度”的回归测试。
- **误把 cancel 当 fatal**：显式枚举 Completed / Interrupted / MaxIterations / fatal，禁止使用 `!ok` 一刀切映射。
- **live/replay 漂移**：用共享纯映射或成对契约测试固定 output、rawOutput、status 形态。
- **跨仓库上线不同步**：Peri sender 与 RCS receiver 可独立合并，但发布 gate 必须要求双方联调；在 RCS 未发布前保留私有兼容事件。

## Follow-up: RCS Receiver Alignment

在 RCS 仓库建立关联 issue，并以以下黑盒用例联调：

1. 输入标准 tool update `{ status: "failed", output: [...] }`，产出一次 `tool_call_failed`。
2. 输入 prompt JSON-RPC `{ error: { code, message } }`，产出一次 turn failure 并结束 loading。
3. 同一 turn 同时收到私有 failure event 与标准 response error，只展示一次。
4. message 缺失时客户端有安全 fallback，但记录 sender contract violation；正常 Peri sender 不应触发该 fallback。
