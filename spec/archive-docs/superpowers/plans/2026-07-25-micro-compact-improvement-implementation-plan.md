# Micro Compact 改进实施计划

> **给实施者：** 按任务顺序执行；每个任务先写列出的测试，再写最小实现，并在任务末尾运行指定验证命令。不要在未完成「持久化恢复」与「协议测试」前默认启用新策略。
>
> **来源：** `docs/design/micro-compact-improvement-proposals.md`（2026-07-25）
>
> **目标：** 将 v2 Micro Compact 从“对消息打 `truncated` flag、在 Reason 阶段临时截取文本”的机制，升级为确定性、Provider 安全、可恢复、可度量的 `pressure → plan → render → apply → report` 流程。在不改变 Frozen System Prompt、不新增默认 LLM 调用、不破坏 tool-use/tool-result 配对或控制状态的前提下，回收足够的上下文 token。

## 1. 当前边界与实施决策

### 1.1 当前代码落点

| 责任 | 当前实现 | 主要问题 |
|---|---|---|
| Micro 候选选择 | `peri-agent/src/agent/compact_v2/micro.rs` | 以消息/round 为粒度；混合 tool call 会误伤受保护调用；候选不等于可投影内容。 |
| Compact 编排 | `peri-agent/src/agent/compact_v2/mod.rs`、`peri-agent/src/agent/stages/compact.rs` | 以 `affected_count` 与百分比决定效果/升级；先写 Micro flag 再运行 Full。 |
| LLM 可见视图 | `peri-agent/src/agent/stages/reason.rs` | 仅对 `Text` 调用 `BaseMessage::truncated_content(100)`；Blocks/Raw 可能无变化。 |
| 消息/内容适配 | `peri-agent/src/messages/{message,content}.rs`、`messages/adapters/{openai,anthropic}.rs` | tool input 可被变成 JSON string；媒体 payload 和恢复提示可能仍进入请求或被丢失。 |
| token 数据 | `peri-agent/src/agent/token.rs` | 已有最近 input、未感知工具输出估算、cache hit rate，但没有 headroom/reclaim 模型。 |
| Transcript 与恢复 | `peri-agent/src/session/transcript.rs`、`peri-acp/src/session/executor_helpers.rs` | flag 能恢复，但缺少 directive/policy；逐条更新和逐条 cache invalidation。 |
| 存储 | `peri-agent/src/thread/{store,sqlite_store,filesystem}.rs` | SQLite 仅持久化两个 bool；Filesystem 没有 compact state；无 batch transaction。 |
| 观测 | `peri-agent/src/agent/events_v2.rs`、`peri-acp/src/langfuse/{bridge,tracer/compact}.rs` | 只有策略、数量、可见消息数，不能衡量估算与真实收益。 |

### 1.2 不变量

以下规则是所有任务的验收前提；若实现不能满足，应拒绝应用对应 plan 并记录结构化原因，而不是降级为不安全截断。

1. **Frozen System Prompt 不变。** Compact 只能处理对话消息；Full 的摘要/控制内容继续使用现有允许的 Human/SystemReminder 路径，绝不在中途新增 `BaseMessage::system()`。
2. **Transcript 保存原始消息。** 投影只生成下一次 LLM 请求的临时 `Vec<BaseMessage>`；不得用投影后的内容覆写历史 `BaseMessage`。
3. **plan 无副作用，apply 是唯一写入点。** 规划、估算、投影视图和协议校验不得写 flags、数据库或 cache；只有成功 apply 才能修改内存状态和持久化状态。
4. **tool exchange 不可拆。** 不删除一端而保留另一端；不变更 `tool_call_id`；不把 input 根值从 JSON object 变成 string；不打乱同一 AI 消息中的并行调用关系。
5. **默认保守。** Human、错误 ToolResult、未完成/控制流相关 exchange、`AskUserQuestion`、`goal`、`TodoWrite` 以及未声明 retention 的工具默认保留。
6. **祖先只读。** `ancestor_len` 之前的消息不得成为候选、不得写 directive/flag、不得被恢复逻辑重解释。
7. **幂等且可恢复。** 同一 plan 第二次 apply 必须是 no-op；恢复后按相同 policy/directive 得到等价 LLM view；未知未来 policy version 必须安全地保留原消息而不是猜测投影。
8. **确定性。** plan action 按 transcript 顺序排列；写入 sidecar/数据库时按稳定顺序序列化，禁止依赖 `HashMap` 迭代顺序。

### 1.3 分期与启用策略

* Phase 1–3 完成前，新投影策略仅通过显式开发配置或 shadow mode 运行；现有 `micro_compact_threshold`、`auto_compact_threshold` 仍是兼容入口，不改变用户已有配置的反序列化能力。
* `micro_min_affected` 保留为兼容字段，但不得再作为新 Micro 成功或升级 Full 的判据；新判据是 `estimated_tokens_saved` 与 `target_reclaim_tokens`。
* `smart_compact_enabled` 不再拥有绕过安全投影的独立写 flag 路径。它可在后续作为 `CompactPolicy` 的候选排序模式，但必须走同一 plan/render/apply/report 链路。
* Full Compact 不重写摘要协议、不增加默认 LLM 请求；当 Micro 不足时，Full 只消费同一份**临时 projected view**，并在 Full 成功后才提交 Full 相关修改。

## 2. 目标架构

```text
TokenTracker + ContextBudget + runtime signals
                 │
                 ▼
          ContextPressure
                 │
                 ▼
plan_micro(transcript, pressure, policy, retention snapshot)
  ├─ TurnGroup / ToolExchange 分析
  ├─ 每个 action 的前后 token 估算
  ├─ tool-pair / provider 不变量验证
  └─ MicroCompactPlan（只含 ID、target、directive；不含消息副本）
                 │
        ┌────────┴────────┐n        │                 │
        ▼                 ▼
目标已满足           仍需 Full / 强制 Full
        │                 │
render_llm_view      render_llm_view（仅供 Full 摘要输入）
        │                 │
validate view         Full 成功后构造 Full apply；失败不留下 Micro 写入
        │                 │
apply_compaction_batch（一次内存提交、一次持久化 batch、一次 cache invalidation）
        │
        ▼
ApplyReport + CompactTelemetry → Reason / Langfuse / shadow 校准
```

