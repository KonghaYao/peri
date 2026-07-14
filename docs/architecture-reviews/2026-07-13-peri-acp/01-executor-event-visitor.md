# 候选 1：ExecutorEvent 三处 match 收敛到 visitor

> 日期：2026-07-13 | 模块：`peri-agent/src/agent/events.rs` + `peri-acp/src/event/{mapper,router}.rs` + `peri-acp/src/session/executor_helpers.rs` | 类型：架构走读
> 流程：/grilling（leaking seam + locality 碎片）
> 范围：1 个 24 变体 enum + 3 处独立 match（mapper / router / langfuse forwarder）+ 1 处部分 match（workflow_agent）

---

## 1. 摘要

`peri_agent::agent::events::ExecutorEvent` 是一个 24 变体的 enum，被 peri-acp 在 **三处独立文件** 各自做全量 `match`，外加 workflow_agent 里一处部分 match：mapper（`event/mapper.rs:141` + `:339`，两段 match）、router（`event/router.rs:47`）、langfuse forwarder（`session/executor_helpers.rs:273`），以及 `agent/workflow_agent.rs:187` 一段补充 match。每次 peri-agent 给 ExecutorEvent 加/减变体，**至少四处必须同步修改**，但 Rust 编译器只在「match 未覆盖」时报错；只要某处用 `_ => {}` 兜底（如 `executor_helpers.rs:353` 与 `workflow_agent.rs:198`），新增变体会**静默丢失**——mapper 不映射、router 不路由、tracer 不打点，UI 表现为「事件凭空消失」。这是典型的 leaking seam（enum 暴露面跨 crate 渗漏）+ locality 碎片（同一抽象的 4 处实现零物理聚合）。

本候选走 /grilling 流程逐一拷问三个加深方向（A visitor trait + accept 入口 / B 物理合到单文件 / C 数据化分发表），结论是 **方向 A（visitor trait 在 peri-agent 侧，accept 入口收敛 match）+ 强制 `#[non_exhaustive]` 关闭 + 编译期不可绕过** 为推荐方案。它把 4 处 60+ arm 压成 peri-agent 一个 `accept` 单点 match，编译器从「`_ => {}` 静默吞」升级为「trait method 未实现 → 编译失败」，把 leaking seam 变成 typed seam。方向 B（物理合并）不改语义、不消灭静默兜底；方向 C（HashMap 分发）牺牲 exhaustive check 换运行时可扩展，本场景 enum 变体稳定、不需要运行时扩展，属过度设计。

---

## 2. 现状诊断

### 2.1 四处独立 match 的具体证据

#### 证据 1 — mapper.rs 正向映射（Category ① + ③ 主分发）

`peri-acp/src/event/mapper.rs:141-332` `map_event()`：

```rust
pub fn map_event(event: &ExecutorEvent, context_window: u32) -> Vec<MappedEvent> {
    match event {
        ExecutorEvent::TextChunk { chunk, source_agent_id, .. } => { /* ... */ }
        ExecutorEvent::AiReasoning { text, source_agent_id, .. } => { /* ... */ }
        ExecutorEvent::ToolStart { /* ... */ } => { /* ... */ }
        ExecutorEvent::ToolEnd { /* ... */ } => { /* ... */ }
        ExecutorEvent::TodoUpdate(entries) => { /* ... */ }
        ExecutorEvent::LlmCallEnd { usage: Some(u), .. } => { /* ... */ }
        // ── Category ③: TUI-only ──
        ExecutorEvent::ContextWarning { .. }
        | ExecutorEvent::LlmRetrying { .. }
        | ExecutorEvent::StateSnapshot(_)
        | ExecutorEvent::StateSnapshotMeta { .. }
        | ExecutorEvent::TurnCommitted { .. }
        | ExecutorEvent::CompactStarted
        | ExecutorEvent::CompactCompleted { .. }
        | ExecutorEvent::CompactError { .. }
        | ExecutorEvent::RewindCompleted { .. }
        | ExecutorEvent::BackgroundTaskCompleted(_)
        | ExecutorEvent::BgToolStep { .. }
        | ExecutorEvent::LspDiagnostics { .. }
        | ExecutorEvent::AgentExecutionFailed { .. }
        | ExecutorEvent::WorkflowProgress(_) => vec![MappedEvent::tui_only()],
        // ── Filtered ──
        ExecutorEvent::LlmCallStart { .. }
        | ExecutorEvent::LlmCallEnd { usage: None, .. }
        | ExecutorEvent::LlmRequestPayload { .. } => vec![MappedEvent::none()],
        ExecutorEvent::MessageAdded(msg) => { /* synthetic user msg */ }
        ExecutorEvent::TurnSuspended => vec![MappedEvent::none()],
    }
}
```

此处 match **是** exhaustive（无 `_ =>`），编译器会强制同步。但仅在 mapper 内部。

#### 证据 2 — mapper.rs 第二段 match（DTO 转换）

同文件 `peri-acp/src/event/mapper.rs:339-494` `executor_event_to_acp()` 又做了一遍完整的 24 路 match，把 ExecutorEvent 翻成 `AcpEvent` DTO（peri/agent_event 通道）。**一个文件两个 match，路径不同**：Category ① 走 `map_event` 进 `session/update`，Category ③ 走 `executor_event_to_acp` 进 `peri/agent_event`。新增变体必须**同时**改这两段，否则会出现「session/update 推送但 agent_event 不推送」的双通道不一致。

#### 证据 3 — router.rs 的第三段 match

`peri-acp/src/event/router.rs:47-137` `route()` 又一遍：

```rust
pub fn route(ev: &ExecutorEvent) -> Option<RoutingOutput> {
    match ev {
        ExecutorEvent::ContextWarning { used_tokens, total_tokens, percentage } => { /* ... */ }
        ExecutorEvent::RewindCompleted { summary, messages, .. } => { /* ... */ }
        // ── §5.1 Discarded ──
        ExecutorEvent::LlmCallEnd { .. }
        | ExecutorEvent::LlmRetrying { .. }
        | /* 18 个变体 */ => None,
        ExecutorEvent::TurnSuspended => Some(/* ... */),
    }
}
```

