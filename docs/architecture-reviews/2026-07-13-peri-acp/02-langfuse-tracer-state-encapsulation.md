# 候选 2：LangfuseTracer 14 字段状态机封装成内聚对象

> 日期：2026-07-13 | 模块：`peri-acp/src/langfuse/tracer/` | 类型：架构走读
> 流程：/grilling（locality 碎片 + 缺失的 seam）
> 范围：1 个 struct 持有 14 个 `pub(crate)` 可变字段，被 6 个 handler 文件通过 `self.field` 散乱读写；facade `mod.rs` 只有 struct 定义和 `new()`，状态机不变量没有物理聚合点。

---

## 1. 摘要

`LangfuseTracer`（`peri-acp/src/langfuse/tracer/mod.rs:43-79`）是 per-turn 的 Langfuse 追踪器，持有 **14 个 `pub(crate)` 可变字段**，分散在 6 个 handler 文件（`llm_handler.rs` / `tool_handler.rs` / `compact_handler.rs` / `trace_lifecycle.rs` / `subagent_stack.rs` + 基础设施 `event_builder.rs` / `context.rs` / `usage.rs`）中通过 `self.field` 直接读写。每个 handler 文件都被迫懂**全部字段的不变量**——例如 `active_step` 必须在 `on_llm_start` 时设置、在 `on_llm_end` 时清空；`tools_batch_span_id` 必须先于子 span 入队、由 `flush_tools_batch` 在 `on_llm_start` / `on_trace_end` / `end_subagent` 三处消费；`pending_tools` 必须按 `tool_call_id` 在 start 时插入、在 end 时取出，且 Agent 工具的 PendingTool 落在**父级** context（`tool_handler.rs:60-68` 的 pop-before-lookup 顺序约束）。

这是典型的 **locality 碎片化**：状态机的「数据」聚在 struct 定义里，状态机的「行为」却散在 6 个 `impl LangfuseTracer` 块里；新增字段时所有 handler 都要逐个核对不变量，新增 handler 时所有字段都要重新审视。本候选走 /grilling 流程拷问加深方向：**把 14 字段按内聚类收成 4 个子状态机对象**（`GenerationTracker` / `ToolBatch` / `SubagentStack` / `CompactSpan`），handler 文件降级为薄转发层。结论是推荐该方向——它把「不变量」从注释（`mod.rs:49` `[不变量] trace_id ...`）升级为类型系统（私有字段 + 窄接口），把「handler 必须懂字段」反转为「字段只通过对象方法暴露给 handler」，leverage 从 6×14 的散点改为 4×3 左右的窄面。

---

## 2. 现状诊断

### 2.1 14 字段的具体证据

`peri-acp/src/langfuse/tracer/mod.rs:43-79`：

```rust
pub struct LangfuseTracer {
    pub(crate) session: std::sync::Arc<LangfuseSession>,         // 1
    pub(crate) session_id: String,                                // 2
    pub(crate) trace_id: String,                                  // 3
    pub(crate) agent_observation_id: String,                      // 4
    pub(crate) generation_data: HashMap<usize, GenerationCached>, // 5
    pub(crate) pending_tools: HashMap<String, PendingTool>,       // 6
    pub(crate) tools_batch_span_id: Option<String>,               // 7
    pub(crate) tools_batch_start_time: Option<String>,            // 8
    pub(crate) tools_batch_end_time: Option<String>,              // 9
    pub(crate) final_answer: String,                              // 10
    pub(crate) subagent_stack: Vec<SubAgentContext>,              // 11
    pub(crate) compact_span: Option<CompactSpanContext>,          // 12
    pub(crate) active_step: Option<usize>,                        // 13
    pub(crate) retry_attempts: Vec<RetryAttempt>,                 // 14
}
```

14 个字段全部 `pub(crate)`，意味着：

- **测试文件 `tracer_test.rs` 直接读写字段**（`tracer.retry_attempts.is_empty()` L257、`tracer.subagent_stack[0].pending_tools.len()` L110、`tracer.compact_span.is_some()` L219）——共 19 处字段访问。
- **6 个 handler 文件全部通过 `self.field` 读写**——下面交叉表是逐行 grep 的结果。

facade `mod.rs` 的 `impl LangfuseTracer` 只有 `new()`（L83-100），**真正状态机逻辑零行**。状态机定义在哪儿？答案：分散在 6 个文件的注释和实现里。

### 2.2 handler × 字段交叉访问表

| 字段 \ handler file                                | llm_handler (135 LOC) | tool_handler (131 LOC) | compact_handler (139 LOC) | trace_lifecycle (101 LOC) | subagent_stack (217 LOC) | context.rs (72, 数据) | event_builder (43, 工具) | usage (61, 纯函数) |
|----------------------------------------------------|----------------------:|----------------------:|--------------------------:|-------------------------:|-------------------------:|---------------------:|------------------------:|-------------------:|
| `session` (batcher)                                | R                     | R                     | R                         | R                        | R                        | -                    | -                       | -                  |
| `session_id`                                       | R                     | R                     | R                         | -                        | R                        | -                    | -                       | -                  |
| `trace_id`                                         | R                     | R                     | R                         | R                        | R                        | -                    | -                       | -                  |
| `agent_observation_id`                             | -                     | -                     | -                         | R                        | R                        | -                    | -                       | -                  |
| `generation_data`                                  | **RW**                | -                     | -                         | -                        | -                        | -                    | -                       | -                  |
| `pending_tools`                                    | -                     | **RW**                | -                         | -                        | **RW**（via context）   | -                    | -                       | -                  |
| `tools_batch_span_id`                              | -                     | **RW**（via ctx）     | -                         | -                        | **RW**（via ctx）        | -                    | -                       | -                  |
| `tools_batch_start_time`                           | -                     | **RW**（via ctx）     | -                         | -                        | **RW**（via ctx）        | -                    | -                       | -                  |
| `tools_batch_end_time`                             | -                     | **RW**（via ctx）     | -                         | -                        | **RW**（via ctx）        | -                    | -                       | -                  |
| `final_answer`                                     | -                     | W（on_text_chunk）    | -                         | R + take（on_trace_end） | -                        | -                    | -                       | -                  |
| `subagent_stack`                                   | -                     | R（is_agent_tool）    | -                         | -                        | **RW**                  | -                    | -                       | -                  |
| `compact_span`                                     | -                     | -                     | **RW**                    | -                        | -                        | -                    | -                       | -                  |
| `active_step`                                      | **RW**                | -                     | -                         | -                        | -                        | -                    | -                       | -                  |
| `retry_attempts`                                   | **RW**                | -                     | -                         | -                        | -                        | -                    | -                       | R（build_retry_metadata） |

