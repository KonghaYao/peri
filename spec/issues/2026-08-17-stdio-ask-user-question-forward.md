# stdio 提问转发：AskUserQuestion（elicitation/create）经 ACP transport 转发

**状态**：Closed（已实施并验证）
**优先级**：中
**类型**：功能增强（stdio 宿主）
**创建日期**：2026-08-17
**来源**：FenixAgent 平台需求（`pazhou/rcs-yjs-squash/docs/design/2026-08-17-ask-user-question-peri-requirement.md`，状态：待 peri 侧评估）

## 背景

FenixAgent 平台经 acp-link 以 stdio 子进程方式运行 peri。`AskUserQuestion` 工具（`peri-middlewares/src/tools/ask_user_tool.rs`）当前只有 mpsc/TUI 路径经 `AcpTransportBroker` 发出标准 ACP `elicitation/create`；stdio 生产路径下提问被 `StdioBroker` 内部吞掉（返回空答案），客户端收不到任何事件。

需求要点：stdio 会话的 `Questions` 分支经 ACP transport 的 server→client request 通道发送 `elicitation/create` 并等待同 id 响应（accept / cancel / decline）；`Approval` 分支自动 approve 语义不得回归；无新增私有协议帧；挂起提问在 transport 关闭时安全中断；客户端不响应有可配置兜底。

## 评估结论（可行性已核实）

1. **发送通道可用**：`agent-client-protocol 2.0.0` 的 `ConnectionTo<Client>::send_request(UntypedMessage::new(method, params))` → `SentRequest<Value>`（`jsonrpc.rs:4491/4558`）。`UntypedMessage` 的 `Response = serde_json::Value`，与 mpsc 路径 `AcpTransport::send_request("elicitation/create", params)` 完全等价；`SentRequest::block_task()` 明确支持 dispatch loop 之外的任务使用（`jsonrpc.rs:5172`）。
2. **event loop 不阻塞**：`session/prompt.rs:100-122` 已将 prompt 执行 `tokio::spawn` 到后台任务（`prompt_exec::run`），`cx: ConnectionTo<Client>` 已 clone 进任务；broker 的 `request` 在后台任务内 await 响应，event loop 保持对 `session/cancel` 的响应性。
3. **transport 关闭天然安全**：incoming EOF 时库会先失败所有挂起请求（"Pending requests are failed first"）→ `send_request` 立即返回错误，不会永久挂起。
4. **响应语义已存在**：`ask_user_tool.rs:132-150` — `Answers` → `[问: {header}]\n回答: {val}`；`Rejected` → `AgentError::ToolRejected`；空 `Answers` → 空答案。验收 1-3 的 LLM 侧行为零改动。
5. **复用面**：`broker/transport_broker.rs:105-186` 的 `handle_questions` 已有完整请求构造（单选/多选/自由文本 + `inject_option_descriptions` 选项描述注入）与响应解析（accept/cancel/decline/解析失败兜底）。
6. **现有装配**：`prompt_exec.rs:63-64` 无条件 `Arc::new(StdioBroker::new())`；`StdioBroker` 无其他引用，可安全替换。
7. **HITL 超时参照**：`peri-agent/src/interaction/channel_broker.rs:99-100` — `tokio::time::timeout(Duration::from_secs(300), rx)` 后 Reject。

## 用户拍板决策（2026-08-17）

1. **超时兜底**：客户端不响应超时 → 返回 `InteractionResponse::Rejected`（LLM 收到 `ToolRejected`，与 decline 语义一致）。
2. **默认超时**：300s（与 HITL 审批 `channel_broker.rs:100` 对齐）。
3. **配置载体**：环境变量（缺省 300，`0` = 不超时）。命名 `PERI_ASK_USER_TIMEOUT_SECS`（秒，正整数；非数字/非法值回落默认 300；`0` 表示不超时）。惯例对齐 `peri-acp-types/src/compact.rs:325-333` 的 `COMPACT_THRESHOLD` 模式，不进 settings.json。

## 设计

### 新 broker：`StdioQuestionBroker`（`peri-acp/src/host/stdio/context.rs`）

```rust
pub(super) struct StdioQuestionBroker {
    cx: ConnectionTo<Client>,   // server→client request 通道
    session_id: SessionId,      // elicitation scope
    timeout: Option<Duration>,  // None = 不超时（env 0）
}
```

`UserInteractionBroker::request`：