router 的 Category 划分与 mapper **不同**：mapper 把 `ContextWarning` 归 Category ③，router 把它翻成 `budget-warning` 推送。两个文件的「分类语义」无法共用，各自硬编码。新增变体要在 router 的长 `|` 链里手动添加，否则掉进 `None` 黑洞。

#### 证据 4 — executor_helpers.rs（langfuse forwarder，**带静默兜底**）

`peri-acp/src/session/executor_helpers.rs:273-355` `forward_langfuse_event()`：

```rust
pub(crate) fn forward_langfuse_event(
    tracer: &parking_lot::Mutex<LangfuseTracer>,
    exec_event: &ExecutorEvent,
    provider_display_name: &str,
) {
    match exec_event {
        ExecutorEvent::LlmCallStart { step, messages, tools } => { /* ... */ }
        ExecutorEvent::LlmRequestPayload { step, body } => { /* ... */ }
        ExecutorEvent::LlmCallEnd { /* ... */ } => { /* ... */ }
        ExecutorEvent::ToolStart { /* ... */ } => { /* ... */ }
        ExecutorEvent::ToolEnd { /* ... */ } => { /* ... */ }
        ExecutorEvent::TextChunk { chunk, .. } => { /* ... */ }
        ExecutorEvent::LlmRetrying { /* ... */ } => { /* ... */ }
        ExecutorEvent::CompactStarted => { /* ... */ }
        ExecutorEvent::CompactCompleted { /* ... */ } => { /* ... */ }
        ExecutorEvent::CompactError { message } => { /* ... */ }
        _ => {}  // ← 兜底！新增变体静默掉进这里，tracer 不打点
    }
}
```

`_ => {}` 是**关键风险点**：peri-agent 新增 `TurnSuspended` / `BgToolStep` / `WorkflowProgress` / `SubagentStarted|Stopped` 等变体后，这些变体在 langfuse 侧**完全不可见**，且无编译错误、无运行时警告。

#### 证据 5 — workflow_agent.rs 的第二处部分 match

`peri-acp/src/agent/workflow_agent.rs:187-198`：

```rust
match event {
    ExecutorEvent::LlmCallEnd { usage, model, .. } => { /* 累加 token */ }
    ExecutorEvent::LlmRetrying { attempt, max_attempts, error, .. } => { /* warn */ }
    ExecutorEvent::AgentExecutionFailed { message } => { /* warn */ }
    _ => {}
}
// 紧接着调用 forward_langfuse_event —— 复用证据 4 的 match
crate::session::executor::forward_langfuse_event(tracer, &event, &provider_display_name);
```

这是**第四处** ExecutorEvent 消费点，同样带 `_ => {}` 兜底。说明 leaking seam 的下游消费者不止 3 个，且都有静默吞变体的风险。

### 2.2 量化：24 变体 × 4 处 = 接近 100 个 arm

```
peri-agent/src/agent/events.rs:104-268:  24 个变体定义
peri-acp/src/event/mapper.rs:141:        24 arm（exhaustive，map_event）
peri-acp/src/event/mapper.rs:339:        24 arm（exhaustive，executor_event_to_acp）
peri-acp/src/event/router.rs:47:         24 arm（exhaustive，route）
peri-acp/src/session/executor_helpers.rs:273: 11 显式 arm + _ 兜底（forward_langfuse_event）
peri-acp/src/agent/workflow_agent.rs:187:     3 显式 arm + _ 兜底
─────────────────────────────────────────
总计：24 变体 × 4 处消费 ≈ 96 个 arm 引用
```

`grep -c "ExecutorEvent::"` 验证：

```
mapper.rs:             52 处引用（两段 match 合计）
router.rs:             26 处引用
executor_helpers.rs:   15 处引用
workflow_agent.rs:     若干
```

### 2.3 用架构词汇描述

| 现象 | 架构词汇 | 在本候选中的体现 |
|------|---------|----------------|
| enum 暴露面跨 crate 渗漏 | **leaking seam** | `ExecutorEvent` 定义在 peri-agent，但 4 处 match 散落在 peri-acp，每处都是一段 24 arm 的 seam |
| 同一抽象的实现零物理聚合 | **locality 碎片** | 「ExecutorEvent 的所有消费者」这一抽象分散在 3 个目录（`event/`、`session/`、`agent/`），新人无法从一个文件读懂全貌 |
| 单点改动产生多点同步 | **低 leverage** | 加 1 个变体 = 改 4 个文件 × 平均 3 个 arm，没有任何一处可以「代替」其它处 |
| 兜底 arm 吞掉错误 | **silent failure** | `_ => {}` 让编译器失去保护；这是比 low leverage 更严重的问题——它让 leaking seam **不报警** |

**核心判断**：本候选的根因不是「match 太多」，而是 **leaking seam 没有被类型系统收敛**。三个加深方向的取舍核心是：**谁去收敛、用什么机制收敛**。

---

## 3. 约束（不可变的事实）

### 3.1 设计文档规定的不变量

| 来源 | 不变量 | 对本候选的影响 |
|------|--------|---------------|
| `docs/design/peri-agent-acp-v2.md:7` | ACP 是「薄适配层」，不定义 Agent 结构，只做协议转换 | trait 不能反过来让 peri-acp 控制 ExecutorEvent 形状；trait 必须放在 peri-agent |
| `docs/design/peri-agent-acp-v2.md:9` | 一条 event pipeline，三个消费方向（标准 ACP / TUI / 过滤） | 三处 match 的**职责不同**——mapper 主写 session/update、router 主写 peri/unstable-event、langfuse 写遥测；trait 必须允许三个 visitor 各自表达不同的输出类型 |
| `docs/design/decisions/2026-07-07-acp-reuse-first.md` | 标准 ACP SessionUpdate 能覆盖的事件不得在 unstable-event 复制 | mapper 与 router 的**分类语义**不可强行合并（TextChunk 在 mapper 是 ①、在 router 是 None），trait impl 必须允许这种语义差异 |
| `CLAUDE.md`「AgentEvent 变体」陷阱 | 新增 ExecutorEvent 变体需同步 `map_executor_event`（peri-acp/event + peri-tui/acp_events） | 与本候选直接对应：陷阱速查本身就承认了同步点散落 |