读法：

- **R** = 只读访问；**W** = 只写；**RW** = 同时读写（典型状态机字段）；**via ctx** = 通过 `current_tools_context()` 返回的 4 元组间接读写，主 struct 和栈顶 SubAgentContext 都可能被命中。
- 6 个 handler 中**每个平均访问 5.0 个全局字段**（`llm_handler` 8、`tool_handler` 9、`compact_handler` 5、`trace_lifecycle` 5、`subagent_stack` 11）。
- **没有任何一个字段是"只被一个 handler 访问"**：连看起来最内聚的 `compact_span` 也跨 `compact_handler`（RW）和 `tracer_test.rs`（assert）；`pending_tools` 同时被 `tool_handler` 和 `subagent_stack` 通过 `current_tools_context()` 间接读写——**两条写入路径必须保持语义一致**（start 时插父级、end 时也查父级），但这一约束**只写在代码注释里**（`tool_handler.rs:60-68` 的 `[TRAP]`），类型系统完全不设防。

### 2.3 不变量散落在注释里的证据

| 不变量 | 注释位置 | 类型系统保护？ |
|--------|---------|---------------|
| `trace_id` 在 new() 一次性生成，禁止重新生成 | `mod.rs:48-51` | 否（字段 `pub(crate)`，handler 可写） |
| `try_add` 同步入队，父 span 先于子 span | `mod.rs:23-24`、`event_builder.rs:24-28` | 否 |
| `active_step` 与 `generation_data` 的关系（start 设、end 清） | `mod.rs:75-76` 注释 | 否（两字段独立可见） |
| Agent 工具的 PendingTool 落在**父级** context，end 时必须先 `end_subagent` 再查 PendingTool | `tool_handler.rs:60-68` `[TRAP]` | 否 |
| `end_subagent` 必须在 `pop()` 前调 `flush_tools_batch()` | `subagent_stack.rs:118-120` `[TRAP]` | 否 |
| `on_llm_start` 必须清空 `retry_attempts` | `llm_handler.rs:36` | 否 |
| `tools_batch_span_id` 三处消费点（`on_llm_start` / `on_trace_end` / `end_subagent`） | 隐式（grep 才能找全） | 否 |

7 条核心不变量全部"注释保护"——这是 locality 碎片化的标准症状。

### 2.4 量化症状

- **代码行**：mod.rs 105 + llm_handler 135 + tool_handler 131 + compact_handler 139 + trace_lifecycle 101 + subagent_stack 217 + context 72 + event_builder 43 + usage 61 = **1004 行**（不含 test）。
- **测试行**：`tracer_test.rs` 356 行，19 处字段白盒访问。
- **`pub(crate)` 字段占比**：14/14 = 100%（无任何私有字段，意味着封装边界为零）。
- **跨文件不变量密度**：平均每个 handler 文件携带 1.2 条 `[TRAP]` / `[不变量]` 注释，描述的不是本文件局部逻辑，而是**跨文件顺序约束**。

---

## 3. 约束

任何重构方案必须保留以下契约，否则就是回滚候选。

### 3.1 Langfuse 事件顺序契约（不可协商）

> 父 span 必须先于子 span 入队。

证据：`event_builder.rs:24-28` 的 `[不变量]`；`subagent_stack.rs:118-120` `[TRAP]` 强调 `end_subagent` 中 `ObservationCreate` 必须先入队、再 `flush_tools_batch` 把 Tools SpanCreate 入队（Tools 的 parent 是 subagent observation），最后才能 `pop()`。颠倒任一步骤 → Langfuse 后端把 Tools 悬挂到错误的 parent → 重复 trace。

含义：**所有"开始事件"必须同步入队、所有"提交批次"必须晚于子 span 的入队**。任何"先 buffer、最后批量入队"的优化方向都违反此契约，除非能证明 buffer 顺序在单线程内严格保留。

### 3.2 `trace_id` 一次性生成不变量

`mod.rs:48-51`：trace_id 在 `new()` 时一次性生成（`uuid::Uuid::now_v7()`），整个 turn 内所有事件共享，**禁止重新生成**。重新生成 → Langfuse 后端把这 turn 拆成多个 trace → 历史回放断裂。

含义：trace_id 必须是**构造期固定、运行期只读**。封装后任何 `set_trace_id()` 接口都是 bug。

### 3.3 SubAgent 嵌套语义（栈式）

`subagent_stack.rs:36-43`：`current_agent_id()` 返回栈顶 SubAgent 的 observation_id，否则返回主 agent。SubAgent 可嵌套（`tracer_test.rs:122` test_nested_subagent_stack_depth 验证 depth=2 场景）。

含义：

- "当前 agent" 是**运行期栈顶**语义，不是字段。
- `pending_tools` 和 `tools_batch_*` 三元组**每层 SubAgent 一份**（`SubAgentContext` 内嵌这三字段，`context.rs:51-55`），主 struct 持有的是 main agent 层。
- `current_tools_context()`（`subagent_stack.rs:46-69`）按栈顶路由到 SubAgentContext 还是主 struct——**这是关键字段的双路径写入**，封装时必须连同 SubAgentContext 一起收敛。

### 3.4 性能：try_add 同步入队

`event_builder.rs:24-28`：所有事件走 `batcher.try_add()` **同步**入队（背压时 DropNew + warn）。`on_trace_end` 的 flush 是 Tracer 唯一 async 路径（`trace_lifecycle.rs:61` `[不变量]`）。

含义：

