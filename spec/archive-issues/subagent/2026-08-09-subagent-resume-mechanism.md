> 归档于 2026-08-11，原路径 spec/issues/2026-08-09-subagent-resume-mechanism.md

# SubAgent 恢复机制：主 agent 凭 thread_id 重新唤起被中断的 subagent

**状态**：Fixed
**标签**：`ready-for-agent`、`subagent`、`resume`
**优先级**：高
**类型**：功能
**创建日期**：2026-08-09

## Problem Statement

subagent 在执行中可能因 LLM 网络中断或用户手动 interrupt 而中断（`run_react_loop` 返回 `LoopResult::Interrupted` / `LoopResult::Error`）。当前行为：

- 同步路径中断仅返回 `"Sub-agent execution was interrupted"` 纯文本，错误路径返回 Err（两者都不携带 `child_thread_id`）——主 agent 无法找回该 subagent 的执行现场；
- 后台任务路径 `BackgroundTaskResult.child_thread_id` 恒为 `None`，bg 完成/失败通知文本也不含 thread_id——后台 subagent 中断后同样无法找回；
- 中断时 thread 已落盘（status = cancelled/error，transcript 完整保留在 `ThreadStore`，可 `load_messages` / `load_meta` 加载），但没有任何机制利用这份现场继续执行。

结果：网络抖动或用户 interrupt 导致 subagent 已完成的工具副作用（写文件等）与未完成目标一并丢失，主 agent 只能重新创建新 subagent 从头执行，重复工作且可能遗漏副作用上下文。

## Solution

在现有 Agent 工具（`SubAgentTool`）上新增 `resume_thread_id` 参数：主 agent 凭工具返回值携带的 `child_thread_id` 重新唤起被中断的 subagent，从磁盘 thread_store 加载其 transcript 重建现场，重放历史 + 追加指令后继续 `run_react_loop`。恢复是"新执行单元"：触发新的 SubagentStart/Stop、status 正常收尾、thread_id 不变。

### 1. 工具层（`peri-middlewares/src/subagent/tool/`）

**define.rs — Agent 工具新增 `resume_thread_id` 参数（可选）**

- 提供时走恢复路径（不创建新 thread）；与 `fork: true` / `subagent_type` 同时提供时报参数互斥错误；
- `run_in_background` 仍可组合：恢复时的 run mode 由本次调用决定（恢复后台中断任务为同步执行是合法的）；
- 校验失败一律走 Err（与现有工具错误处理一致）：
  - thread 不存在 → `Err`；
  - `thread.status == running`（未中断，仍在执行）→ `Err`；
  - `thread.parent_thread_id != 主 agent thread_id`（所有权校验）→ `Err`；
- `resume_thread_id` 校验通过后，其余装配（LLM / tools / chain）与 spawn 路径一致。

**中断/错误暴露（返回值携带 thread_id）**

- 同步路径 interrupted：`Ok("child_thread_id: {id} (sub-agent was interrupted, resume with Agent(resume_thread_id: {id}))")`；
- 同步路径 Err（LLM 网络错误等）：Err 文本前缀带 `child_thread_id: {id}`（网络中断是核心恢复场景，错误路径必须可恢复）；
- 后台路径：`spawn_background_subagent` 时将 `child_thread_id` 填入 `BackgroundTaskResult`（现恒 None），`to_notification()` 文本带上 thread_id——主 agent 从 bg 完成/失败通知中得知可恢复。

**descriptions/agent.md**

- 新增 `resume_thread_id` 参数说明：恢复条件（状态非 running）、prompt 可选语义、与 fork/subagent_type 互斥；
- "When to use" 补恢复指引：subagent 中断后凭返回值中的 thread_id 恢复。

### 2. Agent 层统一恢复入口（`peri-agent/src/session/subagent.rs`）

新增 `SessionFactory::resume_subagent(parent, config)`，与 `spawn_subagent` 并列，流程：