> 图中 `\n` 仅表示两个分支；实现文档中的实际流程不依赖字符串拼接或运行时图解析。

### 2.1 新的领域类型与模块边界

在 `peri-agent/src/agent/compact_v2/` 新增两个纯逻辑模块，避免继续把策略、内容改写和持久化分散在 `micro.rs`、`reason.rs`：

| 模块 | 责任 | 不允许做的事 |
|---|---|---|
| `planner.rs` | `ContextPressure`、`CompactPolicy`、`TurnGroup`/`ToolExchange`、`MicroCompactPlan`、候选排序和 token 估算 | 修改 Transcript、调用 provider、做 I/O。 |
| `projection.rs` | `ProjectionAction`、directive、`render_llm_view`、消息/块级投影、协议不变量验证 | 写 flags、写 DB、修改原始消息。 |
| `mod.rs` | 选择 Micro/Smart/Full、调用 planner/render/apply、组装 `CompactResult` | 重新实现内容截断细节。 |
| `transcript.rs` | 持久化 directive 的内存状态、批量 apply、恢复与持久化屏障 | 决定候选或 Provider 语义。 |

实现以下类型；字段可按 Rust 所有权微调，但语义与序列化边界不得改变：

```rust
pub struct ContextPressure {
    pub estimated_tokens: u64,
    pub context_window: u32,
    pub output_reserve: u32,
    pub predicted_tool_growth: u32,
    pub safety_buffer: u32,
    pub cache_hit_rate: f64,
}

pub enum ProjectionTarget {
    Message,
    ContentBlock { index: usize },
    ToolCall { tool_call_id: String },
}

pub enum ProjectionAction {
    Keep,
    CompactText { max_chars: usize },
    CompactToolResult {
        keep_head: usize,
        keep_tail: usize,
        preserve_recovery_handle: bool,
    },
    CompactToolInput {
        fields: Vec<String>,
        preserve_shape: bool,
    },
    ReplaceMedia { placeholder: String },
    Exclude,
}

pub struct ProjectionActionEntry {
    pub message_id: MessageId,
    pub target: ProjectionTarget,
    pub action: ProjectionAction,
}

pub struct MicroCompactPlan {
    pub policy_version: u32,
    pub target_reclaim_tokens: u64,
    pub actions: Vec<ProjectionActionEntry>,
    pub estimated_before_tokens: u64,
    pub estimated_after_tokens: u64,
    pub estimated_tokens_saved: u64,
}

pub struct ApplyReport {
    pub candidate_count: usize,
    pub changed_messages: usize,
    pub changed_fields: usize,
    pub no_op_candidates: usize,
    pub estimated_tokens_saved: u64,
    pub persistence_batch_size: usize,
}
```

`ContextPressure` 提供 `target_tokens()` 与 `target_reclaim_tokens()`，使用：

```text
max(0,
    estimated_tokens
    - (context_window - output_reserve - predicted_tool_growth - safety_buffer)
)
```

溢出与小窗口必须使用 `saturating_sub`，而不是裸减法。`TokenTracker` 是唯一 token 事实来源；Compact 不另建累计 tracker。

### 2.2 directive 与 flags 的关系

将现有 `MessageFlags` 渐进扩展为“状态 + 可恢复投影 directive”：

* `truncated`/`excluded` 保留，保证已有 session 和旧数据库兼容。
* 为每个非默认消息追加 `projection: Option<MessageProjectionDirective>`，directive 内保存 `policy_version` 和**该 message 的 action entries**，不保存 `BaseMessage` 内容。
* `truncated=true` 的新数据必须同时拥有非空 directive；`excluded=true` 的新数据必须有明确的 whole-message 或完整 exchange 语义。
* 历史行出现 `truncated=true && projection=None` 时标记为 `LegacyV0`：只执行旧的 text-only 兼容投影，Blocks/Raw 保持原样并计入 telemetry 的 legacy/no-op。下一次成功的新 plan 才覆盖为 versioned directive；不对历史消息做猜测式重写。
* 未识别的未来 directive version 不应用局部变更，保留原消息并返回可观测的 `unsupported_policy_version` 原因；压力足够高时由 Full 路径接管。

### 2.3 Provider 能力与协议验证

在 `peri-agent/src/llm/mod.rs` 和 `peri-agent/src/agent/react.rs` 增加最小 `ProviderCapabilities` 查询，并由 `AgentModelBridge` 转发。能力至少表达：OpenAI-compatible、Anthropic、Generic 三种消息协议，以及“带签名 reasoning 是否必须整体保留”的规则。所有 wrapper（含 `Box<dyn ReactLLM>`、retry/decorator 实现）必须完整透传该方法；Mock 使用安全的 Generic 默认值。

`projection.rs` 在返回请求视图前执行 `validate_projected_view`：

* 每个保留下来的 ToolResult 都能追溯到同一视图中的 ToolUse/`tool_calls`；每个被 compact 的 tool call 保留原 ID 和 name。
* 被 compact 的 `ToolCallRequest.arguments` 与 Anthropic `tool_use.input` 根值仍为 object。
* OpenAI 历史 assistant `tool_calls` 与 tool role 消息的 id 顺序仍可被现有 adapter 序列化。
* 带 signature 的 `Reasoning` 要么完整保留，要么按能力整体移除；禁止局部截断 signature。
* CJK 截断统一使用 `chars().take(n)`，禁止字节切片。

## 3. 实施任务

### Task 0：建立特征化基线与模块脚手架