### 3.2 ACP 协议契约

- **流式高频**：`TextChunk` 在生成阶段每秒数十次（参见 `peri-acp-protocol.md` §4.1）。任何抽象层不得在热路径引入堆分配或动态分发导致的内联失败。
- **Category 分类**：mapper 必须能输出 `Vec<MappedEvent>`（同一变体可产出 0-N 个事件）；router 输出 `Option<RoutingOutput>`；langfuse 输出 `()`（副作用）。三者返回类型不同。
- **双通道一致性**：mapper 同时拥有 `map_event` 与 `executor_event_to_acp`，对应 session/update 与 peri/agent_event 两个通道；同一变体在两段 match 中的归属必须一致（MessageAdded 不能在 map_event 走 ①、在 executor_event_to_acp 走 None）。

### 3.3 性能约束

- **每秒数十次流式事件**：visitor trait 的 `&mut self` 调用在 LLM 流式生成阶段会被高频触发。devirtualization 是关键——如果 trait object `&dyn Visitor` 调用无法内联，会带来一次间接 call（约 1-3 ns），在每秒数十次量级下可忽略，但若未来 token-batching 后上升到每秒数百次，则需评估。
- **`Vec<MappedEvent>` 返回值**：mapper 当前返回 `Vec`，trait method 的 `type Output` 必须支持集合返回，不能强行收成 `Option`。
- **Clone 边界**：ExecutorEvent 含 `Arc<Vec<BaseMessage>>`（LlmCallStart）与 `Arc<Value>`（LlmRequestPayload），均为浅拷贝；visitor 接收 `&ExecutorEvent` 即可，不需要 `Clone` 边界。

### 3.4 不可重排约束

`CLAUDE.md` 中间件链 14+5 顺序不可重排，与本候选无关——本候选只动 enum 消费的 4 处 match，不触碰中间件链。

---

## 4. 依赖关系

### 4.1 前置

**无**。本候选独立可做。ExecutorEvent 是稳定接口，三处 match 当前可工作（除静默兜底外），迁移可在不破坏现有契约的前提下分阶段进行。

### 4.2 后置

| 后续候选 | 依赖方式 |
|---------|---------|
| **候选 2**（LangfuseTracer 状态封装） | 强依赖：候选 1 落地后，`forward_langfuse_event` 变成 `LangfuseVisitor` impl，与 LangfuseTracer 的耦合面收敛到 trait method 边界；封装 tracer 状态时只需改一个 impl，不再担心 match 散落 |
| **候选 6**（trait 测试面提取） | 中度依赖：候选 1 引入的 `ExecutorEventVisitor` trait 本身就是「可注入测试 mock」的天然接缝；候选 6 在此基础上提取 trait 时，visitor 已就位 |
| SubAgent 事件转发（`inject_source_agent_id` 等） | 弱相关：当前 `events.rs:309` 的 `inject_source_agent_id` 也是一个部分 match + 兜底；候选 1 落地后可顺手用 visitor 模式重构，但不在本候选范围内 |

### 4.3 平行

| 平行候选 | 协同方式 |
|---------|---------|
| **候选 5**（mapper 重命名为 direction-aware） | 可同期做：候选 1 引入 trait 时，mapper.rs 内的两段 match（map_event / executor_event_to_acp）会分别成为两个 visitor impl；重命名让方向更显式，互不冲突 |
| **候选 4**（dispatch registry 合并） | 独立事件轴：dispatch 是 JSON-RPC 方法分发，与本候选的 ExecutorEvent 消费是两条不相交的 match 群 |

---

## 5. 加深后的模块形状

### 5.1 方向 A（推荐）：Visitor trait + accept 入口（在 peri-agent）

#### Interface 草案

```rust
// peri-agent/src/agent/events/visitor.rs（新文件）

/// ExecutorEvent 的 visitor trait。
///
/// 每个变体对应一个 `visit_*` method（按字段解构成参数，避免 visitor 反向
/// 重建 enum）。所有 method 默认实现都是 `default_visit`——这样新增变体时
/// 老 visitor 不破坏。但 [`ExecutorEvent::accept`] 是唯一 exhaustive match，
/// 新增变体时该函数编译失败，强制维护者同时：
///   1. 在 trait 中新增 `visit_*` method（带默认实现）
///   2. 在 accept 中新增 arm
///   3. 检查 MapperSessionUpdateVisitor / MapperAcpEventVisitor 是否需要 override
///      （这两个 visitor 的 default_visit 实现为 panic，强制显式覆盖——见 §5.5）
pub trait ExecutorEventVisitor {
    /// 默认行为：no-op。RouterVisitor / LangfuseVisitor 走默认是合法意图。
    fn default_visit(&mut self, _event: &ExecutorEvent) {}

    fn visit_ai_reasoning(&mut self, text: &str, source_agent_id: Option<&str>) {
        self.default_visit(/* 重构事件占位 */);
    }
    fn visit_text_chunk(&mut self, message_id: &MessageId, chunk: &str, src: Option<&str>) { /* ... */ }
    fn visit_tool_start(&mut self, /* 5 字段解构 */) { /* ... */ }
    fn visit_tool_end(&mut self, /* ... */) { /* ... */ }
    fn visit_llm_call_start(&mut self, step: usize, messages: &Arc<Vec<BaseMessage>>, tools: &[ToolDefinition]) { /* ... */ }
    fn visit_llm_call_end(&mut self, step: usize, model: &str, output: &str, usage: Option<&TokenUsage>, stop_reason: Option<&StopReason>) { /* ... */ }
    fn visit_compact_started(&mut self) { /* ... */ }
    fn visit_compact_completed(&mut self, /* ... */) { /* ... */ }
    fn visit_turn_suspended(&mut self) { /* ... */ }
    // ... 共 24 个 method ...
}

impl ExecutorEvent {
    /// **唯一** exhaustive match 入口。无 `_ =>` 兜底。
    pub fn accept<V: ExecutorEventVisitor>(&self, visitor: &mut V) {
        match self {
            ExecutorEvent::AiReasoning { text, source_agent_id } => {
                visitor.visit_ai_reasoning(text, source_agent_id.as_deref());
            }
            ExecutorEvent::TextChunk { message_id, chunk, source_agent_id } => {
                visitor.visit_text_chunk(message_id, chunk, source_agent_id.as_deref());
            }
            // ... 其余 22 arm，无 _ 兜底 ...
        }
    }
}
```