1. `thread_store.load_meta(thread_id)`：存在性 + status 非 running + parent_thread_id 链校验（不通过返回明确 Err）；
2. `thread_store.load_messages(thread_id)` 加载 transcript；
3. 重建 Session：thread_id 不变、frozen 从父 session copy（父存在时，与 spawn 一致）、transcript 注入加载的消息并绑定持久化（`with_persistence`）；
4. 截掉最后一条含未完成 tool_calls 的 AI 消息（缺 tool_result 会导致 LLM API 400；复用 define.rs 既有处理模式）；
5. queue 注入指令：prompt 未提供时注入隐式 continue（如 `"Continue your previous task where you left off"`），提供则追加为新指令；
6. 工具集：`thread.title == "fork"` 用父工具集 clone；否则从 title 取 agent 类型重新 `load_agent_def` 应用 tools/disallowed 过滤（防止恢复后权限漂移）；
7. `update_thread_status(thread_id, running)` 置回运行态；
8. 按 run mode 执行：Sync 直接 `run_react_loop`；Background 重新 spawn 任务（TaskManager 注册、新 task_id）；
9. 收尾：新的 SubagentStart/Stop v2 事件（agent_id = child_thread_id，事件配对正常）、`update_thread_status(done/cancelled/error)`、lifecycle hook、`extract_last_ai_text` 结果返回。

- `max_iterations`：agent-def 路径从定义重新读取，fork 路径默认 200（与 spawn 一致）；
- 多次恢复：可无限次，thread_id 不变，transcript 持续追加（每次恢复都是第 1-9 步的重入）。

### 3. 可观测性

- 恢复触发新的 SubagentStart/Stop，同一 child_thread_id 多次 Start/Stop 在 Langfuse/TUI 自然呈现（同 agent 多次 observation），不新增事件类型、不加 resume 标记字段。

## 验收标准

- [ ] `SessionFactory::resume_subagent(parent, config)` 为唯一恢复入口，位于 peri-agent；`load_meta` 校验（存在 / 非 running / parent 链）不通过返回明确 Err。
- [ ] Agent 工具 `resume_thread_id` 参数：与 fork/subagent_type 互斥校验；恢复后 run mode 由本次调用决定（可组合 run_in_background）。
- [ ] 恢复重建正确性：transcript 完整重放、末条未完成 tool_calls 被截断、隐式 continue 或新 prompt 注入、thread_id 不变、status 置回 running 且收尾正常（done/cancelled/error）。
- [ ] 工具集恢复：title=="fork" 用父工具集；否则从 title 重新 load_agent_def 应用过滤。
- [ ] 中断/错误暴露：同步 interrupted Ok 文本、同步 Err 文本、bg 完成/失败通知文本均携带 `child_thread_id`。
- [ ] 恢复触发新的 SubagentStart/Stop（agent_id = child_thread_id），事件配对与 TUI 容器正常。
- [ ] 多次恢复可用（中断→恢复→再中断→再恢复，thread_id 不变）。
- [ ] 测试三层覆盖：单元（重建正确性/校验分支）、集成（FakeLLM 注入网络错误 → 中断 → 恢复 → 完成；取消 token 中断；多次恢复）、1 条 E2E（主 agent 中断 → 恢复链路）。
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 通过；既有 subagent 测试保持绿。

## 非目标

- 不新增独立工具 / 事件类型（Resumed 事件、resume 标记字段不做）。
- 不限制恢复次数、不新增 thread meta 字段（status/title/parent_thread_id 已有字段够用；原 prompt、max_iterations 不落盘，恢复时由本次调用或 agent 定义提供）。
- 不自动恢复：恢复由主 agent 显式调用，LLM 层重试语义不变。
- 不改变 spawn 既有行为（恢复是新增路径，不动既有四路径语义）。

## 关联 Issue

- `spec/issues/2026-08-05-3.0-l3-subagent-factory-to-agent.md`（spawn_subagent 统一入口，本 issue 在其之上新增 resume 路径）
- `spec/issues/2026-08-05-bg-cancel-abort-skips-cleanup.md`（后台任务取消语义，恢复的前置现场）

## 涉及文件

- `peri-agent/src/session/subagent.rs` — 新增 `resume_subagent` 统一入口（主改动）
- `peri-middlewares/src/subagent/tool/define.rs` — `resume_thread_id` 参数、互斥校验、中断/错误文本带 thread_id
- `peri-middlewares/src/subagent/tool/execute_bg.rs` — `BackgroundTaskResult.child_thread_id` 填充
- `peri-acp-types/src/event.rs` — `BackgroundTaskResult::to_notification()` 文本带 thread_id
- `peri-middlewares/src/subagent/tool/descriptions/agent.md` — resume 用法与恢复指引
- 测试：`peri-agent/src/session/subagent_test.rs`、`peri-middlewares/src/subagent/tool/tool_test.rs`、`e2e/`

## 症状详情（2026-08-10 使用实录）

功能已实现并可用，但**恢复失败的错误文案不可读**。实录：并行派三个研究 subagent，一个中断后尝试恢复，两次失败：