- 加一层方法调用（如 `self.generation.on_llm_start(...)`）的开销是 ns 级，相对 `try_add` 的 HashMap insert + Vec push 完全可忽略——**性能不是阻塞因素**。
- 但**禁止把 try_add 改成 async / channel**，会破坏事件顺序契约。

### 3.5 `executor_helpers.rs:189` 的 `parking_lot::Mutex<LangfuseTracer>` 外壳

`executor_helpers.rs:189-191`：tracer 在 pump task 中以 `parking_lot::Mutex<LangfuseTracer>` 形式存在，每个 `forward_langfuse_event` 调用都是 `tracer.lock().on_xxx(...)`。封装后的子对象**不能引入额外的锁**——必须仍是 `&mut self` 同步方法。

### 3.6 测试白盒访问

`tracer_test.rs` 19 处字段访问（如 `tracer.retry_attempts.is_empty()`、`tracer.subagent_stack[0].pending_tools.len()`）。封装后这些测试要么改读公开 getter，要么改调用公开方法返回的状态。**禁止保留 `pub(crate)` 字段只是为了测试不重写**——那等于没封装。

---

## 4. 依赖关系

### 4.1 前置：候选 1（ExecutorEvent visitor）

候选 1 把 `forward_langfuse_event`（`executor_helpers.rs:273-353`）的 9 路 match 收敛到 visitor trait。**候选 2 不强依赖候选 1**——可以直接在现有 `on_*` 方法签名上做封装。但候选 1 落地后，handler 输入边界（`ExecutorEvent` → 单个 `on_xxx` 调用）会更清晰，候选 2 重构时的回归风险更低。

建议：**候选 1 先做、候选 2 后做**，但不强制阻塞。

### 4.2 后置：候选 6（testability trait 抽取）

候选 6 计划把 `LangfuseTracer` 抽成 trait（如 `Tracer`），让 `executor_helpers.rs:189` 可注入 fake tracer 跑单测。**候选 2 是候选 6 的前置**：在 14 字段都 `pub(crate)` 的现状下抽 trait，trait 方法必然暴露内部状态（要么 trait 巨胖、要么测试继续白盒），trait 抽取毫无 leverage。先把状态封进 4 个子对象、把 `LangfuseTracer` 的公开方法收敛到 ~10 个 `on_*`，候选 6 才有干净的 trait surface 可抽。

### 4.3 平行

无。本候选自包含，不阻塞其他候选。

---

## 5. 加深后的模块形状

### 5.1 子状态机划分原则

把 14 字段按下表分桶：

| 子对象 | 持有字段 | 内聚类理由 |
|--------|---------|-----------|
| `GenerationTracker` | `generation_data` + `active_step` + `retry_attempts` | 三字段共同表达"当前 LLM step 的生命周期"：start 设 active_step + 清 retry + 缓存、retry 累积、end 消费三者 |
| `ToolBatch` | `pending_tools` + `tools_batch_span_id` + `tools_batch_start_time` + `tools_batch_end_time` | 四字段共同表达"当前批次的工具组 span"：start 时插入 PendingTool 并 lazy 创建 batch span、end 时移除、flush 时提交 span |
| `SubagentStack` | `subagent_stack`（Vec） | 已存在但接口未收敛（`begin_subagent` / `end_subagent` / `current_agent_id` / `current_tools_context` / `is_agent_tool` / `flush_tools_batch` 6 个方法全在 `LangfuseTracer` 上）。封装后**ToolBatch 与 SubagentStack 协作**——栈顶路由决定了写入哪一层的 ToolBatch |
| `CompactSpan` | `compact_span`（Option） | 单字段，但 on/off 状态机语义清晰（start 设、end 取） |

主 struct 保留：

| 字段 | 理由 |
|------|------|
| `session` | 共享 client/batcher，所有 try_add 入口 |
| `session_id` | 构造期固定 |
| `trace_id` | 构造期固定，**必须只读** |
| `agent_observation_id` | 构造期固定，**必须只读** |
| `final_answer` | 单字段累积器，跨 `on_text_chunk` / `on_trace_end` |

**5 个字段 + 4 个子对象 = 9 个字段**（从 14 降到 9，且 4 个子对象字段全部私有）。

### 5.2 4 个子状态机的 Rust interface 草案

#### 5.2.1 `GenerationTracker`

```rust
/// 单个 LLM step 的生命周期：on_llm_start → on_llm_request_payload → on_llm_retrying* → on_llm_end。
///
/// [不变量] active_step 与 generation_data[active_step] 同步存在；
/// retry_attempts 在 on_llm_start 清空、在 on_llm_end 消费后清空。
pub(crate) struct GenerationTracker {
    generation_data: HashMap<usize, GenerationCached>,
    active_step: Option<usize>,
    retry_attempts: Vec<RetryAttempt>,
}

impl GenerationTracker {
    pub(crate) fn new() -> Self { /* 全空 */ }

    /// on_llm_start：设置 active_step、清空 retry、缓存 input。
    /// 返回 `(gen_id, start_time)` 供 caller 构造 GenerationCreate（
    /// 或返回 owned `GenerationStart` 事件数据）。
    pub(crate) fn on_llm_start(
        &mut self,
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    ) -> GenerationStart;

    /// on_llm_request_payload：补充 Provider 实际请求体。
    /// 未先 on_llm_start 时静默 no-op（保留现有行为，tracer_test L317 验证）。
    pub(crate) fn on_llm_request_payload(
        &mut self,
        step: usize,
        body: Arc<serde_json::Value>,
    );

    /// on_llm_retrying：累积重试记录。
    pub(crate) fn on_llm_retrying(&mut self, attempt: usize, max_attempts: usize, delay_ms: u64, error: &str);

    /// on_llm_end：消费 generation_data[step]，返回构造 GenerationCreate 所需的
    /// 全部数据（input_json + gen_id + start_time + retry_metadata）。
    /// 同时清空 active_step 和 retry_attempts。
    /// 未找到 step 时返回 None（保留现有早返回行为，llm_handler.rs:60-62）。
    pub(crate) fn on_llm_end(&mut self, step: usize) -> Option<GenerationEnd>;
}

pub(crate) struct GenerationStart {
    pub gen_id: String,
    pub start_time: String,
}

pub(crate) struct GenerationEnd {
    pub gen_id: String,
    pub start_time: String,
    pub input_json: serde_json::Value, // 已选好 raw_body 或 fallback
    pub retry_metadata: Option<serde_json::Value>,
}
```

