# Langfuse 监控 v2 架构重设计

> 日期：2026-07-15（v2.1 修订） | 模块：`langfuse-client` + `peri-acp/src/langfuse/` + `peri-agent/ExecutorEvent` | 类型：架构重设计（方案 B：一次性大重构）

---

## 0. 摘要

当前 Langfuse 监控对 `docs/design/` 核心架构覆盖弱：仅 LLM 调用、工具调用、Compact Span、SubAgent 嵌套被上报；ReAct 5 阶段、15+5 中间件链、ContextBudget 阈值点、Compact 三级策略、MessageQueue 续跑、Workflow 调用、AiReasoning 思考过程、TurnError 原因等核心架构动态在 Langfuse UI 上不可见。同时 trace_id 与 turn_id 脱节、无 Sampling 机制、LangfuseTracer 内部 13 字段 `pub(crate)` 散在 6 handler 文件（架构 review 02 候选）。

本设计采用**方案 B 一次性大重构**：

1. **三层映射**：1 个 peri Session → 1 个 Langfuse Session；1 个 turn → 1 个 Trace（trace_id = turn_id）；5 阶段 → 5 个顶层 Span。
2. **12 个新增 ExecutorEvent 变体 + 2 个扩充**，覆盖 ReAct 5 阶段、中间件链、ContextBudget 阈值点、AiReasoning、MessageQueue 排空、Workflow 调用。
3. **LangfuseTracer 内部重构**：13 字段收敛为 6 简单字段 + 7 子状态机（4 个复用架构 review 02 设计 + 3 个新增：SamplingDecider / StageSpans（含 MQ 排空 + Workflow 子能力）/ MiddlewareTracer）。
4. **Sampling**：turn 级采样（hash + rate），错误 turn 强制发 ErrorSpan 挂同 turn。
5. **配置**：5 新增环境变量全部支持 `~/.peri/settings.json`。
6. **测试**：680 行子对象单测 + trait 抽取（架构 review 02 候选 06）+ e2e mock 端到端验证。

---

## 1. 总体架构

### 1.1 三层映射

```
Perihelion 会话模型              Langfuse v4 对象
─────────────────              ──────────────
1 个 peri Session       →      1 个 Langfuse Session（新增）
   └ N 个 Turn          →         └ N 个 Trace（trace_id = turn_id）
        └ 5 个阶段      →             └ N 个顶层 Span（Compact/Receive/Reason/Act/End，条件上报）
             └ 子事件   →                  └ Generation / Observation / 子 Span
```

**关键变化**：

- **引入 Langfuse Session 对象**：v4 原生支持。当前每个 turn 是孤立 Trace，UI 上无会话聚合。重设计后 1 个 peri Session 在 Langfuse 上对应 1 个 Session 对象，所有 turn Trace 挂 session_id 下，UI 可按会话回溯。
- **trace_id = turn_id**：当前 trace_id 是 `uuid::Uuid::now_v7()` 在 tracer `new()` 时生成，与 turn_id 无关。重设计后 `trace_id = turn_id`（架构文档 §2.6 明确要求 "turn_id 作为统一纽带"）。Session id 复用 peri 的 `session_id`。
- **阶段条件上报**：Compact 在阈值以下时不上报（不占位）；Act 在无工具调用时不上报。UI 上看到的 5 阶段 Span 树反映 turn 真实执行路径。

### 1.2 Langfuse UI 上的最终视图

```
Session: sess_abc  (会话："帮我重构 langfuse")
├─ Trace: turn_001 (turn_id)    [sampled ✓]
│  ├─ Span: Receive        2ms
│  │  └─ Span: MW[Hook]    1ms
│  ├─ Span: Reason         2100ms
│  │  ├─ Generation: claude-4.7  2000ms  (cache_read=12000)
│  │  └─ AiReasoning       50ms    (thinking...)
│  ├─ Span: Act            800ms
│  │  ├─ Observation: Read  200ms
│  │  └─ Observation: Edit  600ms
│  └─ Span: End            1ms  (status: Done)
│
├─ Trace: turn_002 (turn_id)    [sampled ✗ dropped]
│  (turn 级采样命中 drop，整 turn 不上报)
│
└─ Trace: turn_003 (turn_id)    [sampled ✓, Compact 触发]
   ├─ Span: Compact        1200ms
   │  └─ metadata: strategy=Full, trigger=Auto, threshold=0.85,
   │              tokens_before=48000, tokens_after=15000
   ├─ Span: Reason         1800ms
   └─ Span: End            1ms  (status: Interrupted, error_kind=ToolFailure)
                                                    ↑
                              ErrorSpan metadata.is_synthetic=true（如该 turn sampled=false）
```

### 1.3 trace_id / turn_id / session_id 契约

| ID | 生成时机 | 不变量 |
|----|---------|--------|
| `session_id` | peri Session 创建时（不变） | 与 Langfuse Session.id 共享同一字符串 |
| `turn_id` | turn 开始时（每轮 ReAct 循环） | 1 turn = 1 turn_id，turn 结束即销毁 |
| `trace_id` | tracer `new()` 时（由 caller 传入 turn_id） | **必须等于 turn_id**，禁止独立生成 |