**目的：** 先把当前回归、受保护状态、恢复路径和 Provider 协议固定为测试合同，防止后续重构把“现在恰好能用”的行为静默丢失。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/compact_v2/micro_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/trigger_test.rs`
- Modify: `peri-agent/src/agent/stages/reason_test.rs`
- Modify: `peri-agent/src/session/transcript_test.rs`
- Modify: `peri-agent/src/thread/sqlite_store_test.rs`
- Create: `peri-agent/src/agent/compact_v2/projection.rs`
- Create: `peri-agent/src/agent/compact_v2/projection_test.rs`
- Create: `peri-agent/src/agent/compact_v2/planner.rs`
- Create: `peri-agent/src/agent/compact_v2/planner_test.rs`

- [ ] 在 `compact_v2/mod.rs` 声明 `planner` 和 `projection` 模块，并在 `#[cfg(test)]` 下以独立 `_test.rs` 文件挂载测试模块；不要把超过 30 行的测试内联到生产文件。
- [ ] 为现有行为补充不会依赖外部 LLM、时钟或网络的特征化测试：低预算 skip、受保护工具不被选择、错误 ToolResult 不被选择、ancestor 永不被选择、Full 无 LLM 时失败不 panic、连续失败降级。
- [ ] 用 `make_` 前缀工厂构造以下消息：Text、Blocks Image/Document、Raw provider blocks、带多个 tool call 的 AI、对应 ToolResult、带 signature reasoning、落盘提示的 Bash/Read 输出。测试数据只使用本地临时路径和非敏感占位内容。
- [ ] 添加 `projection_test.rs` 与 `planner_test.rs` 的最小编译测试，确认这些模块可被独立测试；此任务不改变运行时策略。

**验证：**

```bash
cargo test -p peri-agent --lib -- compact_v2::micro_test
cargo test -p peri-agent --lib -- compact_v2::trigger_test
cargo test -p peri-agent --lib -- stages::reason_test
```

---

### Task 1：定义可序列化的 plan、directive、报告与安全 retention 模型

**目的：** 将“flag 表示什么”和“LLM 应如何看到消息”拆开，建立可恢复且不含原始内容副本的领域模型。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/compact_v2/config.rs`
- Modify: `peri-agent/src/agent/compact_v2/micro.rs`
- Modify: `peri-agent/src/session/transcript.rs`
- Modify: `peri-agent/src/tools/mod.rs`
- Modify: `peri-agent/src/agent/react.rs`
- Modify: `peri-agent/src/llm/mod.rs`
- Modify: `peri-agent/src/llm/react_adapter.rs`
- Modify: 每个实现/包装 `ReactLLM`、`Model`、`BaseTool` 的 wrapper（以编译器和 `impl BaseTool for` 搜索结果为准）
- Modify: `peri-agent/src/agent/compact_v2/{projection,planner}_test.rs`

- [ ] 在 `projection.rs` 定义 `ProjectionTarget`、`ProjectionAction`、`ProjectionActionEntry`、`MessageProjectionDirective` 和 `ProviderCapabilities`。所有 directive 仅引用 `MessageId`、block index 或 `tool_call_id`，禁止嵌入 `BaseMessage`、Base64 或完整工具输出。
- [ ] 在 `planner.rs` 定义 `ContextPressure`、`CompactPolicy`、`MicroCompactPlan`、`ApplyReport` 和 `FullEscalationReason`。`MicroCompactPlan.actions` 必须按 transcript 位置稳定排序；`estimated_tokens_saved` 以饱和减法计算。
- [ ] 扩展 `MessageFlags`，增加带 `#[serde(default)]` 的 projection directive；保持旧 `truncated`/`excluded` JSON 可反序列化。将 `PersistOp` 预留为能承载一个 compaction batch 的富操作，而不是只能逐条 `UpdateFlags`。
- [ ] 在 `BaseTool` 添加 `context_retention() -> ContextRetention`；默认返回 `Preserve`，确保尚未标注的工具绝不会因新增 trait 被意外压缩。定义 `Preserve`、`StateBearing`、`SideEffectReceipt`、`Recomputable` 四个枚举值。
- [ ] 在 `Model` 和 `ReactLLM` 添加默认安全的 provider-capability 方法；`AgentModelBridge`、`Box<dyn ReactLLM>`、retry/wrapper 全部转发。Provider-specific 实现明确返回 OpenAI 或 Anthropic 能力，Generic 默认为保守保留 signed reasoning。
- [ ] 把当前 `micro_excluded_tools` 读取为兼容 fallback，而非新策略的唯一事实来源；新 `CompactPolicy` 优先使用 `ContextRetention`。在第一轮只为 `AskUserQuestion`、`goal`、`TodoWrite` 标注 `StateBearing`，其余尚未审计工具维持 `Preserve`。
- [ ] 在 `CompactConfig` 新增可 serde-default 的目标 headroom、shadow 和 cache-aware 配置入口；保留旧阈值和 `micro_min_affected` 的反序列化。新字段初始以 shadow/显式 opt-in 保护，避免仅因升级配置结构就改变线上行为。

**测试：**

- `test_projection_directive_serde_roundtrip`：directive 中只含 ID/target/action，往返后相等。
- `test_legacy_message_flags_deserialize_without_directive`：旧 JSON 仍可加载为 `LegacyV0` 兼容状态。
- `test_unknown_policy_version_is_safe_keep`：未知版本不生成局部改写。
- `test_base_tool_default_retention_is_preserve`：未 override 的手写测试工具默认保留。
- `test_provider_capability_delegates_through_react_adapter`：能力不会被 adapter/wrapper 静默丢失。

**验证：**

```bash
cargo test -p peri-agent --lib -- projection_test planner_test transcript_test
cargo clippy -p peri-agent --lib -- -D warnings
```

---

### Task 2：以 TurnGroup/ToolExchange 取代临近 round，并生成无副作用的 Micro plan