**不变量升级**：active_step 与 generation_data 的同步关系从注释（`mod.rs:75-76`）升级为私有字段（外部只能通过 `on_llm_start` / `on_llm_end` 操作）。retry_attempts 的清空时机也从注释（`llm_handler.rs:36`）升级为方法副作用。

#### 5.2.2 `ToolBatch`

```rust
/// 当前批次工具组 span：on_tool_start ×N → flush → 重新分配 batch span id。
///
/// [不变量] tools_batch_span_id 的生命周期：
/// - 懒创建：第一个 on_tool_start 触发
/// - 累积：后续 on_tool_start 共享同一 batch span
/// - flush：on_llm_start / on_trace_end / end_subagent 三处消费
pub(crate) struct ToolBatch {
    pending_tools: HashMap<String, PendingTool>,
    batch_span_id: Option<String>,
    batch_start_time: Option<String>,
    batch_end_time: Option<String>,
}

impl ToolBatch {
    pub(crate) fn new() -> Self { /* 全空 */ }

    /// on_tool_start：lazy 创建 batch span（如未存在），插入 PendingTool。
    /// 返回 (parent_span_id, tool_span_id, tool_start_time) 供 caller 构造
    /// ObservationCreate 事件（parent = batch_span_id 或 agent_id）。
    pub(crate) fn on_tool_start(
        &mut self,
        tool_call_id: &str,
        name: &str,
        input: serde_json::Value,
    ) -> ToolStartRecord;

    /// on_tool_end：取出 PendingTool。返回 None 时 caller 早返回（tool_handler.rs:71-73）。
    pub(crate) fn on_tool_end(&mut self, tool_call_id: &str) -> Option<PendingTool>;

    /// 标记 batch 最后一次 ToolEnd 时间（on_tool_end 末尾调用）。
    pub(crate) fn record_end_time(&mut self, end_time: String);

    /// flush：取出三元组并清空。返回 None 表示无待提交批次。
    pub(crate) fn flush(&mut self) -> Option<ToolsBatchRecord>;

    /// 查询：on_tool_start/end 共享，判断是否为 Agent 工具。
    /// 封装 subagent_stack.rs:23-34 的双 HashMap 查找逻辑（
    /// 但 SubAgent 层的查找由 SubagentStack 委托，见下）。
    pub(crate) fn is_agent_tool(&self, tool_call_id: &str) -> bool;

    pub(crate) fn is_empty(&self) -> bool { self.pending_tools.is_empty() }
}

pub(crate) struct ToolStartRecord {
    pub tool_span_id: String,
    pub tool_start_time: String,
    pub parent_span_id: String, // batch_span_id 或 agent_id（lazy 创建时）
}

pub(crate) struct ToolsBatchRecord {
    pub batch_span_id: String,
    pub batch_start_time: String,
    pub batch_end_time: String,
}
```

**关键决策**：`is_agent_tool` 同时查 main 层 `ToolBatch` 和 `SubagentStack` 内嵌的各层 `ToolBatch`——这迫使 `is_agent_tool` **必须由 LangfuseTracer 主 struct 协调**（委托给 SubagentStack 查栈内 + 本层 ToolBatch）。下面 SubagentStack interface 体现这点。

#### 5.2.3 `SubagentStack`

```rust
/// SubAgent 嵌套栈。每层 SubAgentContext 自带一个 ToolBatch（
/// 替代 context.rs:51-55 的内嵌字段散点）。
pub(crate) struct SubagentStack {
    stack: Vec<SubAgentContext>,
}

/// 单层 SubAgent 上下文（封装后字段全私有，仅通过方法暴露）。
pub(crate) struct SubAgentContext {
    observation_id: String,
    agent_id: String,
    start_time: String,
    input: serde_json::Value,
    tool_batch: ToolBatch, // 替代散落的 4 个字段
}

impl SubagentStack {
    pub(crate) fn new() -> Self { Self { stack: Vec::new() } }

    /// 当前活动 agent observation id（栈顶 or 主 agent）。
    /// 主 agent id 由 caller 传入（fallback）。
    pub(crate) fn current_agent_id(&self, fallback_main: &str) -> String;

    /// 当前活动 ToolBatch 的 &mut 引用（栈顶 or 主层）。
    /// 关键：替代 subagent_stack.rs:46-69 的 4 元组返回，借用更窄。
    /// 调用方（主 struct）持有 self.subagent.current_tool_batch(&mut fallback_tool_batch)
    /// 或类似双路由方法。
    pub(crate) fn current_tool_batch_mut(&mut self) -> ToolBatchRef<'_>;

    /// 是否为 Agent 工具：在栈内 + 主层 ToolBatch 一起查。
    /// 由主 struct 协调（main_tool_batch: &ToolBatch, sub_stack: &SubagentStack）→ bool。
    /// 注意：begin_subagent 之前 PendingTool 已插入父级 ToolBatch，
    /// 因此查 main_tool_batch 必须包含尚未 push 的 Agent 工具。
    pub(crate) fn is_agent_tool_anywhere(
        &self,
        main_tool_batch: &ToolBatch,
        tool_call_id: &str,
    ) -> bool;

    /// begin_subagent：构造 SubAgentContext 并压栈。
    /// 不发 ObservationCreate（延迟到 end_subagent，保留 subagent_stack.rs:115-117 语义）。
    pub(crate) fn begin_subagent(&mut self, input: &serde_json::Value);

    /// end_subagent：返回构造 ObservationCreate 所需数据。
    /// caller 负责先 try_add ObservationCreate、再 flush 当前层 ToolBatch、最后 pop。
    /// [TRAP] 三步顺序由 caller 编排——保留 subagent_stack.rs:118-120 的约束，
    /// 但把"必须 flush 后才 pop"从注释升级为类型：SubagentPopGuard RAII 或显式三方法。
    pub(crate) fn end_subagent(&mut self) -> Option<SubagentEnd>;

    pub(crate) fn is_empty(&self) -> bool { self.stack.is_empty() }
    pub(crate) fn depth(&self) -> usize { self.stack.len() }
}
```