`forward_langfuse_event` 接收的所有 ExecutorEvent 必须携带 turn_id（peri-agent 已有该字段）。Tracer 不再自己生成 trace_id。

> **⚠️ 生产路径偏差（待修复）**：`executor_helpers.rs:197` 实际调用 `LangfuseTracer::new()` 而非 `new_with_turn_id()`，导致 `trace_id` 仍为 `uuid::Uuid::now_v7()` 独立生成，不等于 `turn_id`。`new_with_turn_id()` 方法已实现（`tracer/mod.rs:95-105`）但未在生产代码中被调用。此偏差需在后续 commit 中修复（改用 `new_with_turn_id` 并从 turn context 传入 turn_id）。

---

## 2. 接入点与事件流

### 2.1 新增 ExecutorEvent 变体清单

按 5 阶段分类，每个新变体都携带 `turn_id`：

| 阶段 | 新增 ExecutorEvent 变体 | 触发位置（peri-agent） |
|------|----------------------|----------------------|
| **生命周期** | `SessionStarted { session_id, frozen_summary }` | `execute_prompt` 入口 |
| | `TurnStarted { turn_id, session_id }` | ReAct 循环开始 |
| | `TurnEnded { turn_id, status: Done\|Interrupted\|Error, error_kind? }` | ReAct 循环结束 |
| **5 阶段** | `StageStarted { turn_id, stage: Compact\|Receive\|Reason\|Act\|End }` | `stages/*.rs` 每个阶段入口 |
| | `StageEnded { turn_id, stage, status: Done\|Skipped\|Error, duration_ms }` | `stages/*.rs` 每个阶段出口 |
| **Compact** | `BudgetThresholdHit { turn_id, threshold: Micro\|Full, current_pct, tokens_in, tokens_out }` | ContextBudget 检查时（仅在阈值触发时发） |
| | 扩充 `CompactStarted { turn_id, strategy: Micro\|Full\|Smart, trigger: Auto\|Manual }` | `stages/compact.rs` |
| | 扩充 `CompactCompleted { token_before, token_after, files_count, skills_count }` | 同上 |
| **Receive** | `MessageQueueDrained { turn_id, prompt, defer, info }` | `stages/receive.rs` 排空 MQ 后 |
| **Reason** | `AiReasoningChunk { turn_id, text }` | 替代当前被丢弃的 `AiReasoning`，挂 Reason Span 下 |
| **Act** | `WorkflowStarted { turn_id, workflow_id, plan_summary }` | Workflow 中间件 |
| | `WorkflowEnded { turn_id, workflow_id, agents_spawned, tool_calls }` | 同上 |
| **跨阶段** | `MiddlewareStarted { turn_id, mw_name, hook }` | `agent/events.rs` 15+5 链 dispatch 前 |
| | `MiddlewareEnded { turn_id, mw_name, status, error? }` | dispatch 后 |

**总计**：12 个新变体 + 扩充 2 个现有变体（CompactStarted/Completed）。

**MCP 调用不另设变体**：MCP 工具调用走现有 `ToolStart/End` 通道，工具 name 已能区分，无需额外标记。

### 2.2 ExecutorEvent 新变体的归属 crate

| 变体 | 归属 | 理由 |
|------|------|------|
| `SessionStarted/TurnStarted/TurnEnded` | `peri-agent/src/agent/events_v2.rs` | 会话/turn 生命周期归 peri-agent |
| `StageStarted/StageEnded` | `peri-agent/src/agent/events_v2.rs` | ReAct 阶段归 peri-agent |
| `MiddlewareStarted/Ended` | `peri-agent/src/agent/events.rs` | 链 dispatch 归中间件链 |
| `AiReasoningChunk` | `peri-agent/src/agent/events_v2.rs` | 替代 `AiReasoning` |
| `MessageQueueDrained` | `peri-agent/src/agent/stages/receive.rs` | MQ 排空归 receive 阶段 |
| `BudgetThresholdHit` | `peri-agent/src/agent/stages/compact.rs` | 阈值检测归 compact |
| `WorkflowStarted/Ended` | `peri-middlewares/src/workflow/` | Workflow 归 workflow 中间件 |

每个变体新增后**必须**同步：
- `peri-acp/src/event/mapper.rs`（事件映射）
- `peri-tui/src/kit/acp_events.rs`（VIEW_MODELS 转换）

CLAUDE.md 陷阱速查「AgentEvent 变体」明确要求。新增 `variant_coverage_test.rs` 枚举所有变体，断言每个在 mapper_test 中有对应 test。

### 2.3 forward_langfuse_event 路由扩展

`peri-acp/src/session/executor_helpers.rs:287-490` 的 match 扩展为：