1. **互斥错误无用法指引**：`resume_thread_id` 与 `subagent_type` 同时传 → 报 `Error: resume_thread_id is mutually exclusive with fork / subagent_type`（define.rs 互斥校验）。错误只说"互斥"，没说怎么传才对（恢复时应只传 `resume_thread_id`，不传 `subagent_type` / `fork`）。
2. **parent mismatch 无解释**：只传 `resume_thread_id` → 报 `resume_subagent: parent thread mismatch for <id>`（resume_subagent_impl parent 链校验）。用户不知道 parent thread 是什么、为什么并行派发的 subagent 报此错。
3. **文案观感像安全错误**：错误文本形似密码学签名，用户无法判断是「权限问题」还是「用法问题」，无从排查。
4. **后果**：两次失败后用户放弃恢复，重新派全新 agent 从头讲任务，token 和时间双倍消耗。

## 修复要求（2026-08-10 补充，范围：仅 resume 相关错误）

- 涉及文件：`peri-middlewares/src/subagent/tool/define.rs`（互斥错误文案）、`peri-agent/src/session/subagent.rs`（parent mismatch 校验文案，含 active 校验文案一并评估）、`peri-middlewares/src/subagent/tool/descriptions/agent.md`（resume 说明补「常见失败原因」）。
- 错误文案改为「人话版原因 + 正确用法示例」，保留 thread_id 等关键信息：
  - 互斥错误 → 给出正确用法（`Agent(resume_thread_id: <id>)`，勿与 subagent_type / fork 同传）；
  - parent mismatch → 说明「该 thread 属于其他父 agent（并行兄弟上下文），本会话无权恢复」；
  - active → 说明「thread 仍为运行态，确认无执行中任务后再恢复」。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-09 | — | Ready for agent | agent | 经访谈收敛 16 项设计决策后成文（入口形态/持久化边界/续跑语义/状态判定/生命周期/所有权/后台 run mode/隐式 continue/多次恢复/工具集/错误语义/ID 携带/测试/归因/描述文档） |
| 2026-08-10 | Ready for agent | Ready for agent | 用户 | 补充使用反馈：恢复失败错误文案不可读（互斥无用法指引、parent mismatch 无解释、观感像安全错误）；新增「症状详情」「修复要求」，修复范围=仅 resume 相关错误文案 |
| 2026-08-10 | Ready for agent | Fixed | agent | 修复：resume 错误文案改为人话版+用法示例（define.rs 互斥、subagent.rs parent mismatch/active、agent.md 补常见失败原因、测试断言同步） |
| 2026-08-10 | Fixed | Fixed | 用户 | 设计变更：互斥报错 → 容错模式。根因：subagent_type 参数声明 "REQUIRED" 与 resume 互斥契约矛盾，LLM 按 schema 惯性同传两字段，即使错误文案给了用法仍连续失败两次；决定 resume_thread_id 存在时静默忽略 subagent_type / fork，直接恢复 |

## 修复记录

### 修复 #1（2026-08-10）

- **操作人**：agent
- **用户原意**：resume 恢复失败的错误文案不可读（互斥无用法指引、parent mismatch 无解释、观感像安全错误）；失败时直接给「恢复用法示例」和「失败原因人话版」。
- **修复内容**：
  - `peri-middlewares/src/subagent/tool/define.rs`：互斥错误补正确用法示例（恢复只传 `resume_thread_id`，勿带 subagent_type / fork），保留 `mutually exclusive` 关键词；
  - `peri-agent/src/session/subagent.rs`：parent mismatch 补中文人话解释（该 thread 属于其他父 agent，仅原父 agent 可恢复，或改传 subagent_type 新建）；active 错误补「仍在执行或异常未收尾」解释与新建替代用法；
  - `peri-middlewares/src/subagent/tool/descriptions/agent.md`：Resume execution 段补「Common failures」三条（互斥 / parent mismatch / thread not found）；
  - `peri-agent/src/session/subagent_test.rs`：两处精确错误文案断言同步更新。
- **涉及 commit**：a03a2a66（错误文案修复）
- **验证状态**：已验证（测试断言同步通过）
- **后续设计变更（2026-08-10，容错模式）**：互斥报错移除——resume_thread_id 存在时 subagent_type / fork 被静默忽略，直接恢复。原因：LLM 按 schema "REQUIRED" 声明惯性同传两字段，报错 + 文案指引仍连续失败两次（信息源冲突时 LLM 倾向遵循 schema）；容错使恢复恒可成功。涉及 `define.rs`（删互斥校验、参数 description 改容错语义）、`agent.md`（Resume 段与 Common failures 更新）、`tool_test.rs`（两个互斥测试改为容错测试）。