**关键设计**：`current_tool_batch_mut` 的返回类型是难点——栈空时返回主 struct 的 ToolBatch 引用，栈非空时返回栈顶的 ToolBatch 引用。Rust 借用检查器要求两条路径返回同类型，可行方案是 enum 或 trait object；更简单的做法是**让主 struct 也持有一个 ToolBatch 字段**，`current_tool_batch_mut` 总是返回栈顶 or 主 ToolBatch 的 `&mut`（用 `either::Either` 或手写枚举 `ToolBatchRef::Main(&mut ToolBatch) | ToolBatchRef::Sub(&mut ToolBatch)`）。

#### 5.2.4 `CompactSpan`

```rust
/// Compact span 的 on/off 状态机。
/// [不变量] compact_span = Some 仅在 on_compact_start 到 on_compact_end 之间。
pub(crate) struct CompactSpan {
    span: Option<CompactSpanContext>,
}

impl CompactSpan {
    pub(crate) fn new() -> Self { Self { span: None } }

    /// on_compact_start：返回 span_id + start_time（caller 构造 SpanCreate 入队）。
    /// 重复调用覆盖（保留现有行为；测试未覆盖此场景，可加 invariant assert）。
    pub(crate) fn on_start(&mut self) -> CompactSpanStart;

    /// on_compact_end：取出 ctx。返回 None 时 caller 早返回
    /// （compact_handler.rs:64-66，tracer_test L295 验证）。
    pub(crate) fn on_end(&mut self) -> Option<CompactSpanContext>;

    pub(crate) fn is_active(&self) -> bool { self.span.is_some() }
}

pub(crate) struct CompactSpanStart {
    pub span_id: String,
    pub start_time: String,
}
```

### 5.3 handler 文件如何降级为薄转发

封装后，6 个 handler 文件的 `impl LangfuseTracer` 块只剩**调用子对象 + 构造 Langfuse 事件 + try_add** 三步。

#### 5.3.1 `llm_handler.rs` 重构后（伪代码）

```rust
impl LangfuseTracer {
    pub fn on_llm_start(&mut self, step: usize, messages: &[BaseMessage], tools: &[ToolDefinition]) {
        self.flush_tools_batch(); // 委托 ToolBatch + SubagentStack
        self.generation.on_llm_start(step, messages.to_vec(), tools.to_vec());
    }

    pub fn on_llm_request_payload(&mut self, step: usize, body: Arc<serde_json::Value>) {
        self.generation.on_llm_request_payload(step, body);
    }

    pub fn on_llm_end(&mut self, step: usize, model: &str, provider: &str, output: &str, usage: Option<&TokenUsage>) {
        let Some(end) = self.generation.on_llm_end(step) else { return };
        let usage_details = usage.map(build_usage_details);
        let body = GenerationBody {
            id: Some(end.gen_id.clone()),
            trace_id: Some(self.trace_id.clone()),
            input: Some(end.input_json),
            usage_details,
            parent_observation_id: Some(self.current_agent_id()),
            metadata: end.retry_metadata,
            // ... 其他字段
            ..Default::default()
        };
        let event = IngestionEvent::GenerationCreate { /* ... */ };
        try_add_or_warn(&self.session.batcher, event, &self.trace_id, "generation");
    }

    pub fn on_llm_retrying(&mut self, attempt: usize, max_attempts: usize, delay_ms: u64, error: &str) {
        self.generation.on_llm_retrying(attempt, max_attempts, delay_ms, error);
    }
}
```

从 135 行降到约 50 行；`build_retry_metadata` 调用从 handler 移进 `GenerationTracker::on_llm_end` 内部。

#### 5.3.2 `tool_handler.rs` 重构后

```rust
impl LangfuseTracer {
    pub fn on_tool_start(&mut self, tool_call_id: &str, name: &str, input: &serde_json::Value) {
        let record = self.current_tool_batch_mut().on_tool_start(tool_call_id, name, input.clone());
        // 构造 ObservationCreate + try_add ...
        if self.is_agent_tool(tool_call_id) {
            self.subagent.begin_subagent(input);
        }
    }

    pub fn on_tool_end(&mut self, tool_call_id: &str, output: &str, is_error: bool) {
        // [TRAP] Agent 工具先 end_subagent 再查 PendingTool（顺序约束保留）
        if self.is_agent_tool(tool_call_id) {
            self.end_subagent(output, is_error);
        }
        let Some(tool) = self.current_tool_batch_mut().on_tool_end(tool_call_id) else { return };
        // 构造 ObservationCreate + try_add ...
        self.current_tool_batch_mut().record_end_time(end_time);
    }
}
```

**`[TRAP]` 注释保留**——顺序约束本质上跨 SubagentStack 和 ToolBatch，封装无法消除，但**约束现在被收口在 LangfuseTracer 主 struct 的方法里**，不再散落在 handler 各自实现。

---

## 6. seam 后面剩什么

### 6.1 LangfuseTracer 主 struct 的最终形状

```rust
pub struct LangfuseTracer {
    // 构造期固定、运行期只读
    session: Arc<LangfuseSession>,
    session_id: String,
    trace_id: String,
    agent_observation_id: String,
    // 累积字段
    final_answer: String,
    // 4 个子状态机（字段全私有）
    generation: GenerationTracker,
    tool_batch: ToolBatch,        // main agent 层
    subagent: SubagentStack,
    compact: CompactSpan,
}
```

9 个字段（含 4 个子对象），全部私有（无 `pub(crate)`）。子对象的字段对外完全不可见。

### 6.2 公开接口（保持不变）