```rust
match event {
    // ── Session/Turn 层（驱动 trace 与 session 生命周期）
    SessionStarted { .. } => tracer.on_session_start(...),
    TurnStarted { .. } => tracer.on_turn_start(...),     // 内部决定 sampling
    TurnEnded { .. } => tracer.on_turn_end(...),         // 内部 flush 或 drop

    // ── 阶段
    StageStarted { .. } => tracer.on_stage_start(...),
    StageEnded { .. } => tracer.on_stage_end(...),

    // ── 中间件
    MiddlewareStarted { .. } => tracer.on_middleware_start(...),
    MiddlewareEnded { .. } => tracer.on_middleware_end(...),

    // ── Reason 子事件
    AiReasoningChunk { .. } => tracer.on_ai_reasoning_chunk(...),
    LlmCallStart/Payload/End/Retrying => /* 现有，挂 Reason Span 下 */

    // ── Act 子事件
    ToolStart/End => /* 现有，挂 Act Span 下 */
    WorkflowStarted/Ended => tracer.on_workflow_*

    // ── Compact 子事件
    BudgetThresholdHit => tracer.on_budget_threshold_hit,
    CompactStarted/Completed/Error => /* 现有，扩充字段 */

    // ── Receive 子事件
    MessageQueueDrained => tracer.on_mq_drained,

    TextChunk => /* 现有，累加 final_answer */
    _ => {} // 状态层/渲染层事件不上报
}
```

### 2.4 数据流

```
peri-agent ReAct 循环
    │
    ├─ stages/compact.rs   emit BudgetThresholdHit / CompactStarted / CompactCompleted
    ├─ stages/receive.rs   emit MessageQueueDrained
    ├─ stages/reason.rs    emit LlmCall* / AiReasoningChunk
    ├─ stages/act.rs       emit ToolStart/End / WorkflowStarted/Ended
    └─ stages/end.rs       emit TurnEnded
    │
    ▼
ExecutorEvent stream（已有）
    │
    ▼
peri-acp/executor_helpers.rs::forward_langfuse_event
    │  扩充 match 分支
    ▼
LangfuseTracer.on_xxx（parking_lot::Mutex 保护）
    │
    ├─ SamplingDecider    turn 开始决定 sampled=true/false（被动，不通知 caller）
    ├─ StageSpans         5 阶段 Span 生命周期 + MQ 排空计数（Receive 子能力）+ Workflow Span（Act 子能力）
    ├─ GenerationTracker  LLM 调用
    ├─ ToolBatch          工具组 Span
    ├─ SubagentStack      SubAgent 嵌套
    ├─ CompactSpan        Compact Span
    └─ MiddlewareTracer   中间件 Span
    │
    ▼
batcher.try_add (IngestionEvent)
    │
    ▼
Langfuse 后端（OTLP v4）
```

**关键性质**：

- Tracer 完全被动接受事件，不向 caller 返回 SampleDecision。
- sampled=false 时所有 on_* 入口 silently no-op（不构造事件、不入队），caller 不感知。
- 错误 turn 触发 ErrorSpan 挂同 turn（见 §4.3）。

---

## 3. LangfuseTracer 内部重构

### 3.1 主 struct 最终形状

```rust
pub struct LangfuseTracer {
    // ── 构造期固定、运行期只读 ──
    session: Arc<dyn LangfuseSessionLike>,
    session_id: String,
    trace_id: String,                // == turn_id，由 caller 传入，禁止自生成
    agent_observation_id: String,
    config: LangfuseConfig,           // 采样率、ErrorSpan 策略等

    // ── 累积字段（单字段，无内聚类） ──
    final_answer: String,

    // ── 7 个子状态机（pub(crate)） ──
    sampling: SamplingDecider,        // 新增
    stages: StageSpans,               // 新增（含 MQ 排空 + Workflow 子能力）
    middleware: MiddlewareTracer,     // 新增
    generation: GenerationTracker,    // 复用 review 02 设计
    tool_batch: ToolBatch,            // 复用 review 02 设计
    subagent: SubagentStack,          // 复用 review 02 设计
    compact: CompactSpan,             // 复用 review 02 设计
}
```

**13 字段，其中 7 个是子对象**（pub(crate)），6 个简单字段。从"14 pub(crate) 散字段"→"6 简单 + 7 子对象 pub(crate) 字段"。所有跨字段不变量收口在子对象 impl 内。

**设计取舍**：MqDrainTracker 与 WorkflowSpan 仅在特定阶段（Receive / Act）生效，与 StageSpans 强耦合，独立子对象过度拆分。合并入 StageSpans 后内聚类更紧、字段更少。代价是 StageSpans 文件略大（约 200-300 LOC），但仍可独立测试。

### 3.2 7 个子状态机的职责契约

每个子对象回答 what / how / depends on：