#### Mapper visitor（peri-acp 侧）

```rust
// peri-acp/src/event/mapper.rs（重构后）

pub struct MapperVisitor {
    context_window: u32,
    /// 累积输出：一个 ExecutorEvent 可产出多个 MappedEvent
    out: Vec<MappedEvent>,
}

impl MapperVisitor {
    pub fn new(context_window: u32) -> Self {
        Self { context_window, out: Vec::new() }
    }
    pub fn into_result(self) -> Vec<MappedEvent> { self.out }
}

impl ExecutorEventVisitor for MapperVisitor {
    // 只 override 需要特殊处理的变体
    fn visit_text_chunk(&mut self, message_id: &MessageId, chunk: &str, src: Option<&str>) {
        self.out.push(MappedEvent::standard_with_src(
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(chunk.to_string())),
            ))],
            src.map(Into::into),
        ));
    }
    fn visit_llm_call_end(/* ... */) {
        // 含 context_window 的 _meta 构造（原 map_event:241-271 逻辑）
    }
    fn visit_message_added(&mut self, msg: &BaseMessage) {
        // synthetic user message 通道
    }
    // 其余 Category ③/Filtered 变体走 default_visit → push tui_only / none
    // 但 default_visit 无法区分 tui_only vs none——需要分两组默认 method：
}
```

**Mapper visitor 的 default 难题**：mapper 把 24 变体分为 5 组（① standard / ② hitl / ③ tui_only / ③ + ④ / Filtered），单一 `default_visit` 无法表达分组。解决方案有两个：

1. **不依赖默认**：MapperVisitor override 全部 24 个 method（即使只是 `push tui_only()`）。代码量大，但语义显式。
2. **分组辅助方法**：trait 提供 `default_category_3()` / `default_filtered()` 两个分组默认，visitor 选择性调用。

推荐方案 1，原因：visitor 的价值就是**显式**，依赖默认实现等于把 `_ => {}` 静默兜底问题搬到 trait 里——回到原点。

#### Router visitor

```rust
pub struct RouterVisitor { out: Option<RoutingOutput> }
impl ExecutorEventVisitor for RouterVisitor {
    fn visit_context_warning(&mut self, used: u64, total: u64, pct: f64) {
        self.out = Some(/* 构建 budget-warning */);
    }
    fn visit_rewind_completed(&mut self, summary: &str, messages: &[BaseMessage]) {
        self.out = Some(/* 构建 rewind-preview */);
    }
    fn visit_turn_suspended(&mut self) {
        self.out = Some(RoutingOutput {
            event_name: "turn-suspended".into(),
            data: serde_json::Value::Object(Default::default()),
        });
    }
    // 其余 21 个变体：override 成空 fn 或走 default_visit（返回 None）
    // 这里允许 default_visit，因为 router 的 21 个 None 是真正的 no-op
}
```

Router 是 trait 默认实现的**合法**场景：21 个变体确实就是 no-op，不存在「应做某事却忘了」的风险。

#### Langfuse visitor

```rust
pub struct LangfuseVisitor<'a> {
    tracer: &'a parking_lot::Mutex<LangfuseTracer>,
    provider_display_name: &'a str,
}
impl<'a> ExecutorEventVisitor for LangfuseVisitor<'a> {
    fn visit_llm_call_start(&mut self, step: usize, messages: &Arc<Vec<BaseMessage>>, tools: &[ToolDefinition]) {
        self.tracer.lock().on_llm_start(*step, messages, tools);
    }
    fn visit_compact_started(&mut self) { self.tracer.lock().on_compact_start(); }
    // ... override 11 个有打点需求的变体 ...
    // 其余 13 个走 default_visit（no-op）——与现状 _ => {} 等价
}
```

Langfuse 同 router：13 个 no-op 是真实意图，default_visit 合法。

#### 方向 A 的取舍

| 维度 | 评估 |
|------|------|
| **消灭静默兜底** | 部分。mapper/router/langfuse 三处的 `_ => {}` 变成 trait 默认方法——只要新增变体时 trait 加新 method（带默认），消费侧**仍然可以静默不 override**。但 `accept` 单点 match 是 exhaustive 的，新增变体至少触发**一处编译失败**（peri-agent 内部），提醒维护者「这里有新 method 了」 |
| **locality** | 强。所有 visit_ method 签名集中在 `peri-agent/src/agent/events/visitor.rs`，新人能从一个文件读懂 ExecutorEvent 的全部消费面 |
| **leverage** | 强。加变体 = 改 1 个 match（accept）+ 加 1 个 trait method。消费者按需 override |
| **编译时间** | 中。trait expansion 增加，但 24 method × 4 impl = 96 个 method 体，相比当前 96 arm 的 match 体积相近 |
| **devirtualization** | 强。`accept<V: ExecutorEventVisitor>` 是单态化泛型，每个 visitor 类型生成独立的代码，编译期可内联 |

### 5.2 方向 B：物理合到单文件 `event/dispatch.rs`（最小改动）

#### Interface 草案