`pub fn on_*` 全部保留原签名（`on_trace_start` / `on_trace_end` / `on_llm_start` / `on_llm_request_payload` / `on_llm_end` / `on_llm_retrying` / `on_tool_start` / `on_tool_end` / `on_text_chunk` / `on_compact_start` / `on_compact_end`），下游 `executor_helpers.rs:189-353` 和 `agent/workflow_agent.rs:142-504` **零改动**。这是本候选作为可回滚重构的关键 seam。

### 6.3 handler 文件变为薄转发层

| 文件 | 重构前 LOC | 重构后 LOC | 职责 |
|------|-----------:|-----------:|------|
| `llm_handler.rs` | 135 | ~50 | 调用 `generation.on_*` + 构造 GenerationCreate + try_add |
| `tool_handler.rs` | 131 | ~60 | 调用 `tool_batch.on_*` + 构造 ObservationCreate + try_add + Subagent 协调 |
| `compact_handler.rs` | 139 | ~70 | 调用 `compact.on_*` + 构造 SpanCreate + try_add |
| `trace_lifecycle.rs` | 101 | ~70 | 不变（已经薄） |
| `subagent_stack.rs` | 217 | ~80 | `SubagentStack` impl 迁入新模块；LangfuseTracer 上保留 `current_agent_id` / `flush_tools_batch` 转发 |

**handler 文件不再持有任何状态机不变量**——所有不变量收口在 4 个子对象 impl 内部。

### 6.4 facade `mod.rs` 的最终形状

```rust
mod compact;
mod compact_handler;
mod context;
mod event_builder;
mod generation;
mod generation_handler;     // 重命名自 llm_handler
mod llm_handler;            // 转发层（向后兼容文件名）
mod subagent;
mod subagent_handler;
mod tool_batch;
mod tool_handler;
mod trace_lifecycle;
mod usage;

pub struct LangfuseTracer { /* 9 字段 */ }

impl LangfuseTracer {
    pub fn new(...) { /* 构造 4 个子对象 */ }
    pub fn on_trace_start(...) { /* 转发 */ }
    pub fn on_trace_end(...) -> JoinHandle<()> { /* 转发 */ }
    // ...
}
```

facade 真正成为 facade——一眼读完所有公开 API，子状态机各在自己的模块。

---

## 7. 测试面

### 7.1 4 个子状态机独立单测（新增）

每个子对象可在自己的 `_test.rs` 内独立测不变量，无需构造整个 `LangfuseTracer`（不需要 batcher、session、trace_id）：

| 测试模块 | 测什么 | 行数估计 |
|---------|--------|---------|
| `generation_test.rs` | on_llm_start 设 active_step、retry 累积、on_llm_end 消费、retry 早返回（tracer_test.rs:251 的逻辑迁入） | ~80 |
| `tool_batch_test.rs` | lazy 创建 batch span、flush 取出后清空、is_agent_tool、未知 tool_call_id 早返回 | ~60 |
| `subagent_test.rs` | push/pop、current_agent_id fallback、嵌套 depth、end_subagent 顺序（迁移 tracer_test.rs:33-208 的 9 个 subagent 相关 test） | ~200 |
| `compact_test.rs` | on/off 切换、重复 start、未 start 直接 end 早返回（迁移 test L211-247） | ~40 |

**新增约 380 行子对象单测**，但**不依赖 LangfuseSession/batcher**（这些是子对象的纯逻辑测试），跑得更快、断言更精确（直接对 `GenerationEnd` 字段断言，无需读 batcher 内部）。

### 7.2 现有 `tracer_test.rs` (356 LOC) 的处理

| Test 编号 | 内容 | 处理 |
|----------|------|------|
| Test 1-9 | SubAgent 栈 push/pop / current_agent_id / 嵌套 | **迁移到 `subagent_test.rs`**，断言改读 `SubagentStack::depth()` / `current_agent_id()` |
| Test 10-12, 15 | Compact 生命周期 | **迁移到 `compact_test.rs`**，断言改读 `CompactSpan::is_active()` |
| Test 13-14 | LlmRetrying 累积 → metadata、on_llm_start 清空 retry | **迁移到 `generation_test.rs`**，断言改读 `GenerationEnd::retry_metadata` |
| Test 16-19 | on_llm_request_payload 缓存 / fallback | **迁移到 `generation_test.rs`**，断言改读 `GenerationEnd::input_json` |

**`tracer_test.rs` 保留约 50-80 行**作为集成层冒烟测试（`make_tracer()` + 调用 `on_trace_start` → `on_tool_start` → `on_trace_end`，验证子对象协作正常 + 事件入队 batcher）。

### 7.3 新增的边界测试

每个子状态机增加现有 test 未覆盖的场景：

- `GenerationTracker`：on_llm_end 后再 on_llm_retrying（应忽略或 panic？保留现有 silent no-op）。
- `ToolBatch`：on_tool_end 调用后再调 flush（batch_end_time 应保留还是清空？明确语义）。
- `SubagentStack`：空栈时 end_subagent（保留现有 warn 行为 `subagent_stack.rs:168-170`）。
- `CompactSpan`：on_start 后再 on_start 不 end（覆盖行为需明确，建议改为 debug_assert + 保留旧 ctx）。

---

## 8. 风险与回滚

### 8.1 风险 1：性能（多一层方法调用）

**分析**：每个 `on_*` 调用多一层 `self.generation.on_*()` 或 `self.tool_batch.on_*()`，单次开销是 1-2 条 `mov` 指令（ns 级）。相对单次 `try_add` 的 HashMap insert + Vec push（百 ns 级）完全可忽略。

**结论**：性能不是阻塞因素。无需 benchmark。

### 8.2 风险 2：借用冲突（多个子对象同时 mut）

**分析**：现有 `tool_handler.rs:22-45` 的 `[TRAP]` block scope workaround 是因为 `current_tools_context()` 返回 4 个 `&mut` 字段。封装后这一 workaround 可消除（ToolBatch 自己持有 4 字段、对外只暴露 `&mut self` 方法）。但**新的借用冲突点**：

```rust
// 错误示范：同时 mut 两个子对象
let agent_id = self.subagent.current_agent_id();  // 不可变借用
let record = self.tool_batch.on_tool_start(...);   // 可变借用
```