| 子对象 | What（职责） | How（关键方法） | Depends on |
|--------|--------------|----------------|------------|
| `SamplingDecider` | turn 开始决定是否上报；错误 span 兜底 | `should_emit(turn_id, session_id) -> bool`、`cleanup_turn(turn_id)` | session_id（hash 种子）、rate |
| `StageSpans` | 5 阶段（Compact/Receive/Reason/Act/End）Span 生命周期；当前活动阶段栈；Receive 阶段 MQ 排空计数；Act 阶段 Workflow 调用 Span | `on_stage_start(stage) -> StageHandle`、`on_stage_end(handle, status)`、`on_mq_drained(counts)`（仅 Receive）、`on_workflow_start(plan)` / `on_workflow_end(stats)`（仅 Act） | session.batcher、trace_id、agent_observation_id（parent） |
| `MiddlewareTracer` | 15+5 中间件调用 Span；按 hook 分组 | `on_start(name, hook)`、`on_end(name, status)` | 当前活动 StageHandle（决定挂哪个阶段下） |
| `GenerationTracker` | LLM step 生命周期（active_step + retry） | `on_llm_start/end/retrying`、返回 `GenerationStart/End` | trace_id |
| `ToolBatch` | 工具组 Span 批次（lazy 创建、累积、flush） | `on_tool_start/end`、`flush()` | trace_id |
| `SubagentStack` | SubAgent 嵌套栈（每层含独立 ToolBatch） | `begin_subagent`、`end_subagent`、`current_agent_id` | trace_id |
| `CompactSpan` | Compact Span on/off + 三级策略 metadata | `on_start(strategy, trigger)`、`on_end(token_before, token_after)` | trace_id |

子对象方法签名**禁止接收 `&mut LangfuseTracer`**，否则破坏 disjoint borrow。CI 加 grep check 防止。

### 3.3 公开接口（21 个 on_* 方法）

```rust
impl LangfuseTracer {
    // ── Session/Turn 生命周期 ──
    pub fn on_session_start(&mut self, frozen_summary: serde_json::Value); // 当前为 stub：仅 debug log，不发送 SessionCreate 事件
    pub fn on_turn_start(&mut self, input: &str);                       // 返回 ()，sampled 内部决定
    pub fn on_turn_end(&mut self, error_output: Option<&str>) -> JoinHandle<()>;

    // ── 5 阶段 ──
    pub fn on_stage_start(&mut self, stage: Stage, turn_id: &str);
    pub(crate) fn on_stage_end(&mut self, handle: &StageHandle, status: StageStatus);

    // ── Reason 子事件 ──
    pub fn on_ai_reasoning_chunk(&mut self, text: &str);               // 新（当前为 stub：仅 debug log，不生成 Langfuse 事件）
    pub fn on_llm_start(&mut self, step, messages, tools);             // 现有
    pub fn on_llm_request_payload(&mut self, step, body);              // 现有
    pub fn on_llm_end(&mut self, step, model, output, usage);          // 现有
    pub fn on_llm_retrying(&mut self, attempt, max, delay, error);     // 现有

    // ── Act 子事件 ──
    pub fn on_tool_start(&mut self, tool_call_id, name, input);        // 现有
    pub fn on_tool_end(&mut self, tool_call_id, output, is_error);     // 现有
    pub fn on_workflow_start(&mut self, workflow_id, plan);            // 新
    pub fn on_workflow_end(&mut self, workflow_id, stats);             // 新

    // ── Compact 子事件 ──
    pub fn on_budget_threshold_hit(&mut self, threshold, pct, tokens); // 新（当前为 stub：仅 debug log，不生成 Langfuse 事件）
    pub fn on_compact_start(&mut self);                                // strategy/trigger 内部硬编码为 Full/Auto，未参数化
    pub fn on_compact_end(&mut self, summary: &str, files_count: usize, skills_count: usize, micro_cleared: usize, is_error: bool, error_message: &str);

    // ── Receive 子事件 ──
    pub fn on_mq_drained(&mut self, prompt, defer, info);              // 新

    // ── 跨阶段 ──
    pub fn on_middleware_start(&mut self, name, hook);                 // 新
    pub fn on_middleware_end(&mut self, name, status, err);            // 新

    // ── 文本累加 ──
    pub fn on_text_chunk(&mut self, text);                             // 现有
}
```

**`on_trace_start` / `on_trace_end` 重命名为 `on_turn_start` / `on_turn_end`**（语义对齐 trace_id == turn_id）。

`forward_langfuse_event` 的下游调用点（`executor_helpers.rs:189-353`、`workflow_agent.rs:142-504`）一并改签名——这是破坏性变更但不残留旧名。

### 3.4 不变量从注释升级到类型系统

| 现有不变量 | 现状 | 升级后 |
|----------|------|--------|
| `active_step` 与 `generation_data` 同步 | mod.rs:75-76 注释 | `GenerationTracker` 私有字段，外部只能 `on_llm_start/end` 操作 |
| `tools_batch_span_id` 三处消费 | 隐式 grep 才能找全 | `ToolBatch::flush()` 唯一消费点 |
| Agent 工具 PendingTool 落父级 | tool_handler.rs:60-68 `[TRAP]` | `SubagentStack::is_agent_tool_anywhere()` 收口（注释仍保留，约束跨子对象） |
| `trace_id` 一次性生成 | mod.rs:48-51 注释 | 构造期注入 + 私有字段 + 无 setter |
| **新：当前阶段与子事件归属** | 当前无约束（散在 generation/tool 中） | `StageSpans::current_handle()` 返回当前阶段，子事件 try_add 时 parent 必须传 handle |
| **新：sampled=false 时所有事件 no-op** | 当前无 | `SamplingDecider::should_emit() -> bool` 在所有 on_* 入口检查 |
| **新：错误 turn 强制 ErrorSpan 挂同 turn** | 当前无 | `on_turn_end(status=Error)` 检测错误，补发 TraceCreate + ErrorSpan（trace_id = turn_id） |