- `Approval { items }` → **保持现状**：逐项 `ApprovalDecision::Approve { source: None }`（自动 approve，不得回归）。
- `Questions { requests }` → 转发：
  1. 复用提取的 `build_elicitation_params(requests) -> serde_json::Value`（含 option description 注入，逻辑自 `transport_broker.rs:106-147` 平移）；
  2. `cx.send_request(UntypedMessage::new("elicitation/create", params))` → `.block_task()`；
  3. 套 `timeout`（`tokio::time::timeout`，None 时直接 await）；
  4. 结果分派（与 mpsc 路径语义一致）：
     - 客户端响应 accept → `parse_elicitation_response` → `Answers`；
     - `decline` → `Rejected`；
     - `cancel` / 响应解析失败 → 空 `Answers`；
     - **超时（`Elapsed`）→ `Rejected`**（用户拍板：报 ToolRejected）；
     - **transport 错误（断连）→ 空 `Answers`**（需求 4.4：安全中断、会话可继续；与 mpsc 路径 transport error 兜底一致）。

### 复用提取（`peri-acp/src/broker/transport_broker.rs`）

将 `handle_questions` 的请求构造与响应解析拆为 pub(crate) 纯函数，`AcpTransportBroker` 与新 broker 共用（行为零变化）：

- `build_elicitation_params(requests: &[QuestionItem]) -> serde_json::Value`（含 `inject_option_descriptions`）
- `parse_elicitation_response(response: Value, requests: Vec<QuestionItem>) -> InteractionResponse`（accept/cancel/decline/解析失败）

### 装配替换（`peri-acp/src/host/stdio/session/prompt_exec.rs:63-64`）

```rust
let broker: Arc<dyn UserInteractionBroker> = Arc::new(
    StdioQuestionBroker::new(cx.clone(), session_id.clone(), ask_user_timeout()),
);
```

- `cx` / `session_id` 已在 `PromptExecParams` 中可用；`StdioBroker` 删除（无其他引用）。
- `ask_user_timeout()`：读 `PERI_ASK_USER_TIMEOUT_SECS`，非法值回落 300，`0` → None（不超时）。放 `context.rs` 的 broker 构造处（`cfg` 不新增字段，环境变量直接读，符合拍板 3）。

## 实施步骤

| # | 文件 | 变更 | 验证 |
|---|------|------|------|
| 1 | `peri-acp/src/broker/transport_broker.rs` | 提取 `build_elicitation_params` / `parse_elicitation_response` 纯函数，`handle_questions` 改为调用 | 既有 broker 测试全绿（行为零变化） |
| 2 | `peri-acp/src/host/stdio/context.rs` | 新增 `StdioQuestionBroker`（含 env 超时读取）；删除 `StdioBroker` | 编译 + 新增单测 |
| 3 | `peri-acp/src/host/stdio/session/prompt_exec.rs:63-64` | 装配替换 | 编译 |
| 4 | 测试（见下） | 单测 + 双端集成测试 | 验收 1-6 |
| 5 | 本 spec 关闭 + 行为文档 | 注明 `session/cancel` 不解除挂起提问的限制（需求 3.3） | 文档审阅 |

## 测试计划（映射需求验收）

双端驱动模式对齐 `session/create_test.rs` / `commands_test.rs`（`Agent::builder()` + `Channel::duplex()`，client 端 `on_receive_request` 捕获 `elicitation/create`）：

1. **验收 1**：stdio broker `Questions` → client 收到 `elicitation/create`；断言 `requestedSchema` 含单选（oneOf）、多选（array/items）、自由文本（无 options）三形态 + title/description + option description 注入。
2. **验收 2**：client `accept`（`content: {q_id: label}`）→ broker 返回 `Answers`；沿用 `ask_user_tool` 既有测试断言 `[问: header]\n回答: label` 格式。
3. **验收 3**：`decline` → `Rejected`；`cancel` → 空 `Answers`。
4. **验收 4**：transport 关闭（drop client 端 / incoming EOF）→ 挂起提问返回空 `Answers` 而非挂死；后续 prompt 可继续。
5. **验收 5**：`Approval` 分支自动 approve 回归测试（新 broker 单测）。
6. **验收 6**：mpsc 路径既有 `elicitation/create` 行为测试（transport_broker 相关）全绿。

另加：超时单测（`PERI_ASK_USER_TIMEOUT_SECS=1` + client 不响应 → `Rejected`；`0` → 不超时路径不受 env 影响）。

## 边界（维持现状，需求 3.4）

- subagent 不继承 `AskUserQuestion` —— 不动。
- workflow 路径 broker 为 None —— 不动（`prompt_exec.rs:114` 的 workflow executor 装配保持 `broker: None`）。
- 其它 stdio 客户端不响应 `elicitation/create` → 由超时兜底（默认 300s → `ToolRejected`）保障不挂死。