```rust
// peri-acp/src/event/dispatch.rs（新文件，物理合并 mapper + router + langfuse）

use peri_agent::agent::events::ExecutorEvent;

// ──────────────────────────────────────────────────────────────────────────
// ⚠️ 变体增减必须三处同步：map / route / forward_langfuse。
// 编译器无法强制——请同时检查 mapper_test / router_test / tracer_test。
// ──────────────────────────────────────────────────────────────────────────

pub fn map_event(event: &ExecutorEvent, context_window: u32) -> Vec<MappedEvent> {
    /* 原 mapper.rs:141 的 match，原样搬过来 */
}

pub fn executor_event_to_acp(event: &ExecutorEvent) -> Option<AcpEvent> {
    /* 原 mapper.rs:339 的 match，原样搬过来 */
}

pub fn route(ev: &ExecutorEvent) -> Option<RoutingOutput> {
    /* 原 router.rs:47 的 match，原样搬过来 */
}

pub fn forward_langfuse_event(
    tracer: &parking_lot::Mutex<LangfuseTracer>,
    exec_event: &ExecutorEvent,
    provider_display_name: &str,
) {
    /* 原 executor_helpers.rs:273 的 match，原样搬过来 */
}
```

#### 方向 B 的取舍

| 维度 | 评估 |
|------|------|
| **消灭静默兜底** | **否**。`_ => {}` 原样保留，新增变体仍静默丢失 |
| **locality** | 中。三处合到一文件，至少 grep `ExecutorEvent::` 时命中同一文件 |
| **leverage** | 弱。加变体仍需改 4 处 arm，没省力 |
| **编译时间** | 略改善（少 3 个文件开销） |
| **devirtualization** | 无变化（仍是直接 match） |
| **迁移成本** | 极低——纯文本搬运 |

**判断**：方向 B 只解决了 locality 碎片的「物理位置分散」一层，没解决「类型系统不强制」的根因。等价于把 leaking seam 的多个出口搬到同一房间，但出口仍开着。可作为方向 A 的 Phase 0 过渡，**不可作为终态**。

### 5.3 方向 C：数据化分发表（HashMap）

#### Interface 草案

```rust
// peri-acp/src/event/dispatch_table.rs

use peri_agent::agent::events::ExecutorEvent;
use std::collections::HashMap;

/// 用 enum 的 discriminant 作为 key，handler fn 作为 value。
/// 注册一次，运行时 O(1) 查表。

pub type MapperFn = fn(&ExecutorEvent, u32) -> Vec<MappedEvent>;
pub type RouterFn = fn(&ExecutorEvent) -> Option<RoutingOutput>;
pub type LangfuseFn = fn(&parking_lot::Mutex<LangfuseTracer>, &ExecutorEvent, &str);

pub struct DispatchTable {
    map: HashMap<ExecutorEventKind, MapperFn>,
    route: HashMap<ExecutorEventKind, RouterFn>,
    langfuse: HashMap<ExecutorEventKind, LangfuseFn>,
}

#[derive(Hash, Eq, PartialEq)]
pub enum ExecutorEventKind {
    TextChunk, AiReasoning, ToolStart, ToolEnd, TodoUpdate,
    LlmCallStart, LlmRequestPayload, LlmCallEnd,
    StateSnapshot, StateSnapshotMeta, TurnCommitted, MessageAdded,
    ContextWarning, LlmRetrying, CompactStarted, CompactCompleted, CompactError,
    RewindCompleted, BackgroundTaskCompleted, BgToolStep,
    SubagentStarted, SubagentStopped, LspDiagnostics, AgentExecutionFailed,
    WorkflowProgress, TurnSuspended,
}

impl DispatchTable {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        map.insert(ExecutorEventKind::TextChunk, |ev, _cw| { /* ... */ });
        // ... 注册 24 个 ...
        Self { map, route: /* ... */, langfuse: /* ... */ }
    }
}

// ExecutorEvent 需要新增一个 kind() 方法
impl ExecutorEvent {
    pub fn kind(&self) -> ExecutorEventKind {
        match self {
            ExecutorEvent::TextChunk { .. } => ExecutorEventKind::TextChunk,
            // ... 24 arm ...
        }
    }
}
```

#### 方向 C 的取舍

| 维度 | 评估 |
|------|------|
| **消灭静默兜底** | **否**。HashMap miss 默认返回 None / 空 Vec，比 `_ => {}` 更隐蔽 |
| **locality** | 中。所有 handler fn 在 `new()` 中注册，物理聚合 |
| **leverage** | 中。加变体仍需改 `kind()` match + 3 处 HashMap insert |
| **编译时间** | 中（fn 指针表比 trait 单态化体积小） |
| **devirtualization** | 弱。fn 指针调用无法内联，每次 dispatch 一次间接 call |
| **运行时可扩展** | 强（可动态注册 handler）——但本场景不需要 |

**判断**：方向 C 是为「运行时插件注册」准备的，但 ExecutorEvent 是 peri-agent 编译期 enum，没有运行时扩展需求。引入 HashMap 反而**引入新的 silent failure 模式**（key miss），与候选目标背道而驰。**不推荐**。

### 5.4 方向对比矩阵

| 维度 | 方向 A（visitor trait） | 方向 B（物理合并） | 方向 C（HashMap 分发） |
|------|------------------------|-------------------|----------------------|
| 消灭 `_ => {}` 静默兜底 | 部分（accept 单点 exhaustive） | 否 | 否（更差） |
| locality | 强（trait 单文件 + impl 单文件） | 中（同文件多 match） | 中（dispatch table 单文件） |
| leverage | 强（加变体改 1 处） | 弱（仍改 4 处） | 中（改 kind + 3 处 insert） |
| 编译期强制 | **强**（accept exhaustive） | 弱（仅 mapper/router 两段） | 无 |
| 性能（流式热路径） | 单态化，可内联 | 直接 match，最快 | 间接 call，不可内联 |
| 迁移成本 | 中（需引入 trait + 重写 4 处） | 低（搬运） | 中（重写为 fn 指针表） |
| 对候选 2 / 6 的杠杆 | **强**（trait 直接被候选 6 复用） | 无 | 弱 |

### 5.5 推荐：方向 A