`current_agent_id` 是 `&self` 方法，与 `tool_batch.on_tool_start` 的 `&mut self` 不冲突。但如果有方法同时需要 `&mut self.subagent` 和 `&mut self.tool_batch`（如 end_subagent 后立即 flush 当前层 ToolBatch），Rust 允许——因为它们是 struct 的不同字段（disjoint borrow）。**关键：禁止在子对象方法签名里接收 `&mut LangfuseTracer`**，否则破坏 disjoint borrow。

**结论**：借用冲突可控，但需要在 `SubagentStack::current_tool_batch_mut` 的返回类型上小心（用 `ToolBatchRef` 枚举或让 caller 显式分支）。

### 8.3 风险 3：测试白盒访问的迁移成本

`tracer_test.rs` 19 处字段访问需要改写。**这是工作量风险，不是正确性风险**——子对象单测写完后，集成测试只需要少量冒烟用例。

**缓解**：分阶段迁移（见 §9），每个 phase 落地后跑 `cargo test -p peri-acp --lib` 确保 0 回归。

### 8.4 风险 4：SubagentStack 与 ToolBatch 的协作点

**分析**：`current_tools_context()` 的双路径写入（主 struct 字段 vs SubAgentContext 内嵌字段）是最容易出错的地方。封装后这一双路径**仍然存在**（main 层一个 ToolBatch、栈内每层一个 ToolBatch），只是字段从主 struct 收进了子对象。**真正的 leverage 在于**：双路径的协调现在收口在 `LangfuseTracer::current_tool_batch_mut()` 一个方法里（返回 `ToolBatchRef` 枚举），而不是散落在 `tool_handler.rs:24-30`、`subagent_stack.rs:54-68`、`subagent_stack.rs:175-186` 三处。

### 8.5 回滚策略

facade `pub fn on_*` 接口完全不变，下游 `executor_helpers.rs` 和 `workflow_agent.rs` 零改动。任一 phase 出问题：

- **Phase 1（GenerationTracker）失败**：回滚 `generation` 字段拆分，主 struct 直接持有 3 字段。其他 phase 不受影响。
- **Phase 2（ToolBatch）失败**：同上。
- **Phase 3（SubagentStack）失败**：保留现有 `subagent_stack.rs`，但接口已部分收敛（`current_agent_id` 等已迁移）。
- **Phase 4（CompactSpan + final_answer）失败**：影响最小（单字段），保留现有字段。

每个 phase 是独立 commit，git revert 单 commit 即可回滚。

---

## 9. 迁移步骤

### Phase 1：抽 `GenerationTracker`（最复杂，先做）

**理由**：3 字段（`generation_data` + `active_step` + `retry_attempts`）的耦合最紧、注释最多（`mod.rs:75-78` 两段 `[不变量]`）、现有测试覆盖最全（Test 13/14/16/17/18/19 共 6 个）。先做最复杂的，验证封装模式可行后再套用到其他子对象。

**步骤**：

1. 新建 `tracer/generation.rs`，定义 `GenerationTracker` + `GenerationStart` + `GenerationEnd`。
2. 把 `llm_handler.rs:14-135` 的 `on_llm_start` / `on_llm_request_payload` / `on_llm_retrying` 逻辑迁入 `GenerationTracker` impl。`on_llm_end` 拆成两步：`GenerationTracker::on_llm_end` 返回 `GenerationEnd`（含 input_json、retry_metadata），handler 用 `GenerationEnd` 构造 `IngestionEvent::GenerationCreate` + try_add。
3. `LangfuseTracer` 主 struct 删除 `generation_data` / `active_step` / `retry_attempts` 三字段，新增 `generation: GenerationTracker` 字段。
4. 修改 `tracer_test.rs` Test 13/14/16/17/18/19：6 个 test 改读 `GenerationTracker` 公开方法返回值。
5. 新建 `generation_test.rs`，补子对象边界测试（见 §7.3）。
6. `cargo test -p peri-acp --lib` 全绿。

**预计工作量**：2-3 天（含测试迁移）。**风险**：`build_retry_metadata` 调用从 handler 移进子对象，需确认 `usage.rs:42-61` 的签名不变。

### Phase 2：抽 `ToolBatch`

**理由**：4 字段（`pending_tools` + 3 个 `tools_batch_*`），与 SubagentStack 强耦合（双路径写入）。

**步骤**：

1. 新建 `tracer/tool_batch.rs`，定义 `ToolBatch` + `ToolStartRecord` + `ToolsBatchRecord`。
2. 把 `tool_handler.rs:11-131` 的工具缓冲逻辑（不含事件构造）迁入 `ToolBatch` impl。
3. `SubAgentContext`（`context.rs:41-56`）内嵌的 `tools_batch_*` 三字段 + `pending_tools` 改为内嵌一个 `ToolBatch` 字段。
4. **保留** `current_tools_context()` 的语义，但返回类型改为 `ToolBatchRef` 枚举（`Main(&mut ToolBatch) | Sub(&mut ToolBatch)`）。
5. 修改 `tracer_test.rs` Test 5（`pending_tools.len()` L110、`tools_batch_end_time.is_some()` L113）改读 ToolBatch 公开方法。
6. 新建 `tool_batch_test.rs`。

**预计工作量**：3-4 天。**风险**：双路径写入的迁移，`SubAgentContext` 字段重排会影响所有读 `subagent_stack.rs` 的代码。

### Phase 3：`SubagentStack` 接口收敛

**理由**：`subagent_stack.rs` 已存在（217 LOC），但所有方法是 `impl LangfuseTracer`。封装后改为 `impl SubagentStack`，handler 通过 `self.subagent.xxx()` 转发。

**步骤**：