## 非目标

- 不改 `AskUserTool` 工具本体、`peri-acp-types/src/interaction.rs` 契约、`AcpTransportBroker` 行为。
- 不新增任何私有协议帧、不扩展事件链（ARC-EVENT-001 无涉及——本变更走既有 RPC 交互，非 v2 事件）。
- 不引入 settings.json 配置项（拍板 3）。

## 风险

- `ConnectionTo<Client>` 的 `Send + Sync`：`prompt_exec` 已将其传入后台任务（Send 已证）；`mpsc::UnboundedSender` 等字段满足 Sync，broker 作为 `Arc<dyn UserInteractionBroker>` 共享无碍。编译期验证。
- 超时后客户端迟到的响应：`SentRequest` drop 时自动发 cancel（库行为），无需额外清理。

## 实施记录（2026-08-17）

### 验证命令与结果

| 命令 | 结果 |
|------|------|
| `cargo clippy -p peri-acp --all-targets -- -D warnings`（timeout 600000） | 通过，0 warning / 0 error |
| `cargo test -p peri-acp --lib -- host::stdio::context::tests broker::transport_broker::tests` | 20 passed / 0 failed（`context_test` 10 项 + `transport_broker_test` 10 项） |
| `cargo test -p peri-agent --lib -- ask_user` | 2 passed / 0 failed（`test_micro_compact_ask_user_question_preserved_by_default`、`test_no_trigger_after_ask_user_question`） |
| `cargo test -p peri-middlewares --lib -- ask_user_tool` | 14 passed / 0 failed（含 `Rejected → AgentError::ToolRejected` 既有行为，未回归） |

测试覆盖映射（对照「测试计划」）：验收 1 → `test_questions_schema_three_forms_and_option_descriptions`；验收 2 → `test_accept_returns_answers`；验收 3 → `test_decline_returns_rejected` / `test_cancel_returns_empty_answers`；验收 4 → `test_transport_closed_returns_empty_answers`；验收 5 → `test_approval_branch_auto_approve`；验收 6 → `transport_broker_test` 10 项；另加超时与 env 解析单测（`test_timeout_returns_rejected`、`test_no_timeout_waits_for_slow_client`、`test_parse_ask_user_timeout_env_values`、`test_ask_user_timeout_reads_env`）。测试驱动方式与计划一致：双端 builder（`Agent::builder()` + `Channel::duplex()`），broker 任务 `tokio::spawn` 于 dispatch loop 之外，client 端 `on_receive_request` 捕获 `elicitation/create`。

### 实际实现与计划的偏差

1. **`build_elicitation_params` 签名比计划多一个 `session_id: SessionId` 参数**（`transport_broker.rs:124-127`）：计划中 session_id 由 broker 持有、仅作为 elicitation scope 概念字段，实现改为直接传入函数参与 schema 构造（scope 落在 `elicitation/create` params 内）。功能等价，`AcpTransportBroker` 调用点同步更新。
2. **其余无偏差**：`StdioQuestionBroker` 结构与分派语义（Approval 自动 approve；accept → `Answers`；decline → `Rejected`；cancel / 解析失败 → 空 `Answers`；超时 → `Rejected`；transport 错误 → 空 `Answers`）、`parse_ask_user_timeout`（缺失/非法 → 300s，`0` → None）、`prompt_exec.rs:63-66` 装配替换、`StdioBroker` 删除均与设计一致。

### 遗留事项

- 变更尚未 commit（未跟踪文件：`transport_broker_test.rs`、`context_test.rs` 与本 spec）。

### 补充记录（2026-08-17 收尾）

- **行为文档已补**：`docs/design/peri-acp-protocol.md` 新增 §6.3「stdio 提问转发行为」，注明 `session/cancel` 不解除挂起提问、超时兜底（`PERI_ASK_USER_TIMEOUT_SECS`，超时 → `ToolRejected`）、断连兜底（transport 关闭 → 空答案）三组行为。
- **TUI 路径关系确认**：TUI 不是另一套协议——TUI 是 ACP client 端（`AcpTuiClient::send_response` 回 accept/cancel/reject 弹窗），服务端 broker 与 stdio 分属两路：TUI/notify 走 `AcpTransportBroker`（mpsc transport，`host/prompt.rs:146`），stdio 走 `StdioQuestionBroker`（`ConnectionTo` 直发），两路共用 `build_elicitation_params` / `parse_elicitation_response` 纯函数，协议帧一致。
- **提交**：本次变更（3 实现文件 + 2 测试文件 + 本 spec + 行为文档）已提交。
