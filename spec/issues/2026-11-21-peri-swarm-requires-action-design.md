# peri-swarm `requires_action` 映射提案

状态：已批准并实现（修订 4：2026-11-22 第二轮独立复验返修，修复 `peri-acp` 测试
目标编译、生产 Rivet SSE 路径的过期结算与订阅快照、结算持久化失败的显式 5xx、
502 故障注入、子 agent 无 broker 装配测试，以及空来源/空工具名投影与未使用结算
原因；按本机 `openai@7.60.0` `agents.d.ts` 校正 `required_actions`/答复 body 形状，
并补齐来源缺失 fail-closed、子 agent 范围继承、硬崩溃恢复与送达一致性）；观察基线
`22e44e0b9`，实现基线 `deca51f44`。
实现与逐条验收见 `docs/verification/peri-swarm-requires-action-2026-11-21.md`（保留本文的
证据边界与历史判断，不改写）。
本单只回答：官方 `requires_action` 能诚实映射到什么、需要改哪些边界、哪些必须另行批准。
修订 1 依据一次独立审阅：实时审批与历史回放必须拆开，回答回路未闭合前不实施。
修订 2 依据文档复验：官方 SDK 没有 `require_approval`、AskUserQuestion 的 `Decline`
不是空答案、`sessions.list` 没有 live 待办。

## 结论

- 本地唯一可诚实映射的是 **MCP 工具审批** → 官方 `mcp_approval_request`。
- 但官方 `openai@7.60.0` 的 `AgentToolConfigParamMcp` 没有 `require_approval`；
  该字段只有网关扩展接受。因此本映射对纯官方 SDK 客户端**不可达**，
  除非另定默认审批策略或由扩展请求开启。以官方兼容为目标时，它的优先级应下调。
- `AskUserQuestion` 走 ACP `elicitation/create`，官方四类 `required_actions` 没有 elicitation；
  它是“应用执行函数并提交结果”之外的交互，不在本映射范围，保持本地扩展。
- `computer_use_approval_request`、`function_call`、`environment_connection` 没有本地事实，不映射。
- 当前 `peri-swarm` 刻意关闭审批（`PermissionMode::Bypass`），直接投影会造假。
- **实时审批**可以不改 schema 实现；**历史回放 `requires_action`** 需要另立持久事件契约，
  本轮不承诺。答复回路（审批输入回到原 ACP 等待者）未证明前，不进入实施。

## 证据

官方（本地 `openai@7.60.0`）：

- `AgentSession.status` 含 `requires_action`；`required_actions` 是四类联合
  （`resources/beta/agents/agents.d.ts:811-843`）。
- 事件 `agent.session.requires_action`（同文件 `2027-2044`）。
- 答复输入 `agent.session.input.mcp_approval_request_result`，decision 为
  `approve` / `deny` / `cancel`（同文件 `1605-1634`）。
- `AgentTurnResultCollector` 从任何携带 `session` 的事件读取 `required_actions`；
  有无法处理的待办时 `checkAction` 抛 `requires_action`，terminal 仍只认
  `completed` / `failed` / `cancelled`（`lib/beta/agents/agent-turn-result-collector.mjs:28-45,84-104`）。
  因此待办解决后，后续 session 快照必须清空 `required_actions`，否则客户端仍视为挂起。
- `require_approval` 在 `agents.d.ts` 全文出现 0 次；`AgentToolConfigParamMcp`
  （同文件 `2919-2961`）没有该字段。

Peri 本地：

- 审批反向请求 `session/request_permission` 由
  `peri-acp/src/broker/transport_broker.rs:177-184` 发出；
  `peri-middlewares/src/permission/permission_middleware.rs:650-664` 是工具批处理 hook。
- `AskUserQuestion` 走 `elicitation/create`（`transport_broker.rs:214-228`）。
- `require_approval` 由网关扩展请求接受（`peri-swarm/src/http/request.rs:80,386-390`），
  不是官方 SDK 入参。
- `peri-swarm` 强制 `PermissionMode::Bypass`（`peri-swarm/src/actor/runtime.rs:363`）。
- `read_loop` 对 `session/request_permission` 回 `cancelled`，其他反向请求回 `-32601`
  （`runtime.rs:735-751`）。
- `agent.tools[].require_approval` 只进 MCP `_meta`，host 不消费
  （`runtime.rs:52-57,208-225`）。

## 必须一并修的既有缺陷

`elicitation/create` 被网关拒绝（`-32601`）或传输失败后，`AskUserQuestion` 返回空答案
（`transport_broker.rs:214-228`），把“网关不支持/无人可答”伪装成用户回答空内容。
用户明确 `Decline` 走 `Rejected`（`transport_broker.rs:295-298`），不是空答案；
修复要区分“网关不支持、无人可答、用户取消、真正提交空答案”，不要顺手改写取消契约。

## 实施边界（按批准粒度拆分）

### A. 实时审批：不改数据库结构即可做