1. 新建 `tracer/subagent.rs`，定义 `SubagentStack` + `SubAgentContext`（私有字段）+ `SubagentEnd`。
2. 把 `subagent_stack.rs:17-217` 的 `is_agent_tool` / `current_agent_id` / `current_tools_context` / `subagent_identity` / `begin_subagent` / `end_subagent` / `flush_tools_batch` 迁入 `SubagentStack` impl。注意 `flush_tools_batch` 涉及 ToolBatch，签名改为接收 `&mut ToolBatch`（main 层）参数或返回待 flush 的 ToolBatch 引用。
3. `LangfuseTracer` 主 struct 删除 `subagent_stack` 字段，新增 `subagent: SubagentStack` 字段。
4. `tracer_test.rs` Test 1-4, 6-9 迁入 `subagent_test.rs`（约 9 个 test、~200 行）。Test 5（subagent 内部事件路由）保留在 `tracer_test.rs` 作为集成测试。
5. 删除 `subagent_stack.rs`（被 `subagent.rs` 替代）。

**预计工作量**：2 天。**风险**：`is_agent_tool_anywhere` 跨 main 和 sub 查找，签名设计需谨慎（见 §5.2.3）。

### Phase 4：抽 `CompactSpan` + `final_answer` 收口

**理由**：最简单，1 字段状态机 + 1 字段累积器。

**步骤**：

1. 新建 `tracer/compact.rs`，定义 `CompactSpan` + `CompactSpanStart`。
2. 把 `compact_handler.rs:11-139` 的 ctx 取存逻辑迁入 `CompactSpan` impl。事件构造（SpanBody 构造 + try_add）保留在 handler。
3. `final_answer` 保留在主 struct（仅 2 处访问：`on_text_chunk` W、`on_trace_end` R + take）。可选项：封装为 `AnswerAccumulator`，但 leverage 低，建议保留为字段。
4. `tracer_test.rs` Test 10/11/12/15 迁入 `compact_test.rs`。
5. 删除 `compact_handler.rs` 或保留为薄转发层。

**预计工作量**：0.5-1 天。**风险**：无（最简单的 phase）。

### 收尾

6. 更新 `mod.rs` 文件头 doc comment（L1-24），把"持有 11 个状态字段"改为"持有 5 个累积字段 + 4 个子状态机对象"。
7. 更新 `CLAUDE.md` 模块索引（如涉及 LangfuseTracer 描述）。
8. 全量 `cargo test --workspace` + `lefthook run pre-commit`。

---

## 10. ADR 建议

**建议需要 ADR**。理由：

1. **跨文件重构规模大**：14 字段状态机分散在 6 个 handler 文件中直接读写，4 phase 迁移跨 6-8 天，每个 phase 独立合入但需保持总览一致性。ADR 提供迁移期间的决策锚点。
2. **关键设计决策需要回溯依据**：
   - `ToolBatchRef` 枚举 vs trait object（§5.2.3）—— 借用冲突的解决方案选择
   - `final_answer` 不封装（保留在主 struct）—— leverage < 1 的判断
   - `compact_handler.rs` 保留为薄转发层 —— 决策点
3. **是候选 6（trait 抽取）的强前置**：LangfuseTracer 封装后才能抽 `LangfuseTracerLike` trait 让候选 6 的 Phase 4 落地。两个候选的协同需要在 ADR 中记录。
4. **与候选 1（visitor）耦合**：handler 文件在候选 1 落地后会变成 visitor 实现，本候选把 handler 改为薄转发层的决策需要与候选 1 的 trait 接口对齐。

**ADR 标题建议**：`ADR-2026-07-13-langfuse-tracer-state-encapsulation`

**ADR 内容建议结构**：

- **Context**：当前 14 字段 + 6 handler 文件的 locality 碎片化现状
- **Decision**：4 个子状态机对象（GenerationTracker / ToolBatch / SubagentStack / CompactSpan）+ 主 struct 持 5 累积字段
- **Alternatives**：
  - 全字段私有 + 14 getter/setter（被否决：接口爆炸）
  - 合成单一子对象（被否决：内聚类正交）
  - 引入 state machine crate（被否决：过度设计）
- **Consequences**：handler 文件变成薄转发层；tracer_test.rs 19 个 test 分 4 批迁移；候选 6 Phase 4 解锁
- **Compliance**：4 phase 完成后 `LangfuseTracer` 主 struct 全私有字段 + 4 个 `pub` 子对象，单测覆盖每子状态机

**记录时机**：建议 Phase 1（GenerationTracker 抽取）验证封装模式可行后再写 ADR，避免过度前期文档化。若 Phase 1 完成后模式有重大调整，ADR 应反映最终方案。

---

## 附录：grilling 决策记录

- **Q：为什么不把 4 个子对象合成一个大对象？** 4 个子对象内聚类正交（generation 与 tool_batch 无字段共享、与 subagent 仅通过 `current_agent_id` 协作），合成大对象等于把 14 字段换名字重新散开，不解决 locality。
- **Q：为什么不用 state machine crate（`statig` 等）？** LangfuseTracer 是 4 个并行小状态机 + 累积字段，引入 state machine crate 会把简单不变量包装成复杂 transition table，过度设计。手写 4 个 struct + 方法足够。
- **Q：为什么不直接把字段全改私有（不加子对象）？** 14 字段改私有 + 14 个 getter/setter 只是把 locality 碎片化换成接口爆炸——handler 仍需懂全部字段关系。leverage 在于聚成子对象、用高层方法（如 `self.generation.on_llm_end(step)` 返回 `GenerationEnd`）隐藏 active_step / retry_attempts 的同步关系。
- **Q：为什么保留 `final_answer` 在主 struct？** 仅 2 处访问（on_text_chunk W / on_trace_end R+take），封装成 `AnswerAccumulator` 的 leverage < 1（接口比字段多）。
- **Q：候选 1 vs 候选 2 谁先？** 候选 1 先（不阻塞），候选 1 收敛 `forward_langfuse_event` 的 match 让 `on_*` 签名更稳定。但两者可并行——候选 1 不改 tracer 接口。
- **Q：与候选 6（trait 抽取）衔接？** 候选 2 是候选 6 的强前置：现状 14 个 `pub(crate)` 字段，抽 trait 后测试仍需白盒访问 → trait 必须暴露 14 个 getter → trait 巨胖。候选 2 后主 struct 9 字段全私有、trait surface 仅 ~10 个 `on_*` 方法，候选 6 工作量从"重构 + 抽 trait"降为"仅抽 trait"（约 1 天）。