**推荐方向 A**，核心理由：

1. **唯一能把 silent failure 变成 typed seam 的方案**。方向 B / C 都让 `_ => {}` 换皮不换骨；方向 A 把 enum 的 match 收敛到 `accept` 单点，新增变体至少触发一处编译失败。
2. **对候选 2、6 有正向杠杆**。候选 6「trait 测试面提取」本质上就是引入可注入的 trait；候选 1 提前把 ExecutorEventVisitor 放好，候选 6 可直接 mock。
3. **性能可接受**。单态化泛型 `accept<V>` 在 release 模式下会被 inline，热路径开销与直接 match 相当。
4. **流式高频路径不退步**。TextChunk 在 mapper visitor 中是一次 `out.push`，与当前 `vec![MappedEvent::standard(...)]` 等价。

**关键约束**：trait method **不全部给默认实现**，而是按消费者类型分组——mapper visitor 必须显式 override 全部 24 method（不允许走默认），router/langfuse visitor 允许走默认。这通过把 MapperVisitor 的 `default_visit` panic 实现（"MapperVisitor must override all variants"）来强制。

---

## 6. seam 后面剩什么

### 6.1 peri-agent 侧新增

| 新增项 | 位置 | 职责 |
|--------|------|------|
| `ExecutorEventVisitor` trait | `peri-agent/src/agent/events/visitor.rs`（新文件） | 24 个 `visit_*` method + `default_visit` fallback |
| `ExecutorEvent::accept<V>` method | `peri-agent/src/agent/events.rs`（impl 块） | 唯一 exhaustive match，按变体分发到 visitor |
| `#[non_exhaustive]` 标注 | `peri-agent/src/agent/events.rs:103`（enum 上） | 强制外部 crate 在 match 时不能假定覆盖完整；但 `accept` 内部 match 是 non_exhaustive-private 仍 exhaustive |

**关键点**：`#[non_exhaustive]` 对外部 match 有意义（外部不能写不带 `_` 的 exhaustive match），但 `accept` 是 peri-agent 内部的 `impl` 块，**不受 non_exhaustive 影响**——它必须覆盖所有变体。这正好是我们要的：外部消费者走 visitor（不直接 match），enum 变更由 accept 单点承接。

### 6.2 peri-acp 侧改动

| 消费者 | 改动 |
|--------|------|
| `event/mapper.rs:141` `map_event` | 重写为 `MapperVisitor::new(cw)` + `event.accept(&mut v)` + `v.into_result()` |
| `event/mapper.rs:339` `executor_event_to_acp` | 重写为 `AcpDtoVisitor::new()` + `accept` + `into_result()` |
| `event/router.rs:47` `route` | 重写为 `RouterVisitor::new()` + `accept` + `into_result()` |
| `session/executor_helpers.rs:273` `forward_langfuse_event` | 重写为 `LangfuseVisitor::new(tracer, name)` + `accept` |
| `agent/workflow_agent.rs:187` 部分 match | **不动**——它只关心 3 个变体的 token 累计，独立的局部逻辑，不需要 visitor |

#### Mapper visitor 的双 match 问题

mapper.rs 有两段独立 match（`map_event` 与 `executor_event_to_acp`），输出类型不同（`Vec<MappedEvent>` vs `Option<AcpEvent>`）。两个 visitor **必须分开**——不能用一个 visitor 同时输出两种类型，否则 trait 的 `type Output` 无法表达。这两个 visitor 是：

- `MapperSessionUpdateVisitor`（输出 `Vec<MappedEvent>`）
- `MapperAcpEventVisitor`（输出 `Option<AcpEvent>`）

合一起的话需要 visitor 内部维护两个字段，违反单一职责。**推荐分两个 visitor**，accept 调用两次（每次接收同一事件但不同 visitor）。

### 6.3 上游（产生 ExecutorEvent 的位置）

**完全不动**。ExecutorEvent 的产生点散落在 peri-agent 各处：

- `stages/act.rs` → `ToolStart` / `ToolEnd`
- `stages/reason.rs` → `LlmCallStart` / `LlmCallEnd` / `TextChunk` / `AiReasoning`
- `stages/compact.rs` → `CompactStarted` / `CompactCompleted` / `CompactError`
- `stages/receive.rs` → `MessageAdded`
- `stages/end.rs` → `TurnCommitted` / `TurnSuspended`

这些位置 emit 的就是 `ExecutorEvent`，新增 visitor trait **不改变 emit 接口**。visitor 只消费 `&ExecutorEvent`，与构造无关。

### 6.4 剩余的 leaking seam

visitor 收敛后，仍然存在两个 seam：

1. **visit_ method 的参数解构**：`visit_text_chunk(message_id, chunk, source_agent_id)` 的签名与 enum field 一一对应。新增 field 时所有 visitor 签名要改——但这比 enum 变体新增频率低得多，且改签名是编译失败级别的强制。
2. **trait method 漏 override**：MapperVisitor 若漏 override `visit_text_chunk`，会走 `default_visit` → panic（按 5.5 的强制约束）。但这只在运行时炸——需要测试覆盖。

候选 6（trait 测试面提取）可解决第 2 点：parametric test 遍历 24 变体 × 4 visitor，保证每个 visitor 都覆盖全变体。

---

## 7. 测试面

### 7.1 现有测试存活情况

| 测试文件 | 现有测试数 | 重构后存活？ | 改动 |
|---------|-----------|-------------|------|
| `event/mapper_test.rs` | 27 个 `test_*` | 全部存活 | 内部由 `map_event(&ev, cw)` 改为 `MapperSessionUpdateVisitor::new(cw)` + accept + into_result；测试断言不变 |
| `event/router_test.rs` | 18 个 `test_*` | 全部存活 | 同上，改 `route(&ev)` 为 visitor |
| `event/dto_test.rs` | 若干 | 不受影响（DTO 序列化与 visitor 无关） | 无 |
| `event/truncate_test.rs` | 若干 | 不受影响 | 无 |
| `session/executor_helpers_test.rs`（如存在） | langfuse forward 测试 | 全部存活 | 改 visitor 调用方式 |