### 3.5 LangfuseSessionLike trait 抽取（架构 review 02 候选 06）

```rust
pub trait LangfuseSessionLike: Send + Sync {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError>;
    fn flush(&self) -> Pin<Box<dyn Future<Output = Result<(), LangfuseError>> + Send + '_>>;
    fn session_id(&self) -> &str;
}
```

`LangfuseSession`（生产）和 `FakeLangfuseSession`（测试）都 impl。`LangfuseTracer` 持有 `Arc<dyn LangfuseSessionLike>`。

> **IngestionEvent 变体**：`langfuse-client/src/types/mod.rs` 代码注释写"10 种变体"，但实际枚举有 12 个变体——在原有 10 个基础上新增 `SessionCreate` 和 `SessionUpdate`（对应 Langfuse v4 Session API）。文档后续若涉及事件类型枚举需注意此差异。

**测试 fake**：

```rust
pub(crate) struct FakeLangfuseSession {
    events: parking_lot::Mutex<Vec<IngestionEvent>>,
    session_id: String,
}

impl LangfuseSessionLike for FakeLangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.events.lock().push(event);
        Ok(())
    }
    // ...
}

impl FakeLangfuseSession {
    pub(crate) fn assert_event_count(&self, expected: usize) { /* ... */ }
    pub(crate) fn assert_event_tree(&self, expected: &[(parent, child)]) { /* ... */ }
}
```

---

## 4. Sampling 与配置

### 4.1 环境变量

| 变量 | 默认 | 含义 |
|------|------|------|
| `LANGFUSE_PUBLIC_KEY` | — | 必填，缺失则整 langfuse 禁用（保留现有） |
| `LANGFUSE_SECRET_KEY` | — | 必填 |
| `LANGFUSE_BASE_URL` | `https://cloud.langfuse.com` | 保留现有 |
| **`LANGFUSE_TRACE_SAMPLING`** | `1.0` | turn 级采样率，0.0~1.0，1.0=全报 |
| **`LANGFUSE_ERROR_SPAN_ALWAYS`** | `true` | 错误 turn 强制发 ErrorSpan 挂同 turn（即使 sampled=false） |
| **`LANGFUSE_BATCH_MAX_EVENTS`** | `50` | 保留现有，改为可配置 |
| **`LANGFUSE_BATCH_FLUSH_INTERVAL`** | `10` | 保留现有，改为可配置 |

新增 4 个，原有 3 个保留（`LANGFUSE_BATCH_BACKPRESSURE` 未暴露为环境变量，见下方注释）。**全部支持** `~/.peri/settings.json` 中 `langfuse.*` 字段。

> **注意**：`LANGFUSE_BATCH_BACKPRESSURE`（`drop_new` / `block` / `drop_oldest`）在 langfuse-client 的 `ClientConfig` 中存在内部默认值 `DropNew`，但当前不从环境变量读取，也不在 peri-acp 的 `LangfuseConfig` 中暴露。如需自定义背压策略，需修改 langfuse-client 源码。

### 4.2 SamplingDecider 算法

```rust
pub(crate) struct SamplingDecider {
    rate: f64,
    decided: HashMap<String, bool>,  // turn_id -> decision
}

impl SamplingDecider {
    pub(crate) fn should_emit(&mut self, turn_id: &str, session_id: &str) -> bool {
        if let Some(d) = self.decided.get(turn_id) { return *d; }

        let h = hash(turn_id, session_id);
        let decision = (h % 10_000) as f64 / 10_000.0 < self.rate;
        self.decided.insert(turn_id.to_string(), decision);
        decision
    }

    /// turn_end 时调用，防止 HashMap 无限增长。
    pub(crate) fn cleanup_turn(&mut self, turn_id: &str) {
        self.decided.remove(turn_id);
    }
}
```

**关键性质**：

- 同一 turn 内多次调用 `should_emit` 返回一致（HashMap 缓存）。
- decision 在 turn_end 时从 HashMap 移除（防内存增长）。
- `decided.len() > 1000` 时清理最旧条目（兜底，防止异常情况下 HashMap 爆炸）。
- 纯 hash + rate，无场景判断。

### 4.3 错误 turn 的 ErrorSpan（挂同 turn）

```rust
// 在 on_turn_end 内：
if status.is_error() && env_error_span_always {
    if !was_sampled {
        // 未采样 turn：补发 TraceCreate（用 turn_id 作 trace_id）+ ErrorSpan
        // trace_id == turn_id 契约仍 satisfied
        let trace_event = build_synthetic_trace_create(turn_id);
        session.try_add(trace_event);
    }
    // 无论 sampled 与否，都追加 ErrorSpan（已采样 turn 的 trace 已存在，
    // ErrorSpan 挂在 trace 下作为最后一个子 span）
    let error_span = build_error_span(turn_id, error_kind, stacktrace);
    session.try_add(error_span);
}
```