**目的：** 修复 P0-3、P1-2，并让“候选数量”与“可安全投影的 action”成为两个不同概念。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/micro.rs`
- Modify: `peri-agent/src/agent/compact_v2/smart.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/micro_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/trigger_test.rs`

- [ ] 将 `compute_round_starts` 替换为 planner 内的显式 `TurnGroup`/`ToolExchange` 视图：从 Human 开始，到下一条 Human 前结束；在组内以 `tool_call_id` 建 AI tool call 与所有 ToolResult 的索引，不依赖“紧邻 1–2 条”的假设。
- [ ] 对并行 tool call 建立 per-call candidate：同一 AI message 中的 Bash 可以生成 `CompactToolInput { tool_call_id }`，而 `AskUserQuestion`、`goal`、`TodoWrite` 对应 action 必须是 `Keep`。不得因为任一调用可压缩而给整个 AI message 打统一 truncated 标记。
- [ ] 只为以下安全对象生成可变 action：可重新获取的媒体 payload、成功且可重建的 ToolResult、已完成副作用工具的超大 input、明确允许的旧 reasoning。Human、错误结果、state-bearing/incomplete exchange 和未知 retention 必须产生 `Keep` 或完全不成为 candidate。
- [ ] 对 Raw/Unknown 内容明确分类：已识别的 image/document/tool_use/tool_result/thinking 类型可生成有定义的 action；无法证明协议安全的 Raw block 生成 `Keep` 与 no-op 原因，绝不先打 flag 再让 render 静默失败。
- [ ] 将现有 `micro_compact`/`smart_compact` 收缩为 planner 的兼容入口或删除其直接 `set_truncated`/`set_excluded` 写入。`smart_compact_enabled` 只能选择候选排序/保留窗口，不能绕过 `MicroCompactPlan`。
- [ ] planner 只读取 `MessageTranscript` 和 policy snapshot；不调用 `set_truncated`、`set_excluded`、`send_persist`、`invalidate_context_cache` 或 provider。

**测试：**

- `test_turn_group_collects_non_adjacent_tool_results`：中间插入其他消息时仍按 id 找到结果。
- `test_parallel_tool_exchange_preserves_pair_order`：多工具并行调用的 action 顺序稳定，全部 id 保留。
- `test_mixed_bash_and_ask_user_question_only_plans_bash`：仅 Bash 获得 action，AskUserQuestion 的 input/result 不变。
- `test_planner_never_targets_ancestor_or_human_or_error_result`。
- `test_unknown_raw_block_is_reported_as_no_op_not_flagged`。
- `test_same_transcript_and_policy_produce_stable_plan_order`。

**验证：**

```bash
cargo test -p peri-agent --lib -- planner_test micro_test trigger_test
```

---

### Task 3：实现 Provider 安全的 block-level projection，并让 Reason 只消费渲染视图

**目的：** 修复 P0-1、P0-4、P1-1；彻底移除 Reason 阶段对 `truncated_content(100)` 的隐式 Text-only 分支。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/projection.rs`
- Modify: `peri-agent/src/agent/compact_v2/projection_test.rs`
- Modify: `peri-agent/src/agent/stages/reason.rs`
- Modify: `peri-agent/src/agent/stages/reason_test.rs`
- Modify: `peri-agent/src/messages/message.rs`
- Modify: `peri-agent/src/messages/message_test.rs`
- Modify: `peri-agent/src/messages/adapters/openai_test.rs`
- Modify: `peri-agent/src/messages/adapters/anthropic_test.rs`
- Modify: `peri-agent/src/llm/openai_test.rs`
- Modify: `peri-agent/src/llm/anthropic_test.rs`

- [ ] 实现纯函数 `render_llm_view(transcript, directive_source, provider_caps) -> AgentResult<Vec<BaseMessage>>`。它从原始 entries 复制可见消息，按 directive 逐 block/逐 tool call 投影，最后运行 `validate_projected_view`；不修改 Transcript。
- [ ] 对 `MessageContent::Text` 用字符计数截断并附最小、非敏感的 compact 提示；普通 assistant 文本默认 `Keep`，Human 永远 `Keep`。
- [ ] 对 `Blocks`：移除 Image/Document 的 Base64 或大原始 payload，保留媒体类型、标题、URL/可恢复来源及“已压缩”的文本占位；对 Document 的大 Text 使用同样的安全 head/tail 规则。投影结果不得含原始 Base64 字节串。
- [ ] 对 `Raw`：按 provider block type 分类并执行显式保留或替换。已知媒体、tool、thinking、文本类型按对应规则处理；未知类型不得静默 no-op，也不得将任意原始 JSON 粗暴转换为不合法 provider block。不能安全投影时在 plan 阶段 `Keep`，并以 no-op telemetry 说明原因。
- [ ] 对 `BaseMessage::Ai` 同时更新派生 `tool_calls` 与 `MessageContent::Blocks` 中的 `ToolUse`，保持两份表示一致。仅 compact 被选中的 `tool_call_id`；未选中的调用逐字保留。
- [ ] 投影 tool input 时始终产出 JSON object。保留 id、name 与 policy 指定的定位字段；对于被省略的大字段，保留 object 根和必要字段，不把整个 JSON 序列化后塞进 `Value::String`。任何无法维持 object 根的候选回退为 `Keep`。
- [ ] 投影成功 ToolResult 时保留 `is_error=false`、tool call id、可读摘要、head、tail、exit/status 信息，以及终端中已有的 `saved to …` / `use Read tool …` 恢复句柄。错误 ToolResult 不进入该路径。
- [ ] 对 signed reasoning：依 provider capability 整体保留或整体移除；不可切割 text/signature。OpenAI-compatible 路径继续遵循 adapter 当前过滤 reasoning 的规则。
- [ ] 在 `reason.rs` 删除逐消息读取 `truncated` 后调用 `truncated_content(100)` 的循环，改为一次性取得 transcript 快照并调用 `render_llm_view`。在任何 `.await` 前释放 `RwLockReadGuard`。
- [ ] `BaseMessage::truncated_content` 在没有运行时调用者后删除，或仅保留为明确标为 `LegacyV0` 的私有兼容 helper；不得继续作为新策略 API。同步移除由此产生的死代码/测试。

**协议测试：**

- `test_blocks_image_and_document_projection_removes_base64_payload`。
- `test_raw_known_block_has_explicit_projection`，以及 `test_raw_unknown_block_is_kept_safely`。
- `test_tool_input_projection_preserves_object_root_and_tool_call_id`。
- `test_ai_blocks_and_derived_tool_calls_stay_in_sync`。
- `test_tool_result_projection_keeps_head_tail_and_recovery_handle`。
- `test_error_tool_result_is_unchanged`。
- `test_signed_reasoning_is_never_partially_truncated`。
- `test_cjk_projection_uses_character_boundary`。
- `test_openai_adapter_accepts_projected_tool_history`：assistant tool call 与 tool role result id 匹配、input 未变成 string。
- `test_anthropic_adapter_accepts_projected_tool_history`：`tool_use.input` 为 object、`tool_result.tool_use_id` 与其配对。
- 在 provider-level request-body 测试中用 projected messages 调用实际 OpenAI/Anthropic `build_request_body`，而不是只断言中间 `BaseMessage`。