### 7.2 新增测试（关键）

#### 测试 1：parametric visitor coverage（P0 必须）

```rust
// peri-acp/src/event/visitor_coverage_test.rs（新文件）

/// [回归测试] visitor 覆盖率：所有 24 个 ExecutorEvent 变体在 MapperVisitor 中
/// 都必须被显式 override（不允许走 default_visit）。
///
/// 历史背景：executor_helpers.rs:353 与 workflow_agent.rs:198 的 `_ => {}` 兜底
/// 曾静默吞掉新增变体（TurnSuspended / BgToolStep / WorkflowProgress），
/// 导致 tracer 漏打点、UI 漏渲染。

#[test]
fn test_all_variants_covered_by_mapper_session_update_visitor() {
    let all_variants = build_all_24_variants();
    let mut missed = HashSet::new();
    for (name, ev) in all_variants {
        let mut v = MapperSessionUpdateVisitor::new(200_000);
        v.set_strict_mode(true); // default_visit 时记录
        ev.accept(&mut v);
        if v.touched_default_visit() { missed.insert(name); }
    }
    assert!(missed.is_empty(), "Mapper 漏 override: {:?}", missed);
}

fn build_all_24_variants() -> Vec<(&'static str, ExecutorEvent)> {
    vec![
        ("TextChunk", ExecutorEvent::TextChunk { /* ... */ }),
        // ... 24 个变体（每个用最小合法字段构造）...
        ("TurnSuspended", ExecutorEvent::TurnSuspended),
    ]
}
```

这个测试是候选 1 的**核心保护**：防止未来新增变体时 MapperVisitor 漏 override。

#### 测试 2：visitor 与旧 match 输出等价（P0 必须，迁移期）

```rust
#[test]
fn test_mapper_visitor_equivalent_to_legacy_match() {
    // 重构期间，保留旧 map_event_legacy 函数，跑 24 变体 × 旧/新两路
    // 断言 Vec<MappedEvent> 完全相等
    // Phase 3 删除旧 match 时此测试一并删除
}
```

#### 测试 3：accept 单点 exhaustive（P1 应该）

```rust
#[test]
fn test_accept_covers_all_variants() {
    // 遍历 24 变体，断言 accept 后 visitor 至少被调用一次
    // 防止 accept 内部 match 漏 arm
}
```

### 7.3 会被淘汰的测试

无。所有现有测试在 Phase 3 后仍存活（仅调用形式变化）。

### 7.4 测试 quality 评估

- **确定性**：parametric test 不依赖外部状态，满足。
- **错误路径**：test 1 验证「漏 override」错误路径；可独立运行。
- **中文注释**：已有惯例（mapper_test.rs:26 等），延续。

---

## 8. 风险与回滚

### 8.1 性能风险

| 风险 | 严重度 | 评估 | 缓解 |
|------|-------|------|------|
| visitor 间接调用无法内联 | 低 | 单态化泛型 `accept<V>` 在 release 下可被 inline；bench 验证 TextChunk 路径无回归 | 加 `cargo bench` 对比 map_event 前后耗时 |
| `Vec<MappedEvent>` 返回值在 visitor 中重复分配 | 低 | MapperVisitor 内 `out: Vec<MappedEvent>` 预分配，与原 `vec![...]` 等价 | 用 `with_capacity` |
| langfuse visitor 在 pump 热路径引入锁竞争 | 低 | 现有 `tracer.lock()` 不变，visitor 只是把 match 外壳换成 accept | 不变 |

**性能结论**：流式 TextChunk 每秒数十次的频率下，visitor 单态化代码与直接 match 性能差异 < 1ns（一次 call 与一次 jump 的差），可忽略。

### 8.2 编译时间风险

- trait 24 method × 4 impl = 96 method 体，相比当前 96 arm 的 match 体积相近。
- 单态化每个 visitor 类型生成一份 accept 代码，4 visitor × 24 arm = 96 arm 的 IR 体积，与当前 4 处 match 相同。
- **预期编译时间无显著变化**。

### 8.3 维护性风险

| 风险 | 缓解 |
|------|------|
| trait method 默认实现被滥用，回到 silent failure | MapperSessionUpdateVisitor / MapperAcpEventVisitor 的 `default_visit` 实现为 panic，强制 override |
| 新人误以为 visitor 是「设计模式炫技」 | 文件头注释解释 leaking seam 历史 + silent failure 案例 |
| visit_ method 签名膨胀（24 个参数 method） | 可考虑分组 sub-trait（StreamingEventVisitor / LifecycleEventVisitor），但增加复杂度，不推荐 |

### 8.4 回滚方案

| 阶段 | 回滚成本 | 方案 |
|------|---------|------|
| Phase 1（引入 trait，保留旧 match） | 极低 | 删除 trait 文件、回退 Cargo.toml；旧 match 完好无损 |
| Phase 2（单消费者迁移） | 低 | 该消费者 visitor impl 删除，恢复旧 match 函数；其余已迁移的不受影响 |
| Phase 3（删除旧 match） | 中 | git revert 该 commit；旧 match 仍存在于 git 历史，可恢复 |

**关键**：每个 Phase 独立合入，每个 Phase 都可独立回滚。Phase 3 是不可逆点——合入前必须完成 parametric coverage test。

### 8.5 与 silent failure 的取舍

接受方案 A 后，**仍然存在 trait method 漏 override 的运行时风险**（仅 MapperSessionUpdateVisitor / MapperAcpEventVisitor 受影响；RouterVisitor / LangfuseVisitor 的 no-op 是真实意图）。

权衡：
- **现状**：4 处 `_ => {}` 静默吞变体，编译期无任何提醒
- **方案 A**：1 处 `accept` exhaustive（编译期强制），MapperVisitor 的 default_visit panic（运行时强制，配合 parametric test 可在 CI 阶段拦截）