**两种情形**：

- **sampled turn 出错**：trace 已存在（含完整子事件），追加 ErrorSpan 作为最后一个子 span，metadata.is_synthetic=false（trace 是真实的）。
- **unsampled turn 出错**：trace 不存在，先补发 synthetic TraceCreate（metadata.synthetic_error=true），再追加 ErrorSpan。trace 内仅有 ErrorSpan 一个子事件。

`ErrorSpan` 是事后构造的最小 span，**不补全**未采样的子事件。UI 上能看到「这个会话发生 N 次错误，错误类型分布」，但具体子事件仍需调高 sampling 才能看。

**契约**：

- trace_id = turn_id（不破坏契约）
- TraceCreate（仅 unsampled 时补发）metadata 含 `synthetic_error: true` 标识
- ErrorSpan metadata 含 `error_kind`（Interrupted/Timeout/LlmFailure/ToolFailure/RateLimit/MaxIterations）、`stacktrace`、`turn_started_at`、`turn_ended_at`、`was_sampled`

### 4.4 配置加载与降级

`peri-acp/src/langfuse/config.rs` 扩充：

- 解析所有新增环境变量
- 解析失败时按以下优先级 fallback：
  1. 显式 env 变量
  2. `~/.peri/settings.json` 中 `langfuse.*` 字段
  3. 默认值
- 全部解析后构造 `LangfuseConfig` struct（字段 `pub`，支持 `Clone`）
- Tracer 持有 `config: LangfuseConfig`（值，非 Arc）

`from_env()` 返回 None 的情况不变：仅当 `LANGFUSE_PUBLIC_KEY` 或 `LANGFUSE_SECRET_KEY` 缺失。

---

## 5. 测试策略

### 5.1 子状态机独立单测（新增）

每个子对象在自己的 `_test.rs` 内测，**不依赖** LangfuseSession/batcher：

| 测试模块 | 测什么 | 预估 LOC |
|---------|--------|---------|
| `sampling_test.rs` | hash 一致性、rate=0/1.0 边界、cleanup_turn 清理 | ~60 |
| `stages_test.rs` | 5 阶段生命周期、嵌套禁止、重复 end 早返回、Compact 阈值以下不上报、MQ 排空计数（仅 Receive 阶段）、Workflow start/end（仅 Act 阶段） | ~150 |
| `middleware_test.rs` | 15+5 链按 hook 分组、并行同 hook 顺序、status 传递 | ~70 |
| `generation_test.rs` | 现有 6 个 test 迁入（review 02 §7.1） | ~80 |
| `tool_batch_test.rs` | lazy 创建/flush/is_agent_tool（review 02） | ~60 |
| `subagent_test.rs` | 嵌套栈/current_agent_id（review 02） | ~200 |
| `compact_test.rs` | on/off + 三级策略 metadata + token_before/after | ~60 |

**新增约 680 行子对象单测**，独立可跑、跑得快。MQ 与 Workflow 测试合并入 `stages_test.rs`（与子对象合并方案对齐）。

### 5.2 forward_langfuse_event 集成测试

扩充 `peri-acp/src/langfuse/tracer_test.rs`（重构后变成集成层冒烟）：

- 用 `FakeLangfuseSession`（见 §3.5）记录所有 `try_add` 调用
- 21 个 on_* 方法每个 1 个冒烟 test
- 重点测**子对象协作**：如 `on_stage_start(Reason) → on_llm_start → on_ai_reasoning_chunk → on_llm_end → on_stage_end(Reason)` 序列生成的事件树结构正确（parent 关系、顺序）

**`tracer_test.rs` 现有 356 LOC 的处理**：按 review 02 §7.2 的迁移方案——19 处字段白盒访问改读子对象公开方法返回值，test 按归属拆分到 7 个子对象 `_test.rs`，集成层保留约 80 行冒烟用例。

### 5.3 端到端冒烟测试

`peri-acp/tests/langfuse_e2e.rs`（新增 crate-level 集成测试）：

- 用真实 LangfuseSession，但指向 `mockito` mock server
- 构造一个完整 turn：SessionStarted → TurnStarted → CompactStarted/skip → Receive → Reason (LLM) → Act (Tool) → End
- 断言 mock server 收到的 OTLP 请求体含正确的 Session/Trace/5 阶段 Span/Generation/Observation 层级
- sampling=0.0 时断言无事件（除 ErrorSpan 路径）
- sampling=1.0 时断言全部事件
- 错误 turn 断言 ErrorSpan 挂同 turn

### 5.4 P0 测试覆盖矩阵（按盲区）