**验证：**

```bash
cargo test -p peri-agent --lib -- projection_test reason_test message_test openai_test anthropic_test
cargo test -p peri-agent --doc
```

---

### Task 4：从消息数量迁移到 ContextPressure、token 目标和 dry-run Full 决策

**目的：** 修复 P0-2、P1-3、P1-4；让 Micro 的有效性取决于可验证的预计回收量，而非 flag 数量。

**文件：**
- Modify: `peri-agent/src/agent/token.rs`
- Modify: `peri-agent/src/agent/token_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/config.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/compact_v2/_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/trigger_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/full.rs`
- Modify: `peri-agent/src/agent/compact_v2/full_test.rs`
- Modify: `peri-agent/src/agent/stages/compact.rs`
- Modify: `peri-agent/src/agent/stages/compact_test.rs`

- [ ] 从 `TokenTracker::estimated_context_tokens()`、`cache_hit_rate()`、`ContextBudget.context_window` 和配置的 output reserve / predicted tool growth / safety buffer 构造 `ContextPressure`。当 tracker 没有可靠 usage 时，保持 skip 或 legacy gate，不编造 token 数。
- [ ] 在 planner 中为每个候选计算“原始序列化估算”和“投影后序列化估算”。估算函数必须与 provider 可见的 projected message 结构同源，使用稳定的字符级保守模型；它只用于决策/遥测，不污染 `last_usage` 的 API 精确值。
- [ ] 按 `target_reclaim_tokens` 从低语义损失到高语义损失排序选择 action：媒体 payload → 可重建 ToolResult → 已完成副作用工具 input → 可整体移除的 reasoning。普通 assistant 文本和 Human 不作为默认回收来源。
- [ ] 扩展 `CompactResult`，保留旧 `affected_count` 供兼容，同时附带 `ApplyReport`/估算值/可选 `FullEscalationReason`。新的 Micro 成功定义为 `estimated_tokens_saved >= target_reclaim_tokens` 或已满足目标 headroom；`changed_messages` 仅作诊断字段。
- [ ] 将 `micro_min_affected` 从新决策路径移除；保持 serde 兼容并在用户显式设置时记录一次非敏感 deprecation trace。百分比阈值继续作为何时尝试规划的兼容 gate，不再单独决定“已回收足够”。
- [ ] 把 `run_compact` 改为先生成 dry-run plan，再分支：
  1. plan 无有效改变：记录 no-op，并按压力决定 Full；
  2. Micro 足够：render/validate 后 apply；
  3. Micro 不足或达到强制 Full 阈值：不写 Micro flag/directive，将同一临时 projected view 传给 Full 摘要输入；只有 Full 成功才提交 Full 结果。
- [ ] 将 `full_compact_inner` 的摘要输入从内部直接调用 `visible_messages()` 改为接收已验证的 `Vec<BaseMessage>`。保持现有 Full summary prompt、re-inject 逻辑和 Human 注入方式；只替换输入来源。
- [ ] Full 失败时不留下本轮 Micro flags/directives，且不清除历史已成功应用的 directive。将旧的“失败时清理所有 excluded”逻辑收窄为本轮临时状态，避免回滚已持久化的有效压缩。
- [ ] 仅在 Full 成功后 reset `TokenTracker`；Micro apply 后保留 tracker，并等待下一次真实 LLM request 校准回收量，避免把估算当作 API usage。

**测试：**

- `test_context_pressure_calculates_target_reclaim_with_saturating_math`。
- `test_one_large_tool_result_can_meet_reclaim_target`。
- `test_many_short_results_with_zero_saved_do_not_make_micro_effective`。
- `test_micro_full_escalation_does_not_apply_micro_directives`。
- `test_full_receives_projected_view_when_micro_is_insufficient`。
- `test_full_failure_leaves_no_current_plan_flags_or_directives`。
- `test_legacy_percentage_gate_remains_compatible_but_not_success_metric`。
- `test_compact_result_reports_candidates_changes_and_estimated_saved_separately`。

**验证：**

```bash
cargo test -p peri-agent --lib -- token_test planner_test trigger_test compact_v2::_test full_test compact_test
```

---

### Task 5：提供一次事务、一次失效的 batch apply，并持久化/恢复 directive

**目的：** 修复 P2-1 及两条已知“compact flags 在恢复/跨 prompt 丢失”回归；保证 plan、投影视图和 session resume 语义一致。

**文件：**
- Modify: `peri-agent/src/session/transcript.rs`
- Modify: `peri-agent/src/session/transcript_test.rs`
- Modify: `peri-agent/src/thread/store.rs`
- Modify: `peri-agent/src/thread/sqlite_store.rs`
- Modify: `peri-agent/src/thread/sqlite_store_test.rs`
- Modify: `peri-agent/src/thread/filesystem.rs`
- Create: `peri-agent/src/thread/filesystem_test.rs`（若该模块尚无独立测试文件）
- Modify: `peri-acp/src/agent/builder.rs`
- Modify: `peri-acp/src/session/executor_helpers.rs`
- Modify: 与 session/load/resume/fork 相关的 `peri-acp/src/session/**/*.rs` 测试