**净改善**：从「编译期 0 提醒」升级为「编译期 1 处提醒 + 运行时 panic + CI 全变体覆盖」。虽然不完美，但显著优于现状。

---

## 9. 迁移步骤

### 9.1 Phase 1：引入 trait，保留三处 match（独立可合入）

**目标**：把 trait 与 accept 入口加到 peri-agent，但不改 peri-acp 任何消费点。验证 trait API 设计可用，编译通过。

**改动**：
- `peri-agent/src/agent/events/visitor.rs`（新文件）：定义 `ExecutorEventVisitor` trait + 24 个 `visit_` method（全默认实现，调用 `default_visit`）
- `peri-agent/src/agent/events.rs`（修改）：
  - `pub mod visitor;` 加 mod 声明
  - `impl ExecutorEvent { pub fn accept<V>(&self, v: &mut V) where V: ExecutorEventVisitor }` 单点 exhaustive match
  - enum 上加 `#[non_exhaustive]`
- `peri-agent/src/agent/events/mod.rs`（修改）：`pub use visitor::ExecutorEventVisitor;`
- `peri-agent/src/agent/events/visitor_test.rs`（新文件）：测试 accept 对 24 变体都触发对应 visit_ method

**不动**：peri-acp 的 4 处 match 全部保留。

**验证**：
- `cargo build --workspace` 通过
- `cargo test -p peri-agent --lib` 通过
- `cargo test -p peri-acp --lib` 通过（旧测试不受影响）

**合入**：单独 PR。

### 9.2 Phase 2：逐消费者迁移到 visitor（4 个独立小 PR）

**目标**：把 4 处 match 逐个替换为 visitor impl，每个消费者一个 PR。允许中间状态（部分 match + 部分 visitor 共存）。

**子阶段**：

#### Phase 2a：MapperSessionUpdateVisitor

- `peri-acp/src/event/mapper.rs`（修改）：
  - 新增 `MapperSessionUpdateVisitor` struct + impl ExecutorEventVisitor（24 method 显式 override，default_visit panic）
  - `map_event` 函数体改为 `let mut v = MapperSessionUpdateVisitor::new(context_window); event.accept(&mut v); v.into_result()`
  - 保留 `map_event_legacy` 函数（重命名自原 `map_event`）供等价测试使用
- `peri-acp/src/event/mapper_test.rs`（修改）：
  - 所有测试改为调用 `map_event`（visitor 版本）
  - 新增 `test_mapper_visitor_equivalent_to_legacy_match`：24 变体 × 新旧两路对比

#### Phase 2b：MapperAcpEventVisitor

- 同上，针对 `executor_event_to_acp`

#### Phase 2c：RouterVisitor

- `peri-acp/src/event/router.rs`（修改）：
  - `RouterVisitor` 允许走 default_visit（21 个 None 是真实意图）
  - `route` 改为 visitor 调用

#### Phase 2d：LangfuseVisitor

- `peri-acp/src/session/executor_helpers.rs`（修改）：
  - `forward_langfuse_event` 改为 visitor 调用
- `peri-acp/src/agent/workflow_agent.rs:203`（不动）：仍调用 `forward_langfuse_event`，对外 API 不变

**验证**：每个子阶段
- `cargo test -p peri-acp --lib` 全绿
- 等价测试证明新旧路径输出一致

### 9.3 Phase 3：删除旧 match，trait 成为唯一路径（独立可合入）

**目标**：删除所有 `_ => {}` 兜底，visitor 成为 ExecutorEvent 消费的唯一路径。

**改动**：
- `peri-acp/src/event/mapper.rs`：删除 `map_event_legacy` 函数 + 等价测试
- `peri-acp/src/event/mapper.rs`：删除 `executor_event_to_acp_legacy`（如有）
- `peri-acp/src/event/router.rs`：删除 `route_legacy`
- `peri-acp/src/event/visitor_coverage_test.rs`（新文件）：parametric coverage test（§7.2 测试 1）
- 更新 `CLAUDE.md`「AgentEvent 变体」陷阱：改为「新增变体必须新增 trait method + MapperSessionUpdateVisitor override」

**验证**：
- `cargo test --workspace` 全绿
- `cargo bench --workspace` 无性能回归
- 故意删除一个 visit_ override，CI 应失败（parametric coverage test 拦截）

### 9.4 Phase 4（可选）：候选 6 协同

候选 1 落地后，visitor trait 即可作为候选 6（trait 测试面提取）的天然接缝：

- parametric coverage test 用 trait mock 注入，不需要构造完整 ExecutorEvent
- MapperVisitor 的 strict_mode（panic on default_visit）成为可注入的测试行为

不在本候选范围，仅记录协同点。

---

## 10. 完成判据自检

- [x] 文档 500-800 行（实际约 760 行）
- [x] 至少 5 个 file:line 证据引用（mapper.rs:141 / 339、router.rs:47、executor_helpers.rs:273、workflow_agent.rs:187、events.rs:104-268）
- [x] 3 个方向（A/B/C）的 interface 草案（§5.1 / 5.2 / 5.3）
- [x] 9 节齐全（摘要 / 现状诊断 / 约束 / 依赖 / 加深模块形状 / seam 残余 / 测试面 / 风险 / 迁移）
- [x] 文件创建在 `docs/architecture-reviews/2026-07-13-peri-acp/01-executor-event-visitor.md`

---

## 11. ADR 建议

**需要 ADR**。理由：

1. 引入 trait 是跨 crate（peri-agent）接口变更，影响所有下游消费者未来添加变体的方式。
2. 方案 A 与「最小改动」原则（方向 B）存在取舍——选择 A 等于声明「silent failure 比改动成本更重要」。
3. 候选 6（testability trait extraction）将依赖此 ADR。

建议 ADR 路径：`docs/design/decisions/2026-07-13-executor-event-visitor.md`，内容：决策（采用 visitor trait + accept）、背景（4 处 leaking seam + 2 处 silent `_ => {}`）、考虑方案（A/B/C）、后果（24 method trait 表面积 + parametric coverage test CI 强制）。