| 盲区 | 测试 | 优先级 |
|------|------|--------|
| trace_id == turn_id | `test_trace_id_equals_turn_id` | P0 |
| 5 阶段 Span 生命周期 | `test_stage_spans_lifecycle` | P0 |
| 中间件链分组 | `test_middleware_grouped_by_hook` | P0 |
| ContextBudget 阈值点 | `test_budget_threshold_hit_metadata` | P0 |
| Compact 三级策略 metadata | `test_compact_strategy_metadata` | P0 |
| MQ 排空计数 | `test_mq_drained_counts` | P0 |
| AiReasoning chunk 挂 Reason | `test_ai_reasoning_under_reason_span` | P0 |
| Workflow start/end | `test_workflow_span` | P1 |
| Sampling rate=0/1.0 | `test_sampling_boundary` | P0 |
| Sampling 一致性 | `test_sampling_consistent_within_turn` | P0 |
| Sampling cleanup_turn | `test_sampling_cleanup_prevents_growth` | P0 |
| 错误 ErrorSpan 挂同 turn（unsampled） | `test_error_span_uses_turn_id_as_trace_id` | P0 |
| sampled=true 时 ErrorSpan 也追加 | `test_error_span_appended_when_sampled` | P0 |
| Session 聚合 | `test_session_object_traces_under` | P1 |
| SubAgent 嵌套 | （现有 test 迁入 subagent_test.rs） | P1 |
| 事件父子顺序（review 02 §3.1） | `test_event_ordering_parent_before_child` | P0 |
| ExecutorEvent 变体覆盖 | `variant_coverage_test.rs` | P0 |

### 5.5 测试规范遵循

遵循 `docs/design/testing-standards.md`：

- 单元测试 ≥ 30 行用 `_test.rs`，< 30 行用 `#[cfg(test)] mod tests`
- 命名 `test_<对象>_<场景>`，三段 Arrange-Act-Assert 段间无空行
- 错误路径 ≥ 1 条，断言错误**消息内容**而非仅 `is_err()`
- 异步测试用 `#[tokio::test]`
- 全局状态测试用 `#[serial]`
- `make_` 前缀工厂函数，禁止 `mockall`

---

## 6. 迁移、回滚、ADR

### 6.1 一次性 PR 的 commit 序列

方案 B = 单个大 PR，但 git 历史仍分 commit：