1. **触发来源与范围**：官方 SDK 没有 `require_approval`，触发只能来自网关扩展请求
   或一个新的默认策略；必须先决定用哪一个。只对能匹配到具体 MCP 绑定的声明开启审批；
   不把全局 `Bypass` 改成 `Default`，避免把所有外部 MCP 调用一起阻塞。
   匹配规则必须基于绑定来源，不能只按工具名（同名工具会误判）；
   匹配失败时 fail-open 还是 fail-closed 必须显式选择，不能默认放行。
2. **回答回路**：审批输入必须回到仍然挂起的原 `session/prompt` 所在 actor，
   用**原连接、原 ACP JSON-RPC `id`** 调 `send_response`；官方 `request_id` 是不透明公开 ID，
   答复不能走“创建/排队新 turn”的路径。
3. **单次结算**：答复要校验 session、turn、执行代次与待办身份；
   两个并发答复、重复答复、旧代次答复只能有一个成功，其余明确 4xx。
4. **状态投影**：持久态可以继续是 `Running`，只有当前有效待办才派生
   `requires_action`；retrieve 与 SSE 用同一份有效性判断。
   `sessions.list` 当前只读 store，没有 live slot：要么接受列表状态滞后，
   要么扩展 list 读取 live 待办，这是需要决定的行为面。
   turn 不提前终止；`deny` / 审批 `cancel` 只结算这一次工具调用，
   只有 turn 实际结束后才发 completed / failed。
5. **SSE 顺序**：工具调用相关的官方 item（若已有）→ `agent.session.requires_action`
   （session 快照带当前 `required_actions`）→ 答复后工具结果 → `turn.completed` → `session.idle`。
   不为了凑顺序伪造 item，也不等工具执行完才发审批。
6. **失效语义**：actor 丢失、传输断开、审批等待到期时，待办失效并明确收尾；
   旧 `request_id` 不可答复，不能把“无人应答”伪装成用户拒绝，也不能自动重执行工具。
7. **超时**：复用现有审批等待期限，不从 `read_loop` 收到请求时另起独立期限。

### B. 需要明确批准的部分

- **历史回放**：官方 `requires_action` 表达“当前待办”，不是历史事实。
  现有回放按 item `seq` 只重建消息帧；要让挂起期重连也能发现待办，
  需要持久化待办/答复事件与游标契约，或明确“重连后读当前会话状态”。
  后者不需要 schema，但必须放弃“历史同帧回放”的承诺。二选一需要批准。
- **安全语义与默认策略**：打开审批会阻塞工具调用；需要确认默认是否要求审批、
  哪些部署启用、无客户端在线时的失败方式，以及审批员权限范围。
- **store 结构**：若选择持久化待办与答复回执（跨进程恢复），需要新增列/表或迁移，
  按项目规则必须单独批准。
- **环境快照**：所有 `OfficialSession` 快照都没有 `environment` 字段
  （`peri-swarm/src/domain/official.rs:381-403`），不只是 SSE 回放；
  `requires_action` 的 session 快照会继承这个缺口，是否同批补齐需单独决定。
- **幂等与多待办**：同一 `Idempotency-Key` 重试、一个 turn 内多个并行待办、
  答复与 turn 取消同时到达，都需要先定结算顺序与回执语义，否则会出现重复执行或悬空待办。

## 不做

- 不做历史 `requires_action` 回放（直到 A/B 的持久契约被批准）。
- 不把 `AskUserQuestion` 映射成 `function_call` 或任何官方动作。
- 不为 `computer_use`、`environment_connection` 造事实。
- 不在本轮打开 token/usage 记账或环境生命周期事件。

## 验收口径（批准后）

- 带 `require_approval` 的 MCP 工具：会话进入 `requires_action`，事件只发一次，
  turn 不终止；`approve` 后工具执行；`deny` / 审批 `cancel` 后按 host 结果收尾。
- 串行重复答复、两个并发答复、跨 session/旧代次答复：只有一次成功，其余明确 4xx。
- turn 取消与审批答复竞争：只有一次结算；取消先赢时工具不执行。
- 无客户端在线、等待中断线、broker 到期、actor 重启：待办失效；
  旧 ID 不可答复，不伪造用户拒绝或 `turn.completed`，不自动重执行。
- 同批免审批工具与不同 MCP server 的同名工具：审批范围没有被扩大。
- 同一 turn 多个并行待办：各自独立结算；答复一个不会清掉其他待办。
- 同一 `Idempotency-Key` 重试：返回同一回执，不重复执行工具、不创建新 turn。
- 等待超时与无客户端在线：待办失效并给出明确终态；不伪造成用户拒绝。
- 重连后能通过 `sessions.retrieve` 或当前 SSE 快照读到待办；
  已结算动作不会复活；答复不创建新 turn。
- 真实 SDK 探针：审批前、挂起时、答复后终态前分别断线重连，
  collector 只在原 turn 的真实终态结束。
- `AskUserQuestion` 不再返回空答案：无可用客户端时返回明确错误。
- 全量 `cargo test -p peri-swarm`、clippy、fmt、lefthook 通过。

## 复核命令

```bash
git grep -n "PermissionMode::Bypass" -- peri-swarm
git grep -n "session/request_permission\|elicitation/create" -- peri-swarm peri-acp
git grep -n "require_approval" -- peri-swarm
```