- [ ] 定义稳定的 `CompactionUpdate { message_id, flags_with_directive }` 和 `PersistOp::ApplyCompactionBatch`。`MessageTranscript::apply_compaction_batch` 先验证所有 target 非 ancestor、存在且与 plan 一致，再一次性修改内存 flags/directives；相同 directive 的重复应用计入 `no_op_candidates`，不得重复写入。
- [ ] 为 writer 增加 batch acknowledgment/barrier：Compact stage 在把 owned transcript 写回并发出 `MessagesCompacted` 前等待该 batch 完成；不得跨 `.await` 持有 `parking_lot::RwLock` guard。这消除“compact 后立即 stop/resume，writer 尚未落盘”的竞态。
- [ ] 扩展 `ThreadStore`，增加 batch compaction 持久化 API。默认 trait 实现只能用于明确不支持的测试 store；生产 SQLite/Filesystem 必须 override，不能沿用逐条 no-op。
- [ ] SQLite：为 `messages` 增加可空 `projection` JSON 列（幂等 `ALTER TABLE`）；`apply_compaction_batch` 在一个 transaction 内更新所有 `(truncated, excluded, projection)`，并在同一 transaction 将对应 thread 的 `cached_context = NULL`。`load_message_flags` 读取 directive；旧 NULL 解析为 LegacyV0。
- [ ] Filesystem：在每个 thread 目录增加稳定排序的 `compact_state.json` sidecar，内容为 message id → flags/directive。用临时文件 + rename 原子替换；在 `delete_messages`、`delete_messages_since`、thread delete 时同步剔除遗留 state。实现 batch 写和 `load_message_flags`，不可继续默认 no-op。
- [ ] 所有序列化的 compact state 使用 stable vector 或 `BTreeMap`；不要把 `HashMap.values()` 直接写盘，避免 session 恢复后的 directive 顺序不稳定并破坏 provider prompt cache。
- [ ] `MessageTranscript::set_flags_batch` 恢复时同时恢复 directive；`rebuild`、rewind、Full 成功/失败路径必须保留或清理与被移除 message 精确对应的 directive。
- [ ] 保持 `with_persistence` 的调用位置在 v2 session 建立后不变，但确认 `peri-acp/src/agent/builder.rs` 的 `persist_tx` 存在路径与 `executor_helpers.rs` 的 history seed → `load_message_flags` → `set_flags_batch` 顺序都使用新 state。`persist_tx=None` 路径必须仍能在当前 process 内得到正确投影。
- [ ] 扩展每个 session/new、load、resume、fork 入口：在 seed 完历史后恢复 compact state；确保 ancestor 的消息 ID 不被当前 thread 的 state 覆盖。缺失/损坏 directive 记录 warning 并安全保留原消息，不能让 TUI 或 Reason 卡死。

**测试：**

- `test_apply_compaction_batch_is_idempotent_and_reports_changed_fields`。
- `test_apply_compaction_batch_performs_one_store_batch_and_one_cache_invalidation`：用测试文件内手写 `ThreadStore` spy 断言次数。
- `test_sqlite_batch_persists_directives_and_invalidates_cache_once`。
- `test_sqlite_load_legacy_flags_without_projection`。
- `test_filesystem_roundtrip_compact_state`。
- `test_rewind_and_delete_remove_compact_sidecar_entries`。
- `test_v2_persistence_enabled_restore_renders_same_view`。
- `test_v2_without_persist_tx_renders_same_view_within_process`。
- `test_cached_context_hit_and_miss_restore_flags_and_directives`。
- `test_ancestor_messages_never_receive_restored_current_thread_directives`。

**验证：**

```bash
cargo test -p peri-agent --lib -- transcript_test sqlite_store_test filesystem_test
cargo test -p peri-acp --lib -- executor_helpers
cargo build -p peri-agent -p peri-acp
```

---

### Task 6：将 Compact stage、Full、Reason 与 TUI 快照接到统一报告，完成端到端回归