1. **commit 1**：langfuse-client crate（加 Session/ScoreBody 等数据结构、config 扩展、BackpressurePolicy 可配置）
2. **commit 2**：peri-agent ExecutorEvent 新变体（12 新 + 2 扩充）+ event/mapper.rs + acp_events.rs + `variant_coverage_test.rs`
3. **commit 3**：peri-agent/stages/* 实际 emit 新事件（Compact/Receive/Reason/Act/End 阶段、middleware/chain.rs）
4. **commit 4**：LangfuseTracer 内部重构（7 子对象 + `LangfuseSessionLike` trait 抽取）+ 21 on_* 方法（含 `on_trace_*` 重命名为 `on_turn_*`）
5. **commit 5**：`forward_langfuse_event` 扩展路由 + `workflow_agent.rs:142-504` 改挂主 Trace Act Span 下
6. **commit 6**：Sampling + ErrorSpan + 配置加载 + `~/.peri/settings.json` 支持
7. **commit 7**：测试（680 行子对象单测 + 集成 + e2e + mapper_test 同步）
8. **commit 8**：删除旧 tracer 文件、文档更新（CLAUDE.md + ADR + langfuse-monitoring-v2.md）

每个 commit 独立可编译、独立 `cargo test`。整体作为一个 PR 提交。

### 6.2 回滚

- 单 PR `git revert` 即整体回滚
- 中途发现严重问题：revert PR 即可，不影响 main 分支（旧 tracer 仍在）
- 数据风险：langfuse 后端已收到的旧 trace（旧 schema）和新 trace（新 schema）共存，UI 上旧 trace 仍可读，不会丢失历史数据

### 6.3 关键风险

| 风险 | 缓解 |
|------|------|
| ExecutorEvent 新变体忘记同步 mapper/acp_events | `variant_coverage_test.rs`：枚举所有变体，断言每个在 mapper_test 有对应 test |
| 7 子对象的 `&mut self` 借用冲突 | `ToolBatchRef` 枚举 / disjoint borrow；CI 加 compile-only check |
| SamplingDecider HashMap 内存增长（长会话） | turn_end 时 remove；`decided.len() > 1000` 时清理最旧条目 |
| 错误 turn 信息少（仅 ErrorSpan，无子事件） | 文档说明：要看错误 turn 全貌需调高 sampling |
| WorkflowAgent 独立 tracer pump 的 trace 不挂主 trace | workflow_agent.rs:142-504 改为挂主 Trace 的 Act Span 下，不创建独立 trace |
| trace_id == turn_id 破坏现有持久化 | 旧 trace 用旧 ID，新 trace 用 turn_id；langfuse 后端不冲突 |
| trace_id = turn_id 契约生产路径未执行 | `executor_helpers.rs:197` 调用 `new()` 而非 `new_with_turn_id()`，trace_id 仍为独立 UUID v7。需改用 `new_with_turn_id` 并传入 turn_id（见 §1.3 ⚠️ 注） |
| 借用冲突（多个子对象同时 mut） | 禁止子对象方法签名接收 `&mut LangfuseTracer`，CI 加 grep check |
| `on_trace_*` 重命名漏改调用点 | 全文 search "on_trace_start" / "on_trace_end"，CI 加 grep check |

### 6.4 ADR

建议写 `ADR-2026-07-15-langfuse-architecture-revamp`：

- **Context**：监控盲区 + 14 字段散 + turn_id 脱节 + 无 Sampling
- **Decision**：方案 B 一次性重构 + 7 子状态机 + Session 对象 + trace_id = turn_id + Turn 级 Sampling + ErrorSpan 兜底
- **Alternatives**：
  - 方案 A 分阶段（被否决：残留中间状态、用户明确要求激进）
  - 方案 C 最小补丁（被否决：盲区仍在）
  - 引入预判机制（被否决：用户拒绝，简化为纯 hash + rate）
  - 错误 turn 独立 ErrorTrace（被否决：破坏 trace_id 契约，改用同 turn 挂 ErrorSpan）
- **Consequences**：单大 PR；旧 trace schema 与新 schema 共存；后续 langfuse 改动收敛在子对象层
- **Compliance**：14 个 P0 测试全过 + e2e mock 验证 + variant_coverage_test 全过

### 6.5 文档更新

- `CLAUDE.md` 「任务入口矩阵」加 langfuse 重设计行
- `CLAUDE.md` 「陷阱速查」加：sampled=false 时 tracer silently no-op；新增 ExecutorEvent 必须扩 mapper_test 与 variant_coverage_test
- `docs/design/langfuse-monitoring-v2.md`（本设计文档归档到此位置）
- `docs/architecture-reviews/` 加 ADR

---

## 附录 A：与架构 review 02 候选的关系

本设计**完整包含**架构 review 02 候选 2（`docs/architecture-reviews/2026-07-13-peri-acp/02-langfuse-tracer-state-encapsulation.md`）：

- 4 个子状态机（GenerationTracker / ToolBatch / SubagentStack / CompactSpan）的接口设计完整复用 review 02 §5.2
- `LangfuseSessionLike` trait 抽取（review 02 候选 06）作为本设计 §3.5 落地
- 测试迁移策略复用 review 02 §7.2
- 不变量升级对照表复用 review 02 §3

本设计在 review 02 基础上**追加**：3 个新子状态机（SamplingDecider / StageSpans（含 MQ 排空 + Workflow 子能力）/ MiddlewareTracer）、Langfuse Session 对象引入、trace_id = turn_id 契约、Sampling 与 ErrorSpan 机制。

---

## 附录 B：grilling 决策记录

- **Q：为什么方案 B 而非方案 A 分阶段？** 用户明确要求"激进、一次性规划好，不残留失败设计"。分阶段方案在 P2-P3 之间会有"5 阶段 Span 已建立但中间件未挂"的中间状态，UI 上能看出半成品。
- **Q：为什么 trace_id = turn_id 而非保留独立生成？** 架构文档 §2.6 明确要求 turn_id 作为"统一纽带"。当前 tracer 自生成 trace_id 与 turn_id 脱节，破坏了架构契约。
- **Q：为什么 Langfuse Session 对象保留？** v4 原生支持，UI 上能看到会话级聚合（多 turn 平均延迟、错误率、采样命中数）。零额外后端成本。
- **Q：为什么 5 阶段条件上报而非占位？** Compact 阈值以下时占位 0ms Span 在 UI 上是噪声，干扰阅读。用户明确选择"0ms 则不上报"。
- **Q：为什么 MCP 不另设变体？** MCP 工具调用走现有 ToolStart/End 通道，工具 name 已能区分，无需额外标记。用户明确选择"用 tool 兼容，不用什么字段"。
- **Q：为什么去掉预判机制？** 用户明确拒绝"先不要这个"。预判逻辑复杂、覆盖不全（如纯 LLM 出错无法预判），简化为纯 hash + rate 后行为更可预测。
- **Q：为什么错误用 ErrorSpan 挂同 turn 而非独立 ErrorTrace？** 独立 ErrorTrace 破坏 trace_id == turn_id 契约（需 `format!("{turn_id}-error")`）。挂同 turn 时 trace_id 不变，仅追加 1 个 Span，metadata.is_synthetic=true 标识。用户明确选择"挂在同一个 turn 下面作为 span"。
- **Q：为什么 tracer 完全被动？** 用户明确选择"tracer 被动接受"。反向信息流（返回 SampleDecision）会让 peri-acp 感知 tracer 内部决定，破坏层级。
- **Q：为什么 WorkflowAgent 改挂主 Trace？** 当前 WorkflowAgent 独立 tracer pump 创建独立 trace，父子关系在 UI 不可见。改挂主 Trace Act Span 下后，主 Trace 能看到 Workflow 的 SubAgent、tool call 全貌。
- **Q：为什么所有 5 个新增环境变量都支持 settings.json？** 用户明确选择"都做 settings.json 支持"。统一配置入口，减少 env 变量散落。