**目的：** 让运行时只通过目标模型驱动决策，保证 TUI/Langfuse 看到的消息快照就是 Reason 实际发送的 projected view，且所有失败路径可恢复。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/stages/compact.rs`
- Modify: `peri-agent/src/agent/stages/compact_test.rs`
- Modify: `peri-agent/src/agent/stages/reason.rs`
- Modify: `peri-agent/src/agent/stages/reason_test.rs`
- Modify: `peri-agent/src/agent/events_v2.rs`
- Modify: `peri-agent/src/agent/events_v2_mapper.rs`
- Modify: `peri-agent/src/agent/events_v2_mapper_test.rs`
- Modify: `peri-acp/src/langfuse/bridge.rs`
- Modify: `peri-acp/src/langfuse/tracer/compact.rs`
- Modify: `peri-acp/src/langfuse/tracer/compact_test.rs`
- Modify: 必要时 `peri-acp/src/event/mapper.rs`、`peri-tui/src/kit/acp_events/` 下的对应 mapper/coverage tests

- [ ] 在 Compact stage 以 `ContextPressure` 建 plan，而非只将 `pct` 传给 `run_compact`。保留现有 cancel `select!`、start/end 成对 observe event、`after_compact` middleware hook 以及 `compact_post_hook`；新代码不得在取消时遗失 owned transcript。
- [ ] 仅在有实际 `changed_messages > 0` 或 Full 成功时发 `MessagesCompacted`；其 `messages` 快照必须来自 `render_llm_view` 的同源结果，而不是只调用 `visible_messages()` 后重新展示完整 truncated 内容。若 TUI 仍应展示原始历史，新增独立的 `llm_view` telemetry 字段或只发统计，避免把紧凑请求视图误当作用户历史替换。
- [ ] 在 `ObserveEvent::MessagesCompacted` 中增加一个向后兼容的 `CompactTelemetry` 值对象，包含：`candidate_count`、`changed_messages`、`changed_fields`、`no_op_candidates`、`estimated_tokens_before`、`estimated_tokens_after`、`estimated_tokens_saved`、`full_escalation_reason`、`persistence_batch_size`、`projection_latency_ms`。不携带原始 prompt、Base64、工具输出或任何秘密。
- [ ] 若需要扩展 `ExecutorEvent`/ACP DTO，遵守事件全链覆盖：`peri-agent` mapper、`peri-acp/event/mapper.rs`、`peri-tui` ACP events、`variant_coverage_test`/mapper tests 全部同步。优先把纯 telemetry 保留在 Observe/Langfuse 路径，避免无必要改变 UI 协议。
- [ ] Langfuse compact span 使用同一 `CompactTelemetry`；下一次 `LlmCallEnd.input_tokens` 关联 pending compact measurement，按提案公式记录：

  ```text
  actual_reclaim = previous_input_tokens
                   - next_request_input_tokens
                   + tokens_added_since_compact
  ```

  `TokenTracker` 只记录此校准元数据，不把估算值混入 `last_usage` 或 UI 的精确 API token 显示。
- [ ] 验证 Full 输入为 projection 后，Full 成功、Full 失败、取消、连续失败降级和下一轮 Reason 都保持 Start→End 事件成对、Transcript 完整、TUI 不永久 loading。

**端到端测试：**

- `Compact → Reason → 下一轮 prompt`：第二次 Reason 的 provider request 不含被移除 Base64/大 output，但具备恢复句柄和合法 tool pair。
- `Compact → session stop → resume → Reason`：恢复后请求视图与 compact 前进程内的请求视图等价。
- `cached context` 命中与未命中各一条；两者恢复的 directive 相同。
- `Micro → Full`：Micro 不足时不发生额外 batch write；Full 只在成功后改变 transcript。
- `Full 失败`：无当前 plan 残留 flags/directive，下一次可安全重试。
- OpenAI 和 Anthropic 两条 provider request-body 路径均通过。

**验证：**

```bash
cargo test -p peri-agent --lib -- compact_test reason_test events_v2_mapper_test
cargo test -p peri-acp --lib -- compact
cargo build -p peri-agent -p peri-acp -p peri-tui
```

---

### Task 7：引入 shadow mode、估算校准和 cache-aware 策略开关

**目的：** 在不先改变默认行为的情况下校准 token 估算，并在保证 headroom 的情况下减少不必要的 prompt-cache 失效。

**文件：**
- Modify: `peri-agent/src/agent/compact_v2/config.rs`
- Modify: `peri-agent/src/agent/compact_v2/planner.rs`
- Modify: `peri-agent/src/agent/compact_v2/mod.rs`
- Modify: `peri-agent/src/agent/token.rs`
- Modify: `peri-agent/src/agent/token_test.rs`
- Modify: `peri-agent/src/agent/compact_v2/{planner,trigger}_test.rs`
- Modify: `peri-acp/src/langfuse/{bridge.rs,tracer/compact.rs}`
- Modify: 相应 Langfuse/事件测试

- [ ] 实现 `micro_shadow_mode`：只构造 plan、render/validate 并上报估算，绝不 apply directive、写 cache、改变 Transcript 或让 Reason 使用 shadow 视图。manual/force Full 的既有语义保持不变；shadow 不得意外阻断紧急 Full。
- [ ] shadow telemetry 同时记录“当前真实 request 的 input tokens”和“按照 projected view 的估算 before/after”；明确标识为 counterfactual，不能将其报成已实际回收的 token。
- [ ] 在实际启用的 Micro 上，使用下一次真实 `LlmCallEnd` 完成 `actual_reclaim` 校准；当缺少前后 usage 或中间发生 Full/错误时记录 `measurement_unavailable`，不输出伪精度。
- [ ] 将 `TokenTracker::request_history` 用作 cache hit 趋势的输入，添加不会破坏 serde 的运行时 idle signal。cache-aware 决策必须遵守硬约束：若 `target_reclaim_tokens > 0`，高 cache hit 不能阻止安全 compact；只有 cache hit 高且已有足够 headroom 时才可返回 `DeferredForCache`。
- [ ] 长时间 idle 的主动清理仅在 cache-aware 显式启用、idle 达到配置阈值、plan 能安全回收大量 payload 时执行；时间来源通过可注入/显式 duration 测试，避免依赖真实 wall clock。
- [ ] 默认配置继续关闭 cache-aware 主动策略，直到 shadow 数据证明估算误差和 provider cache 影响可接受。将启用条件写入配置注释和 release note，而非静默改变 `0.75/0.95`。

**测试：**

- `test_shadow_mode_does_not_mutate_flags_or_persist`。
- `test_shadow_mode_reports_counterfactual_estimate`。
- `test_actual_reclaim_uses_next_real_input_and_added_tokens`。
- `test_cache_hit_defers_only_when_headroom_is_sufficient`。
- `test_hard_reclaim_target_overrides_cache_deferral`。
- `test_idle_cleanup_requires_opt_in_and_explicit_duration`。

**验证：**

```bash
cargo test -p peri-agent --lib -- token_test planner_test trigger_test
cargo test -p peri-acp --lib -- langfuse
```

---

### Task 8：逐步将工具分类下沉为 retention metadata，并清理旧黑名单依赖

**目的：** 落实长期策略，防止继续通过集中式工具名黑名单修补遗漏，同时不扩大压缩范围。

**文件：**
- Modify: `peri-agent/src/tools/mod.rs`
- Modify: 各 Core/Deferred tool 的 `impl BaseTool`（仅审计后明确分类的工具）
- Modify: `peri-middlewares/src/**` 中包装、过滤或代理 `BaseTool` 的实现
- Modify: `peri-agent/src/agent/compact_v2/{planner,projection}.rs`
- Modify: 工具所属测试与 `peri-agent/src/agent/compact_v2/planner_test.rs`

- [ ] 为已经验证的工具逐个声明 retention：用户交互/状态类为 `StateBearing`，已完成副作用但只需收据的工具为 `SideEffectReceipt`，可从磁盘/网络重新获得的只在证据充分后标为 `Recomputable`。未审计工具始终保留 `Preserve`。
- [ ] 对每一个 override 增加一条 planner 合约测试，验证它产生的 action 与保护级别一致；不得仅修改集中名单而无测试。
- [ ] 检查所有 tool wrapper/过滤器/代理，完整透传 `context_retention()`，避免包装后悄悄退回 default。此项遵守项目“工具包装层必须完整透传 trait 方法”的约束。
- [ ] 当全部当前黑名单工具已有 metadata 覆盖且生产观察稳定后，保留 `micro_excluded_tools` 仅作旧配置兼容 fallback；不删除字段前记录迁移说明。绝不将 fallback 默认改为“未知工具可压缩”。
- [ ] 语义记忆、额外 LLM 摘要、外部 cache-edit API 均不在本任务实现；若后续引入，必须是显式可选层且复用同一 plan/render/apply/report 不变量。

**验证：**

```bash
cargo test -p peri-agent --lib -- planner_test
cargo test -p peri-middlewares --lib
cargo clippy -p peri-agent -p peri-middlewares --lib -- -D warnings
```

## 4. 覆盖矩阵

| 提案内容 | 交付任务 | 验收证据 |
|---|---|---|
| P0-1 Blocks/Raw flag 但不投影 | Task 2、Task 3 | Blocks/Raw/media projection 与 provider request-body 测试。 |
| P0-2 `affected_count` 不是收益 | Task 4、Task 6、Task 7 | `estimated_tokens_saved`、真实请求校准、独立 telemetry。 |
| P0-3 混合 tool call 误伤 | Task 2、Task 3、Task 8 | per-`tool_call_id` plan、混合 Bash + AskUserQuestion 测试。 |
| P0-4 JSON shape 破坏 | Task 3 | OpenAI/Anthropic tool input object 与 ID 配对测试。 |
| P1-1 固定前 100 字符丢恢复入口 | Task 3 | head/tail、status、`saved to`/`Read` 句柄测试。 |
| P1-2 简化 round 分组 | Task 2 | `TurnGroup`/`ToolExchange` 非紧邻与并行调用测试。 |
| P1-3 Micro 后立即 Full 的额外写入 | Task 4、Task 5、Task 6 | dry-run 后才 apply；Full 前无 Micro batch；Full 失败无残留。 |
| P1-4 无显式 headroom | Task 4 | `ContextPressure` target reclaim 与饱和数学测试。 |
| P2-1 N 次写与 cache invalidation | Task 5 | SQLite 一 transaction、Filesystem 一 sidecar 更新、spy 次数测试。 |
| System Prompt / tool-pair / state / ancestor 不变量 | Task 1–6 | projection validator、状态工具、ancestor、adapter 与恢复测试。 |
| 推荐 `ContextPressure`、action、plan、report API | Task 1、Task 4 | 纯模块边界、serde 与 no-side-effect 测试。 |
| 投影优先级、reasoning 与媒体策略 | Task 2、Task 3 | 每类 action 的单元测试。 |
| Phase 1–4 路线 | Task 1–8 | 安全投影 → token 目标 → 持久化 → cache/metadata 的顺序。 |
| 必须单测与集成/回归场景 | Task 0、3–7 | 下节完整测试门禁。 |
| 可观测性、shadow、估算误差 | Task 6、Task 7 | `CompactTelemetry`、Langfuse、实际 reclaim 校准。 |
| 非目标与风险 | 全部任务的 guardrail | 下一节明确限定与风险门禁。 |

## 5. 完整测试门禁

除每任务的定向命令外，默认启用前必须依次运行：

```bash
cargo fmt --all -- --check
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-middlewares --lib
cargo test -p peri-agent --doc
cargo clippy -p peri-agent -p peri-acp -p peri-middlewares --lib -- -D warnings
cargo build --workspace
```

若修改了 `ExecutorEvent`、ACP mapper 或 TUI event payload，还必须运行对应 mapper/variant coverage 测试和：

```bash
cargo test -p peri-tui --lib
```

回归测试需在测试名或紧邻注释标明来源，至少覆盖：

- `spec/archive-issues/agent-core/2026-07-17-compact-flags-lost-on-session-restore.md`；
- `spec/archive-issues/agent-core/2026-07-18-compact-effect-lost-between-prompts-v2.md`；
- `spec/issues/2026-07-25-micro-compact-preserve-ask-user-question.md`。

所有测试采用手写 trait 实现与 `make_` 工厂，不使用网络、真实 provider、随机值或真实时间；错误路径断言错误类型/内容或安全 fallback 原因，而不只断言 `is_err()`。

## 6. rollout、指标与停止条件

1. **内部测试阶段：** 完成 Task 1–6，保持新策略非默认；对可控会话执行语义与恢复测试。
2. **Shadow 阶段：** 启用 Task 7 的 shadow mode，收集 `estimated_before/after/saved`、下一次实际 input、cache creation/read 与 `measurement_unavailable` 原因。shadow 绝不修改请求或持久化状态。
3. **小范围 opt-in：** 仅在估算误差、adapter 协议和恢复测试稳定后启用真实 apply；持续检查 `actual_reclaim`、Full escalation、cache hit 变化和 projection latency。
4. **默认启用前门槛：** 不存在 tool-pair/protocol violation、恢复后 directive 丢失、状态工具误压缩或 cache invalidation 重复；估算模型在目标 provider 上有可解释的误差分布。数值阈值基于 shadow 数据确定，不在缺乏数据时盲调 `0.75/0.95`。
5. **立即停止/回退：** 若出现 provider 请求校验失败、AskUserQuestion/goal/Todo 状态损失、恢复语义不一致、TUI loading 卡死或 `actual_reclaim` 长期为负，关闭新策略开关，保留 telemetry，使用现有 Full/manual 路径诊断；不要通过继续扩大黑名单或改阈值掩盖根因。

## 7. 明确非目标

本计划不包含：

- 修改 Frozen System Prompt、破坏 prompt-cache 静态前缀，或向中途消息注入新的 System 消息；
- 重写 Full Compact 的摘要 prompt/协议；
- 在 Micro 默认路径新增额外 LLM 调用；
- 立即实现语义记忆、外部 cache-edit API 或无边界的工具分类迁移；
- 删除历史原始消息、把 Base64/工具输出记录写入 telemetry，或将任何秘密写入日志。

## 8. 风险与对应防线

| 风险 | 防线 |
|---|---|
| Provider 工具协议被破坏 | projection validator + OpenAI/Anthropic request-body 双路径测试 + fail-closed `Keep`。 |
| token 估算不准确 | shadow counterfactual、下一真实请求校准、估算与 API usage 分离。 |
| directive 在恢复时丢失 | versioned directive、SQLite batch、Filesystem sidecar、persist barrier、resume/cached-context 双路径测试。 |
| Micro 与 Full 重复工作 | dry-run 决策；Full 使用临时 projected view；Full 前不提交不足的 Micro。 |
| 工具分类遗漏 | `Preserve` 默认、逐工具 opt-in metadata、wrapper 透传测试。 |
| 大批量更新拖慢会话 | 单 batch transaction、单 cache invalidation、稳定序列化；Compact 不在 RwLock guard 跨 await。 |
| 旧持久化数据无法解释 | `LegacyV0` 安全兼容，不猜测未知版本，下一次成功 plan 再升级。 |
| 指标泄露内容或秘密 | telemetry 只含计数、token、原因、耗时；不含 message/content/path/URL/raw provider body。 |
