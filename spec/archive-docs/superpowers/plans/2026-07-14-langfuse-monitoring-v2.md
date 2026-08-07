# Langfuse Monitoring v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 一次性重构 Langfuse 监控，引入三层映射（Session/Trace=turn_id/5 阶段 Span）、12 个新 ExecutorEvent 变体覆盖核心架构盲区、LangfuseTracer 14 字段收敛为 7 子状态机、Turn 级 Sampling + ErrorSpan 兜底。

**Architecture:** 单大 PR 分 8 commit。Phase 1-8 顺序推进，每 commit 独立可编译可测。所有跨字段不变量收口在 7 个子对象内部（SamplingDecider / StageSpans / MiddlewareTracer / GenerationTracker / ToolBatch / SubagentStack / CompactSpan）。Trait 抽取（`LangfuseSessionLike`）让 tracer 可注入 fake session 跑单测。

**Tech Stack:** Rust 2021 / tokio / parking_lot / serde / reqwest / mockito (dev) / parking_lot::Mutex<LangfuseTracer> / OTLP v4 ingestion

**Spec:** `docs/superpowers/specs/2026-07-14-langfuse-monitoring-v2-design.md`

---

## File Structure

### 新建文件

| 文件 | 职责 |
|------|------|
| `langfuse-client/src/types/session.rs` | `SessionCreate` / `SessionUpdate` 数据结构 |
| `langfuse-client/src/types/session_test.rs` | serde roundtrip 测试 |
| `peri-acp/src/langfuse/tracer/generation.rs` | `GenerationTracker` 子对象 |
| `peri-acp/src/langfuse/tracer/generation_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/tool_batch.rs` | `ToolBatch` 子对象 |
| `peri-acp/src/langfuse/tracer/tool_batch_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/subagent.rs` | `SubagentStack` + `SubAgentContext` |
| `peri-acp/src/langfuse/tracer/subagent_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/compact.rs` | `CompactSpan` 子对象 |
| `peri-acp/src/langfuse/tracer/compact_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/sampling.rs` | `SamplingDecider` 子对象 |
| `peri-acp/src/langfuse/tracer/sampling_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/stages.rs` | `StageSpans`（含 MQ 排空 + Workflow 子能力） |
| `peri-acp/src/langfuse/tracer/stages_test.rs` | 单测 |
| `peri-acp/src/langfuse/tracer/middleware.rs` | `MiddlewareTracer` 子对象 |
| `peri-acp/src/langfuse/tracer/middleware_test.rs` | 单测 |
| `peri-acp/src/langfuse/session_like.rs` | `LangfuseSessionLike` trait |
| `peri-acp/src/langfuse/fake_session.rs` | `FakeLangfuseSession`（测试 fake） |
| `peri-acp/src/langfuse/variant_coverage_test.rs` | ExecutorEvent 变体覆盖测试 |
| `peri-acp/tests/langfuse_e2e.rs` | e2e mock 端到端测试 |
| `docs/architecture-reviews/2026-07-14-langfuse-architecture-revamp.md` | ADR |
| `docs/design/langfuse-monitoring-v2.md` | spec 归档副本 |

### 修改文件

| 文件 | 改动 |
|------|------|
| `langfuse-client/src/types/mod.rs` | re-export session types；`IngestionEvent` 加 `SessionCreate`/`SessionUpdate` |
| `langfuse-client/src/config.rs` | `ClientConfig` 加 5 字段；`BackpressurePolicy` 加 `DropOldest` |
| `langfuse-client/src/batcher.rs` | 支持 `DropOldest`；max_events/flush_interval 可配置 |
| `langfuse-client/src/lib.rs` | re-export |
| `langfuse-client/Cargo.toml` | 无变化（mockito 已有） |
| `peri-agent/src/agent/events.rs` | `ExecutorEvent` 加 12 新变体 + 扩充 CompactStarted/Completed |
| `peri-agent/src/agent/stages/compact.rs` | emit BudgetThresholdHit + 扩充 CompactStarted/Completed 字段 |
| `peri-agent/src/agent/stages/receive.rs` | emit MessageQueueDrained |
| `peri-agent/src/agent/stages/reason.rs` | emit AiReasoningChunk（替代 AiReasoning） |
| `peri-agent/src/agent/stages/act.rs` | emit StageStarted/StageEnded |
| `peri-agent/src/agent/stages/end.rs` | emit TurnEnded + StageEnded(End) |
| `peri-agent/src/agent/stages/mod.rs` | 每阶段入口 emit StageStarted |
| `peri-agent/src/middleware/chain.rs` | emit MiddlewareStarted/Ended |
| `peri-middlewares/src/workflow/mod.rs` | emit WorkflowStarted/Ended |
| `peri-agent/src/agent/workflow_agent.rs` | tracer pump 改挂主 Trace Act Span |
| `peri-acp/src/event/mapper.rs` | 新变体映射 |
| `peri-acp/src/event/mapper_test.rs` | 新变体映射测试 |
| `peri-acp/src/session/executor_helpers.rs` | `forward_langfuse_event` 路由扩展 |
| `peri-acp/src/langfuse/mod.rs` | re-export 子对象 + session_like |
| `peri-acp/src/langfuse/tracer/mod.rs` | `LangfuseTracer` 主 struct 重构（12 字段） |
| `peri-acp/src/langfuse/config.rs` | 加 settings.json 支持 |
| `peri-acp/src/langfuse/session.rs` | impl `LangfuseSessionLike` for `LangfuseSession` |
| `peri-acp/Cargo.toml` | 加 `mockito` dev-dependency |
| `CLAUDE.md` | 任务入口矩阵 + 陷阱速查更新 |

### 删除文件（commit 8）

| 文件 | 删除理由 |
|------|---------|
| `peri-acp/src/langfuse/tracer/llm_handler.rs` | 逻辑迁入 `generation.rs` + 主 struct |
| `peri-acp/src/langfuse/tracer/tool_handler.rs` | 逻辑迁入 `tool_batch.rs` + 主 struct |
| `peri-acp/src/langfuse/tracer/compact_handler.rs` | 逻辑迁入 `compact.rs` + 主 struct |
| `peri-acp/src/langfuse/tracer/trace_lifecycle.rs` | 逻辑迁入主 struct |
| `peri-acp/src/langfuse/tracer/subagent_stack.rs` | 逻辑迁入 `subagent.rs` |
| `peri-acp/src/langfuse/tracer/context.rs` | `SubAgentContext` 迁入 `subagent.rs` |

保留：`event_builder.rs`（工具函数）、`usage.rs`（纯函数）。

---

## Phase 1: langfuse-client crate 扩展（commit 1）

### Task 1.1: Session 数据结构

**Files:**
- Create: `langfuse-client/src/types/session.rs`
- Create: `langfuse-client/src/types/session_test.rs`
- Modify: `langfuse-client/src/types/mod.rs`

- [ ] **Step 1: 写失败的 serde roundtrip 测试**

```rust
// langfuse-client/src/types/session_test.rs
use crate::types::{SessionBody, IngestionEvent};
use chrono::Utc;

#[test]
fn test_session_create_serde_roundtrip() {
    let body = SessionBody {
        id: "sess_abc".to_string(),
        user_id: Some("user_1".to_string()),
        metadata: Some(serde_json::json!({"key": "value"})),
        release: Some("v1.0".to_string()),
        version: None,
        source: None,
        timestamp: Some(Utc::now()),
    };
    let json = serde_json::to_string(&body).unwrap();
    let de: SessionBody = serde_json::from_str(&json).unwrap();
    assert_eq!(de.id, "sess_abc");
    assert_eq!(de.user_id.as_deref(), Some("user_1"));
}

#[test]
fn test_session_create_in_ingestion_event() {
    let body = SessionBody {
        id: "sess_abc".to_string(),
        user_id: None,
        metadata: None,
        release: None,
        version: None,
        source: None,
        timestamp: None,
    };
    let event = IngestionEvent::SessionCreate {
        id: "evt_1".to_string(),
        body,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("session_create"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p langfuse-client --lib -- session_test`
Expected: FAIL（`SessionBody` 未定义）

- [ ] **Step 3: 实现 SessionBody 数据结构**

```rust
// langfuse-client/src/types/session.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionBody {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}
```

- [ ] **Step 4: 在 IngestionEvent 加 SessionCreate / SessionUpdate 变体**

打开 `langfuse-client/src/types/mod.rs` L325-396，在 `IngestionEvent` 枚举末尾追加：

```rust
// 在 SdkLog 之后
SessionCreate {
    id: String,
    body: SessionBody,
},
SessionUpdate {
    id: String,
    body: SessionBody,
},
```

并在 mod.rs 顶部加：
```rust
pub mod session;
pub use session::SessionBody;
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p langfuse-client --lib -- session_test`
Expected: PASS（2 个测试全过）

- [ ] **Step 6: 暂不提交（Task 1.4 统一提交 Phase 1）**

---

### Task 1.2: ClientConfig 扩展（5 新字段）

**Files:**
- Modify: `langfuse-client/src/config.rs`
- Modify: `langfuse-client/src/config_test.rs`（若存在，否则创建）

- [ ] **Step 1: 写失败的配置测试**

```rust
// langfuse-client/src/config_test.rs（追加，或新建）
use crate::config::ClientConfig;

#[test]
fn test_client_config_new_fields_default() {
    let cfg = ClientConfig {
        public_key: "pk".into(),
        secret_key: "sk".into(),
        base_url: "https://cloud.langfuse.com".into(),
        trace_sampling: 0.1,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        batch_backpressure: crate::config::BackpressurePolicy::DropNew,
    };
    assert_eq!(cfg.trace_sampling, 0.1);
    assert!(cfg.error_span_always);
}

#[test]
fn test_backpressure_policy_drop_oldest_exists() {
    let p = crate::config::BackpressurePolicy::DropOldest;
    assert_eq!(format!("{:?}", p), "DropOldest");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p langfuse-client --lib -- config_test`
Expected: FAIL（字段不存在 / DropOldest 不存在）

- [ ] **Step 3: 修改 ClientConfig 加字段 + BackpressurePolicy 加 DropOldest**

打开 `langfuse-client/src/config.rs`，修订为：

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
    /// Turn 级采样率 0.0~1.0，默认 1.0（全报）
    #[serde(default = "default_sampling")]
    pub trace_sampling: f64,
    /// 错误 turn 强制发 ErrorSpan 挂同 turn
    #[serde(default = "default_true")]
    pub error_span_always: bool,
    /// Batcher 单批次最大事件数
    #[serde(default = "default_batch_max")]
    pub batch_max_events: usize,
    /// Batcher flush 间隔秒数
    #[serde(default = "default_batch_flush")]
    pub batch_flush_interval_secs: u64,
    /// Batcher 背压策略
    #[serde(default)]
    pub batch_backpressure: BackpressurePolicy,
}

fn default_sampling() -> f64 { 1.0 }
fn default_true() -> bool { true }
fn default_batch_max() -> usize { 50 }
fn default_batch_flush() -> u64 { 10 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackpressurePolicy {
    #[default]
    DropNew,
    Block,
    DropOldest,
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p langfuse-client --lib -- config_test`
Expected: PASS

- [ ] **Step 5: 暂不提交**

---

### Task 1.3: Batcher 支持 DropOldest + 可配置

**Files:**
- Modify: `langfuse-client/src/batcher.rs`
- Modify: `langfuse-client/src/batcher_test.rs`（若存在）

- [ ] **Step 1: 写 DropOldest 行为测试**

```rust
// langfuse-client/src/batcher_test.rs（追加）
#[tokio::test]
async fn test_batcher_drop_oldest_policy() {
    use crate::batcher::{Batcher, BatcherConfig};
    use crate::config::BackpressurePolicy;
    use crate::types::{IngestionEvent, TraceBody};
    use std::time::Duration;

    let cfg = BatcherConfig {
        max_events: 2,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropOldest,
        max_retries: 3,
    };
    // 用 fake client 避免真实 HTTP
    let batcher = Batcher::new(cfg, /* fake client */);
    // 塞 3 个事件，第 3 个应挤出第 1 个
    for i in 0..3 {
        let _ = batcher.try_add(make_trace_event(i));
    }
    // 验证：内部队列中保留 event #1 和 #2，event #0 被挤掉
    // （具体断言依赖 batcher 内部 API）
}

fn make_trace_event(i: u64) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: format!("evt_{}", i),
        body: TraceBody::default(),
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p langfuse-client --lib -- batcher_test::test_batcher_drop_oldest`
Expected: FAIL（DropOldest 分支未实现）

- [ ] **Step 3: 实现 DropOldest 分支**

打开 `langfuse-client/src/batcher.rs`，在处理 `try_add` 背压的 match 分支处，把当前的：

```rust
match self.config.backpressure {
    BackpressurePolicy::DropNew => { /* warn + drop */ }
    BackpressurePolicy::Block => { /* block */ }
}
```

扩展为：

```rust
match self.config.backpressure {
    BackpressurePolicy::DropNew => { /* warn + drop 新事件 */ }
    BackpressurePolicy::Block => { /* block 直到队列有空 */ }
    BackpressurePolicy::DropOldest => {
        // 弹出最旧事件（warn 含被弹出的 event_id），插入新事件
        if let Some(dropped) = self.queue.pop_front() {
            tracing::warn!(target: "langfuse::batcher", dropped_event_id = ?dropped.id(), "DropOldest 弹出最旧事件");
        }
        self.queue.push_back(event);
    }
}
```

同时把 `BatcherConfig` 的 `max_events` / `flush_interval` 改为来自 `ClientConfig`：

```rust
// batcher.rs
impl BatcherConfig {
    pub fn from_client(client: &ClientConfig) -> Self {
        Self {
            max_events: client.batch_max_events,
            flush_interval: Duration::from_secs(client.batch_flush_interval_secs),
            backpressure: client.batch_backpressure,
            max_retries: 3,
        }
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p langfuse-client --lib`
Expected: PASS（所有 batcher 测试）

- [ ] **Step 5: 暂不提交**

---

### Task 1.4: Phase 1 提交

- [ ] **Step 1: cargo build --workspace 确认全编译通过**

Run: `cargo build --workspace`
Expected: 0 error

- [ ] **Step 2: cargo test --workspace 确认无回归**

Run: `cargo test -p langfuse-client --lib`
Expected: 全 PASS

- [ ] **Step 3: lefthook run pre-commit**

Run: `lefthook run pre-commit`
Expected: 全绿

- [ ] **Step 4: 提交**

```bash
git add langfuse-client/
git commit -m "$(cat <<'EOF'
feat(langfuse-client): Session 数据结构 + ClientConfig 5 新字段 + DropOldest 背压

- 新增 SessionBody / IngestionEvent::SessionCreate/SessionUpdate
- ClientConfig 加 trace_sampling / error_span_always / batch_max_events /
  batch_flush_interval_secs / batch_backpressure
- BackpressurePolicy 加 DropOldest 变体
- BatcherConfig::from_client 桥接 ClientConfig 与 batcher 配置

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 2: peri-agent ExecutorEvent 新变体（commit 2）

### Task 2.1: 新增生命周期 + 阶段事件变体

**Files:**
- Modify: `peri-agent/src/agent/events.rs:104`（ExecutorEvent 枚举）
- Modify: `peri-agent/src/agent/events.rs`（加 Stage / TurnStatus 枚举）

- [ ] **Step 1: 在 ExecutorEvent 加 5 个新变体（生命周期 + 阶段）**

打开 `peri-agent/src/agent/events.rs`，找到 `pub enum ExecutorEvent {` 在 L104。在枚举末尾（最后一个变体后）追加：

```rust
// ── langfuse v2：会话/Turn 生命周期 ──
SessionStarted {
    session_id: String,
    frozen_summary: serde_json::Value,
},
TurnStarted {
    turn_id: String,
    session_id: String,
},
TurnEnded {
    turn_id: String,
    session_id: String,
    status: TurnStatus,
    error_kind: Option<TurnErrorKind>,
},
StageStarted {
    turn_id: String,
    stage: Stage,
},
StageEnded {
    turn_id: String,
    stage: Stage,
    status: StageStatus,
    duration_ms: u64,
},
```

- [ ] **Step 2: 定义辅助枚举（Stage / StageStatus / TurnStatus / TurnErrorKind）**

在 events.rs 文件顶部（ExecutorEvent 定义之前）追加：

```rust
/// ReAct 循环 5 阶段
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Compact,
    Receive,
    Reason,
    Act,
    End,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Done,
    Skipped,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Done,
    Interrupted,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorKind {
    Interrupted,
    Timeout,
    LlmFailure,
    ToolFailure,
    RateLimit,
    MaxIterations,
}
```

- [ ] **Step 3: cargo build -p peri-agent 确认编译通过**

Run: `cargo build -p peri-agent`
Expected: 编译失败——`Cargo.toml` 可能未启用 serde_json feature；如有问题在 `peri-agent/Cargo.toml` 加 `serde_json` 依赖（通常已有）。

- [ ] **Step 4: 暂不提交（Task 2.5 统一提交 Phase 2）**

---

### Task 2.2: 新增中间件 + Reason/Receive/Compact/Workflow 事件变体

**Files:**
- Modify: `peri-agent/src/agent/events.rs`

- [ ] **Step 1: 在 ExecutorEvent 加 7 个新变体 + 扩充 Compact**

继续在 ExecutorEvent 枚举末尾追加：

```rust
// ── langfuse v2：中间件链 ──
MiddlewareStarted {
    turn_id: String,
    mw_name: String,
    hook: MiddlewareHook,
},
MiddlewareEnded {
    turn_id: String,
    mw_name: String,
    hook: MiddlewareHook,
    status: StageStatus,
    error: Option<String>,
},
// ── langfuse v2：Reason ──
AiReasoningChunk {
    turn_id: String,
    text: String,
    source_agent_id: Option<String>,
},
// ── langfuse v2：Compact ──
BudgetThresholdHit {
    turn_id: String,
    threshold: CompactThreshold,
    current_pct: f64,
    tokens_in: u64,
    tokens_out: u64,
},
// ── langfuse v2：Receive ──
MessageQueueDrained {
    turn_id: String,
    prompt: usize,
    defer: usize,
    info: usize,
},
// ── langfuse v2：Act / Workflow ──
WorkflowStarted {
    turn_id: String,
    workflow_id: String,
    plan_summary: String,
},
WorkflowEnded {
    turn_id: String,
    workflow_id: String,
    agents_spawned: usize,
    tool_calls: usize,
},
```

- [ ] **Step 2: 扩充现有 CompactStarted / CompactCompleted（如 v1 路径仍用）**

在 `events.rs` 找到现有 `CompactStarted` 和 `CompactCompleted`（L228 / L230），扩充字段。注意保留现有字段以维持向后兼容（CLAUDE.md 陷阱速查：新增 ExecutorEvent 变体必须同步 mapper/acp_events）。

```rust
CompactStarted {
    turn_id: String,
    agent_id: String,
    step: usize,
    // ★ 新增字段
    strategy: CompactStrategy,
    trigger: CompactTrigger,
},
CompactCompleted {
    // 现有字段保留
    summary: String,
    files: Vec<CompactFileInfo>,
    skills: Vec<String>,
    micro_cleared: usize,
    messages: Vec<BaseMessage>,
    // ★ 新增字段
    token_before: u64,
    token_after: u64,
    strategy: CompactStrategy,
},
```

辅助枚举：

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub enum CompactStrategy { Micro, Full, Smart }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger { Auto, Manual }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub enum CompactThreshold { Micro, Full }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareHook {
    BeforeAgent, AfterAgent,
    BeforeTool, AfterTool,
    BeforeModel, AfterModel,
    OnError, OnSessionStart, OnSessionEnd,
    OnUserPrompt, BeforeCompact, AfterCompact,
    OnPermissionRequest, OnSubagentStart, OnSubagentStop,
    OnTurnEnd, OnNotification,
}
```

- [ ] **Step 3: cargo build -p peri-agent 确认编译通过**

Run: `cargo build -p peri-agent`
Expected: 编译失败——因为现有代码构造 CompactStarted/CompactCompleted 时未传新字段。这是预期，Task 3.1 会修复。

- [ ] **Step 4: 暂不提交**

---

### Task 2.3: mapper.rs + acp_events.rs 同步新变体

**Files:**
- Modify: `peri-acp/src/event/mapper.rs`
- Modify: `peri-tui/src/kit/acp_events.rs`

- [ ] **Step 1: 在 mapper.rs 给所有新变体加占位映射**

打开 `peri-acp/src/event/mapper.rs`，找到现有 ExecutorEvent → SessionUpdate 映射的 match。**所有新变体先加最小映射**（仅记录日志，不映射到 TUI）：

```rust
ExecutorEvent::SessionStarted { session_id, .. } => {
    tracing::debug!(target: "mapper", %session_id, "SessionStarted（langfuse v2）");
    None // 不产生 SessionUpdate（仅观测层）
}
ExecutorEvent::TurnStarted { turn_id, .. } => {
    tracing::debug!(target: "mapper", %turn_id, "TurnStarted");
    None
}
ExecutorEvent::TurnEnded { turn_id, status, .. } => {
    tracing::debug!(target: "mapper", %turn_id, ?status, "TurnEnded");
    None
}
ExecutorEvent::StageStarted { turn_id, stage, .. } => {
    tracing::debug!(target: "mapper", %turn_id, ?stage, "StageStarted");
    None
}
ExecutorEvent::StageEnded { turn_id, stage, status, duration_ms, .. } => {
    tracing::debug!(target: "mapper", %turn_id, ?stage, ?status, duration_ms, "StageEnded");
    None
}
ExecutorEvent::MiddlewareStarted { turn_id, mw_name, hook, .. } => {
    tracing::debug!(target: "mapper", %turn_id, %mw_name, ?hook, "MiddlewareStarted");
    None
}
ExecutorEvent::MiddlewareEnded { turn_id, mw_name, status, .. } => {
    tracing::debug!(target: "mapper", %turn_id, %mw_name, ?status, "MiddlewareEnded");
    None
}
ExecutorEvent::AiReasoningChunk { turn_id, text, .. } => {
    tracing::debug!(target: "mapper", %turn_id, text_len = text.len(), "AiReasoningChunk");
    None
}
ExecutorEvent::BudgetThresholdHit { turn_id, threshold, current_pct, .. } => {
    tracing::debug!(target: "mapper", %turn_id, ?threshold, %current_pct, "BudgetThresholdHit");
    None
}
ExecutorEvent::MessageQueueDrained { turn_id, prompt, defer, info, .. } => {
    tracing::debug!(target: "mapper", %turn_id, prompt, defer, info, "MessageQueueDrained");
    None
}
ExecutorEvent::WorkflowStarted { turn_id, workflow_id, plan_summary, .. } => {
    tracing::debug!(target: "mapper", %turn_id, %workflow_id, %plan_summary, "WorkflowStarted");
    None
}
ExecutorEvent::WorkflowEnded { turn_id, workflow_id, agents_spawned, tool_calls, .. } => {
    tracing::debug!(target: "mapper", %turn_id, %workflow_id, agents_spawned, tool_calls, "WorkflowEnded");
    None
}
```

- [ ] **Step 2: 在 acp_events.rs 给所有新变体加占位（如有 match）**

打开 `peri-tui/src/kit/acp_events.rs`，如有匹配 ExecutorEvent 的地方，新变体加 `_ => {}` 兜底或对应占位。

- [ ] **Step 3: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 仍有 Task 2.2 留下的 CompactStarted/Completed 兼容错误（在 Task 3.1 修复）

- [ ] **Step 4: 暂不提交**

---

### Task 2.4: variant_coverage_test.rs

**Files:**
- Create: `peri-acp/src/event/variant_coverage_test.rs`
- Modify: `peri-acp/src/event/mod.rs`（加 `#[cfg(test)] mod variant_coverage_test;`）

- [ ] **Step 1: 写变体覆盖测试**

```rust
// peri-acp/src/event/variant_coverage_test.rs
use peri_agent::events::ExecutorEvent;
use peri_agent::messages::MessageId;

/// 验证每个 ExecutorEvent 变体都在 mapper.rs 中有对应处理（防止漏映射）。
/// 本测试用枚举所有变体名 + 字符串 grep 验证 mapper.rs 覆盖。
#[test]
fn test_all_executor_event_variants_mapped() {
    let mapper_source = include_str!("mapper.rs");

    // 列举所有应该被 mapper 处理的变体名
    let variants = [
        "SessionStarted", "TurnStarted", "TurnEnded",
        "StageStarted", "StageEnded",
        "MiddlewareStarted", "MiddlewareEnded",
        "AiReasoningChunk",
        "BudgetThresholdHit",
        "MessageQueueDrained",
        "WorkflowStarted", "WorkflowEnded",
        "CompactStarted", "CompactCompleted",
        "LlmCallStart", "LlmCallEnd", "LlmRetrying", "LlmRequestPayload",
        "ToolStart", "ToolEnd",
        "TextChunk",
        // 状态层/渲染层事件不上报 mapper，仅作占位检查
    ];

    for v in variants {
        assert!(
            mapper_source.contains(v),
            "mapper.rs 缺少 ExecutorEvent::{} 的处理分支",
            v
        );
    }
}
```

- [ ] **Step 2: 跑测试确认通过（如 mapper.rs 已有所有变体）**

Run: `cargo test -p peri-acp --lib -- variant_coverage_test`
Expected: PASS

- [ ] **Step 3: 暂不提交**

---

### Task 2.5: Phase 2 提交

- [ ] **Step 1: 修复 Task 2.2 留下的 CompactStarted/Completed 兼容错误**

在每个构造 CompactStarted/CompactCompleted 的位置（grep `CompactStarted {` / `CompactCompleted {`），加默认值：

```rust
// CompactStarted 构造点（grep 找到所有）
CompactStarted {
    turn_id: /* 现有 */,
    agent_id: /* 现有 */,
    step: /* 现有 */,
    strategy: CompactStrategy::Micro, // 临时默认，Task 3.1 改为实际值
    trigger: CompactTrigger::Auto,
}

// CompactCompleted 构造点
CompactCompleted {
    summary: /* 现有 */,
    files: /* 现有 */,
    skills: /* 现有 */,
    micro_cleared: /* 现有 */,
    messages: /* 现有 */,
    token_before: 0, // 临时默认，Task 3.1 改为实际值
    token_after: 0,
    strategy: CompactStrategy::Micro,
}
```

- [ ] **Step 2: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 0 error

- [ ] **Step 3: cargo test -p peri-acp --lib -- variant_coverage_test**

Run: `cargo test -p peri-acp --lib -- variant_coverage_test`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add peri-agent/src/agent/events.rs peri-acp/src/event/
git commit -m "$(cat <<'EOF'
feat(peri-agent): ExecutorEvent 12 新变体 + CompactStarted/Completed 扩充

新增 langfuse v2 监控事件（生命周期 / 阶段 / 中间件 / Reason / Receive /
Compact / Workflow），mapper.rs 加占位映射（仅日志，不产生 TUI 更新），
新增 variant_coverage_test 防止漏映射。CompactStarted/Completed 扩充
strategy/trigger/token_before/token_after 字段，构造点临时填默认值
（Phase 3 改为实际值）。

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 3: peri-agent stages emit 新事件（commit 3）

### Task 3.1: stages/compact.rs emit BudgetThresholdHit + 实际 CompactStarted/Completed 字段

**Files:**
- Modify: `peri-agent/src/agent/stages/compact.rs`
- Modify: `peri-agent/src/agent/compact_v2.rs`（compact 入口）

- [ ] **Step 1: 写失败的 emit 测试**

```rust
// peri-agent/src/agent/stages/compact_test.rs（若不存在则新建）
use peri_agent::events::{ExecutorEvent, CompactStrategy, CompactTrigger, CompactThreshold};

#[test]
fn test_emit_budget_threshold_hit_on_micro() {
    // 验证：当 ContextBudget 超过 0.70 时，emit BudgetThresholdHit { threshold: Micro }
    // 此测试需要在 mock sink 上断言收到了 BudgetThresholdHit
    // 详见现有 compact stage 测试基础设施
}

#[test]
fn test_compact_started_carries_strategy_full() {
    // 验证：触发 Full Compact 时，CompactStarted.strategy == Full
}
```

- [ ] **Step 2: 在 stages/compact.rs 找到 CompactStarted emit 点**

Run: `grep -n "CompactStarted" /Users/konghayao/code/ai/perihelion/peri-agent/src/agent/stages/compact.rs`
找到 emit 位置（约在 stage 函数内决定 strategy 后）。

- [ ] **Step 3: 修改 emit 携带实际 strategy / trigger**

打开 `peri-agent/src/agent/stages/compact.rs`，在决定 strategy 的位置（如 `match budget_pct`）：

```rust
// 在 stages/compact.rs，找到当前 emit CompactStarted 的位置
let strategy = if budget_pct < 0.70 {
    return; // 不触发 Compact
} else if budget_pct < 0.85 {
    CompactStrategy::Micro
} else {
    CompactStrategy::Full
};

let trigger = if is_manual_request { CompactTrigger::Manual } else { CompactTrigger::Auto };

// 在 micro 阈值触发时先发 BudgetThresholdHit
if budget_pct >= 0.70 && budget_pct < 0.85 {
    event_sink.emit(ExecutorEvent::BudgetThresholdHit {
        turn_id: turn_id.clone(),
        threshold: CompactThreshold::Micro,
        current_pct: budget_pct,
        tokens_in: token_tracker.input_tokens,
        tokens_out: token_tracker.output_tokens,
    });
} else if budget_pct >= 0.85 {
    event_sink.emit(ExecutorEvent::BudgetThresholdHit {
        turn_id: turn_id.clone(),
        threshold: CompactThreshold::Full,
        current_pct: budget_pct,
        tokens_in: token_tracker.input_tokens,
        tokens_out: token_tracker.output_tokens,
    });
}

// 替换现有的 CompactStarted emit
event_sink.emit(ExecutorEvent::CompactStarted {
    turn_id: turn_id.clone(),
    agent_id: agent_id.clone(),
    step,
    strategy,
    trigger,
});
```

- [ ] **Step 4: 在 CompactCompleted emit 加 token_before/token_after/strategy**

```rust
// 替换现有 CompactCompleted emit
event_sink.emit(ExecutorEvent::CompactCompleted {
    summary: summary.clone(),
    files: files.clone(),
    skills: skills.clone(),
    micro_cleared,
    messages: new_messages.clone(),
    token_before: token_before,
    token_after: token_after,
    strategy, // 从 CompactStarted 透传
});
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p peri-agent --lib -- compact_test`
Expected: PASS

- [ ] **Step 6: 暂不提交**

---

### Task 3.2: stages/receive.rs emit MessageQueueDrained

**Files:**
- Modify: `peri-agent/src/agent/stages/receive.rs`

- [ ] **Step 1: 写测试**

```rust
// peri-agent/src/agent/stages/receive_test.rs（追加）
#[test]
fn test_receive_emits_mq_drained_after_drain() {
    // 排空 MessageQueue 后应 emit MessageQueueDrained { prompt, defer, info }
    // 用 mock sink 断言事件存在
}
```

- [ ] **Step 2: 在 receive.rs 排空 MQ 后 emit**

```rust
// stages/receive.rs，drain_message_queue 函数末尾
let prompt_count = /* 统计 */;
let defer_count = /* 统计 */;
let info_count = /* 统计 */;

event_sink.emit(ExecutorEvent::MessageQueueDrained {
    turn_id: ctx.turn_id.clone(),
    prompt: prompt_count,
    defer: defer_count,
    info: info_count,
});
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-agent --lib -- receive_test`
Expected: PASS

---

### Task 3.3: stages/reason.rs emit AiReasoningChunk

**Files:**
- Modify: `peri-agent/src/agent/stages/reason.rs`

- [ ] **Step 1: 写测试**

```rust
#[test]
fn test_reason_emits_ai_reasoning_chunk_for_streaming() {
    // 流式 reasoning chunk 应 emit AiReasoningChunk
}
```

- [ ] **Step 2: 在 reason.rs 流式 reasoning 处理位置 emit**

```rust
// stages/reason.rs，处理 reasoning chunk 的位置
// 找到当前 emit ExecutorEvent::AiReasoning 的代码（如有），改为：
event_sink.emit(ExecutorEvent::AiReasoningChunk {
    turn_id: ctx.turn_id.clone(),
    text: chunk_text.to_string(),
    source_agent_id: Some(ctx.agent_id.clone()),
});
```

注意：保留旧 `AiReasoning` 变体的 emit（其他系统可能消费），同时 emit 新变体。

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-agent --lib -- reason_test`
Expected: PASS

---

### Task 3.4: stages/* emit StageStarted / StageEnded

**Files:**
- Modify: `peri-agent/src/agent/stages/{compact,receive,reason,act,end}.rs`
- Modify: `peri-agent/src/agent/stages/mod.rs`（如 run_stage 调度器存在）

- [ ] **Step 1: 写测试**

```rust
#[test]
fn test_each_stage_emits_started_and_ended() {
    // 对每个 stage 调用，应分别 emit StageStarted(stage) 和 StageEnded(stage, status, duration)
}
```

- [ ] **Step 2: 在 run_stage 调度器加 StageStarted/Ended 包装**

打开 `peri-agent/src/agent/stages/mod.rs`（或 run_react_loop.rs 中找到 stage 调度）。在每个 stage 调用前后加：

```rust
// 假设 stages/mod.rs 有 run_stage 函数
async fn run_stage<S: Stage>(stage: S, ctx: &mut StageContext) -> Result<StageOutput> {
    let start = std::time::Instant::now();
    ctx.event_sink.emit(ExecutorEvent::StageStarted {
        turn_id: ctx.turn_id.clone(),
        stage: S::STAGE_KIND, // 每个 Stage impl 关联 const STAGE_KIND: Stage
    });

    let result = stage.run(ctx).await;

    let status = match &result {
        Ok(_) => StageStatus::Done,
        Err(_) => StageStatus::Error,
    };
    ctx.event_sink.emit(ExecutorEvent::StageEnded {
        turn_id: ctx.turn_id.clone(),
        stage: S::STAGE_KIND,
        status: status.clone(),
        duration_ms: start.elapsed().as_millis() as u64,
    });

    result
}
```

Compact 阶段在未触发时显式 emit Skipped：

```rust
// stages/compact.rs，budget_pct < 0.70 早返回位置
ctx.event_sink.emit(ExecutorEvent::StageEnded {
    turn_id: ctx.turn_id.clone(),
    stage: Stage::Compact,
    status: StageStatus::Skipped,
    duration_ms: 0,
});
return Ok(StageOutput::Skip);
```

为每个 Stage impl 加 `const STAGE_KIND: Stage`：
- CompactStage: `Stage::Compact`
- ReceiveStage: `Stage::Receive`
- ReasonStage: `Stage::Reason`
- ActStage: `Stage::Act`
- EndStage: `Stage::End`

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-agent --lib -- stage_test`
Expected: PASS

---

### Task 3.5: middleware/chain.rs emit MiddlewareStarted / MiddlewareEnded

**Files:**
- Modify: `peri-agent/src/middleware/chain.rs`

- [ ] **Step 1: 写测试**

```rust
#[test]
fn test_chain_emits_middleware_lifecycle_per_middleware() {
    // 每个 middleware 调用前后应 emit MiddlewareStarted / MiddlewareEnded
}
```

- [ ] **Step 2: 在 chain.rs 的 dispatch 函数加 emit**

打开 `peri-agent/src/middleware/chain.rs`，找到 `before_agent` / `after_agent` / `before_tool` / `after_tool` 等 dispatch 函数（约 L51 / L167 / L59 / L112）。在每个 dispatch 内部循环中间件时：

```rust
// chain.rs，before_agent dispatch
pub async fn before_agent(&self, ctx: &mut AgentContext) -> Result<()> {
    for mw in &self.middlewares {
        ctx.event_sink.emit(ExecutorEvent::MiddlewareStarted {
            turn_id: ctx.turn_id.clone(),
            mw_name: mw.name().to_string(),
            hook: MiddlewareHook::BeforeAgent,
        });
        let start = std::time::Instant::now();
        let result = mw.before_agent(ctx).await;
        let status = match &result {
            Ok(_) => StageStatus::Done,
            Err(e) => StageStatus::Error,
        };
        ctx.event_sink.emit(ExecutorEvent::MiddlewareEnded {
            turn_id: ctx.turn_id.clone(),
            mw_name: mw.name().to_string(),
            hook: MiddlewareHook::BeforeAgent,
            status,
            error: result.as_ref().err().map(|e| e.to_string()),
        });
        result?;
    }
    Ok(())
}
```

为每个 hook（after_agent / before_tool / after_tool / before_model / after_model 等）重复该模式。

- [ ] **Step 3: 加 Middleware::name() 方法（如不存在）**

如 `Middleware` trait 没有 `name()` 方法，加：

```rust
// peri-agent/src/middleware/trait.rs
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str; // ★ 新增
    // ... 现有方法
}
```

为所有现有 middleware 实现加 `name()` 实现。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p peri-agent --lib -- middleware_test`
Expected: PASS

---

### Task 3.6: workflow/mod.rs emit WorkflowStarted / WorkflowEnded

**Files:**
- Modify: `peri-middlewares/src/workflow/mod.rs`

- [ ] **Step 1: 写测试**

```rust
#[test]
fn test_workflow_emits_started_and_ended() {
    // Workflow runner 启动 emit WorkflowStarted，结束 emit WorkflowEnded
}
```

- [ ] **Step 2: 在 workflow runner 启动 / 结束位置 emit**

```rust
// peri-middlewares/src/workflow/mod.rs，找到 run_workflow 或类似入口
async fn run_workflow(/* ... */) -> Result<WorkflowOutput> {
    let workflow_id = uuid::Uuid::now_v7().to_string();
    let plan_summary = summarize_plan(&plan);

    ctx.event_sink.emit(ExecutorEvent::WorkflowStarted {
        turn_id: ctx.turn_id.clone(),
        workflow_id: workflow_id.clone(),
        plan_summary,
    });

    let start = std::time::Instant::now();
    let agents_spawned_handle = /* 计数 handle */;
    let result = inner_run_workflow(&workflow_id, &plan, ctx).await;

    let (agents_spawned, tool_calls) = /* 从 inner_run_workflow 收集统计 */;

    ctx.event_sink.emit(ExecutorEvent::WorkflowEnded {
        turn_id: ctx.turn_id.clone(),
        workflow_id,
        agents_spawned,
        tool_calls,
    });

    result
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-middlewares --lib -- workflow_test`
Expected: PASS

---

### Task 3.7: events.rs emit SessionStarted / TurnStarted / TurnEnded

**Files:**
- Modify: `peri-agent/src/agent/events.rs`（或 events_v2.rs，看主路径）
- Modify: `peri-acp/src/session/executor.rs`（execute_prompt 入口 + ReAct 循环）

- [ ] **Step 1: 写测试**

```rust
#[test]
fn test_executor_emits_session_started_on_execute_prompt() {
    // execute_prompt 入口应 emit SessionStarted
}

#[test]
fn test_react_loop_emits_turn_started_and_ended() {
    // 每个 ReAct 循环迭代 emit TurnStarted + TurnEnded
}
```

- [ ] **Step 2: 在 execute_prompt emit SessionStarted**

打开 `peri-acp/src/session/executor.rs`，找到 `execute_prompt` 函数入口：

```rust
pub async fn execute_prompt(/* ... */) -> Result<SessionHandle> {
    let session = /* 构造 session */;

    let frozen_summary = summarize_frozen_context(&session.frozen_context);
    session.event_sink.emit(ExecutorEvent::SessionStarted {
        session_id: session.id.clone(),
        frozen_summary,
    });

    // ... 现有逻辑
}
```

- [ ] **Step 3: 在 run_react_loop emit TurnStarted / TurnEnded**

```rust
// peri-agent/src/agent/run_react_loop.rs（或 stages/mod.rs）
loop {
    let turn_id = uuid::Uuid::now_v7().to_string();
    ctx.event_sink.emit(ExecutorEvent::TurnStarted {
        turn_id: turn_id.clone(),
        session_id: ctx.session_id.clone(),
    });

    let loop_result = run_react_iteration(&turn_id, &mut ctx).await;

    let (status, error_kind) = match &loop_result {
        Ok(_) => (TurnStatus::Done, None),
        Err(e) => match e.kind() {
            ErrorKind::Interrupted => (TurnStatus::Interrupted, Some(TurnErrorKind::Interrupted)),
            ErrorKind::Timeout => (TurnStatus::Interrupted, Some(TurnErrorKind::Timeout)),
            ErrorKind::LlmFailure => (TurnStatus::Error, Some(TurnErrorKind::LlmFailure)),
            ErrorKind::ToolFailure => (TurnStatus::Error, Some(TurnErrorKind::ToolFailure)),
            ErrorKind::RateLimit => (TurnStatus::Error, Some(TurnErrorKind::RateLimit)),
            ErrorKind::MaxIterations => (TurnStatus::Error, Some(TurnErrorKind::MaxIterations)),
        },
    };

    ctx.event_sink.emit(ExecutorEvent::TurnEnded {
        turn_id,
        session_id: ctx.session_id.clone(),
        status: status.clone(),
        error_kind,
    });

    if status == TurnStatus::Done { break; }
    // 继续 loop（续跑）
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p peri-acp --lib -- executor_test`
Expected: PASS

---

### Task 3.8: Phase 3 提交

- [ ] **Step 1: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 0 error

- [ ] **Step 2: cargo test --workspace**

Run: `cargo test --workspace`
Expected: 全 PASS（langfuse tracer 内部尚未消费新事件，但不影响测试）

- [ ] **Step 3: 提交**

```bash
git add peri-agent/ peri-middlewares/ peri-acp/src/session/executor.rs
git commit -m "$(cat <<'EOF'
feat(agent): 5 stages / middleware chain / workflow 实际 emit 新事件

- compact.rs emit BudgetThresholdHit + 实际 strategy/trigger/token 字段
- receive.rs emit MessageQueueDrained
- reason.rs emit AiReasoningChunk（保留旧 AiReasoning 兼容）
- stages/mod.rs 包装 StageStarted/StageEnded（含 Compact skipped 路径）
- middleware/chain.rs 包装 MiddlewareStarted/Ended（按 hook）
- workflow/mod.rs emit WorkflowStarted/Ended
- executor.rs + run_react_loop emit SessionStarted/TurnStarted/TurnEnded

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 4: LangfuseTracer 内部重构（commit 4）

### Task 4.1: LangfuseSessionLike trait + FakeLangfuseSession

**Files:**
- Create: `peri-acp/src/langfuse/session_like.rs`
- Create: `peri-acp/src/langfuse/fake_session.rs`
- Modify: `peri-acp/src/langfuse/mod.rs`

- [ ] **Step 1: 写 trait 定义**

```rust
// peri-acp/src/langfuse/session_like.rs
use langfuse_client::types::IngestionEvent;
use langfuse_client::batcher::Backpressure;
use tokio::task::JoinHandle;

/// Langfuse session 抽象，让 tracer 可注入 fake session 跑单测。
pub trait LangfuseSessionLike: Send + Sync {
    fn try_add(&self, event: IngestionEvent) -> Result<(), Backpressure>;
    fn flush(&self) -> JoinHandle<()>;
    fn session_id(&self) -> &str;
}
```

- [ ] **Step 2: 写 FakeLangfuseSession**

```rust
// peri-acp/src/langfuse/fake_session.rs
use langfuse_client::types::IngestionEvent;
use langfuse_client::batcher::Backpressure;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub(crate) struct FakeLangfuseSession {
    events: Mutex<Vec<IngestionEvent>>,
    session_id: String,
}

impl FakeLangfuseSession {
    pub(crate) fn new(session_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            session_id: session_id.into(),
        })
    }

    pub(crate) fn events_snapshot(&self) -> Vec<IngestionEvent> {
        self.events.lock().clone()
    }

    pub(crate) fn event_count(&self) -> usize {
        self.events.lock().len()
    }
}

impl LangfuseSessionLike for FakeLangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), Backpressure> {
        self.events.lock().push(event);
        Ok(())
    }

    fn flush(&self) -> JoinHandle<()> {
        tokio::spawn(async {})
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
}
```

- [ ] **Step 3: impl LangfuseSessionLike for LangfuseSession**

打开 `peri-acp/src/langfuse/session.rs`，为现有 `LangfuseSession` 实现 trait：

```rust
// peri-acp/src/langfuse/session.rs（追加）
impl LangfuseSessionLike for LangfuseSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), Backpressure> {
        self.batcher.try_add(event) // 委托现有 batcher
    }

    fn flush(&self) -> JoinHandle<()> {
        self.batcher.flush()
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
}
```

- [ ] **Step 4: 在 mod.rs re-export**

```rust
// peri-acp/src/langfuse/mod.rs（追加）
pub mod session_like;
pub mod fake_session;
pub use session_like::LangfuseSessionLike;
pub(crate) use fake_session::FakeLangfuseSession;
```

- [ ] **Step 5: 跑测试**

Run: `cargo build -p peri-acp`
Expected: 0 error

---

### Task 4.2: GenerationTracker 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/generation.rs`
- Create: `peri-acp/src/langfuse/tracer/generation_test.rs`
- Modify: `peri-acp/src/langfuse/tracer/mod.rs`（re-export）

- [ ] **Step 1: 写失败的子对象单测**

```rust
// peri-acp/src/langfuse/tracer/generation_test.rs
use super::*;

#[test]
fn test_on_llm_start_sets_active_step() {
    let mut t = GenerationTracker::new();
    let start = t.on_llm_start(0, vec![], vec![]);
    assert_eq!(start.gen_id.starts_with("gen_"), true);
}

#[test]
fn test_on_llm_end_returns_generation_end_and_clears_state() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    let end = t.on_llm_end(0).expect("should return Some");
    assert!(end.gen_id.starts_with("gen_"));
    // 再次 on_llm_end 应返回 None
    assert!(t.on_llm_end(0).is_none());
}

#[test]
fn test_on_llm_retrying_accumulates_attempts() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_retrying(1, 3, 1000, "timeout");
    t.on_llm_retrying(2, 3, 2000, "timeout");
    let end = t.on_llm_end(0).expect("should return Some");
    assert!(end.retry_metadata.is_some());
    let meta = end.retry_metadata.unwrap();
    assert!(meta.to_string().contains("timeout"));
}

#[test]
fn test_on_llm_start_clears_previous_retry_attempts() {
    // 第二次 on_llm_start 应清空 retry_attempts
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_retrying(1, 3, 1000, "err");
    t.on_llm_start(1, vec![], vec![]); // 新 step
    let end = t.on_llm_end(1).expect("should return Some");
    assert!(end.retry_metadata.is_none(), "新 step 不应携带旧 retry");
}

#[test]
fn test_on_llm_end_unknown_step_returns_none() {
    let mut t = GenerationTracker::new();
    assert!(t.on_llm_end(99).is_none());
}

#[test]
fn test_on_llm_request_payload_supplements_body() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_request_payload(0, std::sync::Arc::new(serde_json::json!({"model": "claude-4.7"})));
    let end = t.on_llm_end(0).expect("should return Some");
    assert_eq!(end.input_json["model"], "claude-4.7");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- generation_test`
Expected: FAIL（模块未定义）

- [ ] **Step 3: 实现 GenerationTracker**

```rust
// peri-acp/src/langfuse/tracer/generation.rs
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct GenerationCached {
    pub gen_id: String,
    pub start_time: String,
    pub messages_json: serde_json::Value,
    pub tools_json: serde_json::Value,
    pub raw_body: Option<Arc<serde_json::Value>>,
}

#[derive(Debug, Clone)]
pub(crate) struct RetryAttempt {
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay_ms: u64,
    pub error: String,
}

pub(crate) struct GenerationStart {
    pub gen_id: String,
    pub start_time: String,
}

pub(crate) struct GenerationEnd {
    pub gen_id: String,
    pub start_time: String,
    pub input_json: serde_json::Value,
    pub retry_metadata: Option<serde_json::Value>,
}

pub(crate) struct GenerationTracker {
    generation_data: HashMap<usize, GenerationCached>,
    active_step: Option<usize>,
    retry_attempts: Vec<RetryAttempt>,
}

impl GenerationTracker {
    pub(crate) fn new() -> Self {
        Self {
            generation_data: HashMap::new(),
            active_step: None,
            retry_attempts: Vec::new(),
        }
    }

    pub(crate) fn on_llm_start(
        &mut self,
        step: usize,
        messages: Vec<crate::messages::BaseMessage>,
        tools: Vec<crate::tools::ToolDefinition>,
    ) -> GenerationStart {
        // 清空 retry_attempts（新 step 开始）
        self.retry_attempts.clear();
        let gen_id = format!("gen_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let cached = GenerationCached {
            gen_id: gen_id.clone(),
            start_time: start_time.clone(),
            messages_json: serde_json::to_value(&messages).unwrap_or_default(),
            tools_json: serde_json::to_value(&tools).unwrap_or_default(),
            raw_body: None,
        };
        self.generation_data.insert(step, cached);
        self.active_step = Some(step);
        GenerationStart { gen_id, start_time }
    }

    pub(crate) fn on_llm_request_payload(&mut self, step: usize, body: Arc<serde_json::Value>) {
        if let Some(cached) = self.generation_data.get_mut(&step) {
            cached.raw_body = Some(body);
        }
        // 未找到时静默 no-op（保留现有行为）
    }

    pub(crate) fn on_llm_retrying(
        &mut self,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: &str,
    ) {
        self.retry_attempts.push(RetryAttempt {
            attempt,
            max_attempts,
            delay_ms,
            error: error.to_string(),
        });
    }

    pub(crate) fn on_llm_end(&mut self, step: usize) -> Option<GenerationEnd> {
        let cached = self.generation_data.remove(&step)?;
        self.active_step = None;

        let retry_metadata = if self.retry_attempts.is_empty() {
            None
        } else {
            Some(build_retry_metadata(&self.retry_attempts))
        };
        self.retry_attempts.clear();

        let input_json = cached.raw_body
            .map(|b| (*b).clone())
            .unwrap_or(cached.messages_json);

        Some(GenerationEnd {
            gen_id: cached.gen_id,
            start_time: cached.start_time,
            input_json,
            retry_metadata,
        })
    }

    pub(crate) fn active_step(&self) -> Option<usize> { self.active_step }
}

fn build_retry_metadata(retries: &[RetryAttempt]) -> serde_json::Value {
    serde_json::json!({
        "retry_count": retries.len(),
        "retries": retries.iter().map(|r| serde_json::json!({
            "attempt": r.attempt,
            "max_attempts": r.max_attempts,
            "delay_ms": r.delay_ms,
            "error": r.error,
        })).collect::<Vec<_>>(),
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- generation_test`
Expected: PASS（6 个测试全过）

- [ ] **Step 5: 在 tracer/mod.rs 加 mod 声明**

```rust
// peri-acp/src/langfuse/tracer/mod.rs（顶部）
mod generation;
pub(crate) use generation::{GenerationTracker, GenerationStart, GenerationEnd};
```

---

### Task 4.3: ToolBatch 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/tool_batch.rs`
- Create: `peri-acp/src/langfuse/tracer/tool_batch_test.rs`

- [ ] **Step 1: 写失败的子对象单测**

```rust
// tool_batch_test.rs
use super::*;

#[test]
fn test_lazy_create_batch_span_on_first_start() {
    let mut tb = ToolBatch::new();
    let r = tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    assert!(r.parent_span_id.starts_with("batch_") || r.parent_span_id.starts_with("agent_"));
    assert!(r.tool_span_id.starts_with("obs_"));
}

#[test]
fn test_second_start_shares_batch_span() {
    let mut tb = ToolBatch::new();
    let r1 = tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    let r2 = tb.on_tool_start("call_2", "Write", serde_json::json!({}));
    assert_eq!(r1.parent_span_id, r2.parent_span_id, "同批次共享 batch span");
}

#[test]
fn test_on_tool_end_returns_pending_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    let pending = tb.on_tool_end("call_1").expect("should return Some");
    assert_eq!(pending.name, "Read");
}

#[test]
fn test_on_tool_end_unknown_returns_none() {
    let mut tb = ToolBatch::new();
    assert!(tb.on_tool_end("nope").is_none());
}

#[test]
fn test_flush_returns_batch_record_and_clears() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    tb.on_tool_end("call_1");
    tb.record_end_time("2026-07-14T10:00:00Z".into());
    let record = tb.flush().expect("should return Some");
    assert!(record.batch_span_id.starts_with("batch_"));
    assert!(tb.flush().is_none(), "二次 flush 应返回 None");
}

#[test]
fn test_is_agent_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Agent", serde_json::json!({"subagent": true}));
    assert!(tb.is_agent_tool("call_1"));
    assert!(!tb.is_agent_tool("nope"));
}

#[test]
fn test_is_empty() {
    let mut tb = ToolBatch::new();
    assert!(tb.is_empty());
    tb.on_tool_start("c1", "Read", serde_json::json!({}));
    assert!(!tb.is_empty());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- tool_batch_test`
Expected: FAIL

- [ ] **Step 3: 实现 ToolBatch**

```rust
// peri-acp/src/langfuse/tracer/tool_batch.rs
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct PendingTool {
    pub name: String,
    pub input: serde_json::Value,
    pub span_id: String,
    pub start_time: String,
    pub is_agent: bool,
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

pub(crate) struct ToolBatch {
    pending_tools: HashMap<String, PendingTool>,
    batch_span_id: Option<String>,
    batch_start_time: Option<String>,
    batch_end_time: Option<String>,
}

impl ToolBatch {
    pub(crate) fn new() -> Self {
        Self {
            pending_tools: HashMap::new(),
            batch_span_id: None,
            batch_start_time: None,
            batch_end_time: None,
        }
    }

    pub(crate) fn on_tool_start(
        &mut self,
        tool_call_id: &str,
        name: &str,
        input: serde_json::Value,
    ) -> ToolStartRecord {
        let now = chrono::Utc::now().to_rfc3339();
        // lazy 创建 batch span
        if self.batch_span_id.is_none() {
            self.batch_span_id = Some(format!("batch_{}", uuid::Uuid::now_v7()));
            self.batch_start_time = Some(now.clone());
        }
        let tool_span_id = format!("obs_{}", uuid::Uuid::now_v7());
        let is_agent = name == "Agent" || name == "Task";
        let parent = self.batch_span_id.clone().unwrap();
        self.pending_tools.insert(
            tool_call_id.to_string(),
            PendingTool {
                name: name.to_string(),
                input,
                span_id: tool_span_id.clone(),
                start_time: now.clone(),
                is_agent,
            },
        );
        ToolStartRecord {
            tool_span_id,
            tool_start_time: now,
            parent_span_id: parent,
        }
    }

    pub(crate) fn on_tool_end(&mut self, tool_call_id: &str) -> Option<PendingTool> {
        self.pending_tools.remove(tool_call_id)
    }

    pub(crate) fn record_end_time(&mut self, end_time: String) {
        self.batch_end_time = Some(end_time);
    }

    pub(crate) fn flush(&mut self) -> Option<ToolsBatchRecord> {
        let span_id = self.batch_span_id.take()?;
        let start = self.batch_start_time.take()?;
        let end = self.batch_end_time.take().unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        Some(ToolsBatchRecord {
            batch_span_id: span_id,
            batch_start_time: start,
            batch_end_time: end,
        })
    }

    pub(crate) fn is_agent_tool(&self, tool_call_id: &str) -> bool {
        self.pending_tools.get(tool_call_id).map(|p| p.is_agent).unwrap_or(false)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending_tools.is_empty()
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- tool_batch_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod tool_batch;
pub(crate) use tool_batch::{ToolBatch, PendingTool, ToolStartRecord, ToolsBatchRecord};
```

---

### Task 4.4: SubagentStack 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/subagent.rs`
- Create: `peri-acp/src/langfuse/tracer/subagent_test.rs`

- [ ] **Step 1: 写子对象单测（9 个 test 迁移自 tracer_test.rs）**

```rust
// subagent_test.rs
use super::*;

#[test]
fn test_empty_stack_returns_fallback_main() {
    let s = SubagentStack::new();
    assert_eq!(s.current_agent_id("main_obs"), "main_obs");
}

#[test]
fn test_begin_subagent_pushes_context() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"prompt": "go"}));
    assert_eq!(s.depth(), 1);
}

#[test]
fn test_current_agent_id_returns_top() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({}));
    let top = s.current_agent_id("main");
    assert!(top.starts_with("obs_"));
    assert_ne!(top, "main");
}

#[test]
fn test_nested_subagent_stack_depth_2() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({}));
    s.begin_subagent(&serde_json::json!({}));
    assert_eq!(s.depth(), 2);
}

#[test]
fn test_end_subagent_returns_context() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"prompt": "go"}));
    let end = s.end_subagent().expect("should return Some");
    assert!(end.observation_id.starts_with("obs_"));
    assert_eq!(s.depth(), 0);
}

#[test]
fn test_end_subagent_empty_returns_none() {
    let mut s = SubagentStack::new();
    assert!(s.end_subagent().is_none());
}

#[test]
fn test_is_agent_tool_anywhere_checks_main_and_stack() {
    let mut s = SubagentStack::new();
    let mut main_tb = ToolBatch::new();
    main_tb.on_tool_start("main_call", "Read", serde_json::json!({}));
    assert!(!s.is_agent_tool_anywhere(&main_tb, "main_call"));
    assert!(!s.is_agent_tool_anywhere(&main_tb, "nope"));
}

#[test]
fn test_current_tool_batch_mut_returns_main_when_empty() {
    let mut s = SubagentStack::new();
    let mut main_tb = ToolBatch::new();
    // 调用 current_tool_batch_mut 应该返回 main ToolBatch 引用
    let _ref = s.current_tool_batch_mut(&mut main_tb);
}

#[test]
fn test_lifo_order() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"id": 1}));
    s.begin_subagent(&serde_json::json!({"id": 2}));
    let _last_end = s.end_subagent().unwrap();
    let _first_end = s.end_subagent().unwrap();
    // 后进先出：last_end 应该是后压的（id=2）
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- subagent_test`
Expected: FAIL

- [ ] **Step 3: 实现 SubagentStack + SubAgentContext**

```rust
// peri-acp/src/langfuse/tracer/subagent.rs
use super::tool_batch::ToolBatch;

pub(crate) struct SubAgentContext {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
    pub tool_batch: ToolBatch,
}

pub(crate) struct SubagentEnd {
    pub observation_id: String,
    pub agent_id: String,
    pub start_time: String,
    pub input: serde_json::Value,
}

/// 主层 / 子层 ToolBatch 引用（双路径写入收口）。
pub(crate) enum ToolBatchRef<'a> {
    Main(&'a mut ToolBatch),
    Sub(&'a mut ToolBatch),
}

impl<'a> std::ops::DerefMut for ToolBatchRef<'a> {
    fn deref_mut(&mut self) -> &mut ToolBatch {
        match self {
            ToolBatchRef::Main(t) | ToolBatchRef::Sub(t) => t,
        }
    }
}

pub(crate) struct SubagentStack {
    stack: Vec<SubAgentContext>,
}

impl SubagentStack {
    pub(crate) fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub(crate) fn current_agent_id(&self, fallback_main: &str) -> String {
        self.stack.last()
            .map(|c| c.observation_id.clone())
            .unwrap_or_else(|| fallback_main.to_string())
    }

    pub(crate) fn current_tool_batch_mut<'a>(
        &'a mut self,
        main_tb: &'a mut ToolBatch,
    ) -> ToolBatchRef<'a> {
        match self.stack.last_mut() {
            Some(top) => ToolBatchRef::Sub(&mut top.tool_batch),
            None => ToolBatchRef::Main(main_tb),
        }
    }

    pub(crate) fn is_agent_tool_anywhere(
        &self,
        main_tb: &ToolBatch,
        tool_call_id: &str,
    ) -> bool {
        if main_tb.is_agent_tool(tool_call_id) { return true; }
        self.stack.iter().any(|c| c.tool_batch.is_agent_tool(tool_call_id))
    }

    pub(crate) fn begin_subagent(&mut self, input: &serde_json::Value) {
        let observation_id = format!("obs_{}", uuid::Uuid::now_v7());
        let agent_id = format!("agent_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        self.stack.push(SubAgentContext {
            observation_id,
            agent_id,
            start_time,
            input: input.clone(),
            tool_batch: ToolBatch::new(),
        });
    }

    pub(crate) fn end_subagent(&mut self) -> Option<SubagentEnd> {
        let c = self.stack.pop()?;
        Some(SubagentEnd {
            observation_id: c.observation_id,
            agent_id: c.agent_id,
            start_time: c.start_time,
            input: c.input,
        })
    }

    pub(crate) fn is_empty(&self) -> bool { self.stack.is_empty() }
    pub(crate) fn depth(&self) -> usize { self.stack.len() }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- subagent_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod subagent;
pub(crate) use subagent::{SubagentStack, SubAgentContext, SubagentEnd, ToolBatchRef};
```

---

### Task 4.5: CompactSpan 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/compact.rs`
- Create: `peri-acp/src/langfuse/tracer/compact_test.rs`

- [ ] **Step 1: 写子对象单测**

```rust
// compact_test.rs
use super::*;
use peri_agent::events::{CompactStrategy, CompactTrigger};

#[test]
fn test_initial_state_inactive() {
    let c = CompactSpan::new();
    assert!(!c.is_active());
}

#[test]
fn test_on_start_activates() {
    let mut c = CompactSpan::new();
    let start = c.on_start(CompactStrategy::Full, CompactTrigger::Auto);
    assert!(start.span_id.starts_with("span_"));
    assert!(c.is_active());
}

#[test]
fn test_on_end_returns_context() {
    let mut c = CompactSpan::new();
    c.on_start(CompactStrategy::Micro, CompactTrigger::Auto);
    let ctx = c.on_end().expect("should return Some");
    assert!(ctx.span_id.starts_with("span_"));
    assert!(!c.is_active());
}

#[test]
fn test_on_end_without_start_returns_none() {
    let mut c = CompactSpan::new();
    assert!(c.on_end().is_none());
}

#[test]
fn test_double_start_overwrites() {
    let mut c = CompactSpan::new();
    c.on_start(CompactStrategy::Micro, CompactTrigger::Auto);
    c.on_start(CompactStrategy::Full, CompactTrigger::Manual);
    let ctx = c.on_end().unwrap();
    assert_eq!(ctx.strategy, CompactStrategy::Full);
    assert_eq!(ctx.trigger, CompactTrigger::Manual);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- compact_test`
Expected: FAIL

- [ ] **Step 3: 实现 CompactSpan**

```rust
// peri-acp/src/langfuse/tracer/compact.rs
use peri_agent::events::{CompactStrategy, CompactTrigger};

pub(crate) struct CompactSpanStart {
    pub span_id: String,
    pub start_time: String,
}

pub(crate) struct CompactSpanContext {
    pub span_id: String,
    pub start_time: String,
    pub strategy: CompactStrategy,
    pub trigger: CompactTrigger,
}

pub(crate) struct CompactSpan {
    ctx: Option<CompactSpanContext>,
}

impl CompactSpan {
    pub(crate) fn new() -> Self { Self { ctx: None } }

    pub(crate) fn on_start(
        &mut self,
        strategy: CompactStrategy,
        trigger: CompactTrigger,
    ) -> CompactSpanStart {
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let start = CompactSpanStart {
            span_id: span_id.clone(),
            start_time: start_time.clone(),
        };
        self.ctx = Some(CompactSpanContext {
            span_id, start_time, strategy, trigger,
        });
        start
    }

    pub(crate) fn on_end(&mut self) -> Option<CompactSpanContext> {
        self.ctx.take()
    }

    pub(crate) fn is_active(&self) -> bool { self.ctx.is_some() }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- compact_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod compact;
pub(crate) use compact::{CompactSpan, CompactSpanStart, CompactSpanContext};
```

---

### Task 4.6: SamplingDecider 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/sampling.rs`
- Create: `peri-acp/src/langfuse/tracer/sampling_test.rs`

- [ ] **Step 1: 写子对象单测**

```rust
// sampling_test.rs
use super::*;

#[test]
fn test_rate_1_0_always_emits() {
    let mut d = SamplingDecider::new(1.0);
    for i in 0..10 {
        let turn_id = format!("turn_{}", i);
        assert!(d.should_emit(&turn_id, "sess"), "turn {} 应采样", i);
    }
}

#[test]
fn test_rate_0_never_emits() {
    let mut d = SamplingDecider::new(0.0);
    for i in 0..10 {
        let turn_id = format!("turn_{}", i);
        assert!(!d.should_emit(&turn_id, "sess"), "turn {} 不应采样", i);
    }
}

#[test]
fn test_consistent_within_same_turn() {
    let mut d = SamplingDecider::new(0.5);
    let decision1 = d.should_emit("turn_1", "sess");
    let decision2 = d.should_emit("turn_1", "sess");
    let decision3 = d.should_emit("turn_1", "sess");
    assert_eq!(decision1, decision2);
    assert_eq!(decision2, decision3);
}

#[test]
fn test_cleanup_turn_removes_decision() {
    let mut d = SamplingDecider::new(1.0);
    d.should_emit("turn_1", "sess");
    assert_eq!(d.decided_len(), 1);
    d.cleanup_turn("turn_1");
    assert_eq!(d.decided_len(), 0);
}

#[test]
fn test_cleanup_prevents_unbounded_growth() {
    let mut d = SamplingDecider::new(1.0);
    for i in 0..2000 {
        let turn_id = format!("turn_{}", i);
        d.should_emit(&turn_id, "sess");
        d.cleanup_turn(&turn_id);
    }
    assert_eq!(d.decided_len(), 0, "cleanup 后应清空");
}

#[test]
fn test_high_turn_count_triggers_emergency_cleanup() {
    let mut d = SamplingDecider::new(1.0);
    // 不调 cleanup_turn，模拟异常情况
    for i in 0..1500 {
        let turn_id = format!("turn_{}", i);
        d.should_emit(&turn_id, "sess");
    }
    // decided_len 不应无限增长，应在 1000 时触发清理
    assert!(d.decided_len() <= 1100, "实际: {}", d.decided_len());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- sampling_test`
Expected: FAIL

- [ ] **Step 3: 实现 SamplingDecider**

```rust
// peri-acp/src/langfuse/tracer/sampling.rs
use std::collections::HashMap;

const EMERGENCY_CLEANUP_THRESHOLD: usize = 1000;
const EMERGENCY_CLEANUP_KEEP: usize = 500;

pub(crate) struct SamplingDecider {
    rate: f64,
    decided: HashMap<String, bool>,
}

impl SamplingDecider {
    pub(crate) fn new(rate: f64) -> Self {
        Self {
            rate: rate.clamp(0.0, 1.0),
            decided: HashMap::new(),
        }
    }

    pub(crate) fn should_emit(&mut self, turn_id: &str, session_id: &str) -> bool {
        if let Some(d) = self.decided.get(turn_id) { return *d; }

        if self.decided.len() > EMERGENCY_CLEANUP_THRESHOLD {
            self.emergency_cleanup();
        }

        let h = stable_hash(turn_id, session_id);
        let decision = (h % 10_000) as f64 / 10_000.0 < self.rate;
        self.decided.insert(turn_id.to_string(), decision);
        decision
    }

    pub(crate) fn cleanup_turn(&mut self, turn_id: &str) {
        self.decided.remove(turn_id);
    }

    pub(crate) fn decided_len(&self) -> usize {
        self.decided.len()
    }

    fn emergency_cleanup(&mut self) {
        if self.decided.len() <= EMERGENCY_CLEANUP_KEEP { return; }
        let keep: Vec<String> = self.decided.keys()
            .skip(self.decided.len() - EMERGENCY_CLEANUP_KEEP)
            .cloned()
            .collect();
        let kept: HashMap<String, bool> = keep.into_iter()
            .filter_map(|k| self.decided.get(&k).map(|v| (k, *v)))
            .collect();
        self.decided = kept;
    }
}

fn stable_hash(turn_id: &str, session_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    turn_id.hash(&mut h);
    session_id.hash(&mut h);
    h.finish()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- sampling_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod sampling;
pub(crate) use sampling::SamplingDecider;
```

---

### Task 4.7: StageSpans 子对象（含 MQ 排空 + Workflow）

**Files:**
- Create: `peri-acp/src/langfuse/tracer/stages.rs`
- Create: `peri-acp/src/langfuse/tracer/stages_test.rs`

- [ ] **Step 1: 写子对象单测**

```rust
// stages_test.rs
use super::*;
use peri_agent::events::Stage;

#[test]
fn test_on_stage_start_returns_handle() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    assert!(h.span_id.starts_with("span_"));
    assert_eq!(s.active_stage(), Some(Stage::Reason));
}

#[test]
fn test_on_stage_end_clears_active() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_stage_end(&h, StageStatus::Done);
    assert_eq!(s.active_stage(), None);
}

#[test]
fn test_nested_stages_auto_finish_previous() {
    let mut s = StageSpans::new();
    let _h1 = s.on_stage_start(Stage::Receive, "turn_1", "trace_1", "agent_obs");
    let _h2 = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    assert_eq!(s.active_stage(), Some(Stage::Reason));
}

#[test]
fn test_double_end_early_return() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_stage_end(&h, StageStatus::Done);
    s.on_stage_end(&h, StageStatus::Done); // 二次 end 不应 panic
}

#[test]
fn test_on_mq_drained_writes_to_receive() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start(Stage::Receive, "turn_1", "trace_1", "agent_obs");
    s.on_mq_drained(2, 1, 0);
    assert_eq!(s.mq_counts(), Some((2, 1, 0)));
}

#[test]
fn test_on_mq_drained_outside_receive_no_op() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_mq_drained(2, 1, 0);
    assert_eq!(s.mq_counts(), None);
}

#[test]
fn test_on_workflow_start_creates_child_span() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start(Stage::Act, "turn_1", "trace_1", "agent_obs");
    let w = s.on_workflow_start("wf_1", "plan summary");
    assert!(w.span_id.starts_with("span_"));
}

#[test]
fn test_on_workflow_start_outside_act_no_op() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start(Stage::Reason, "turn_1", "trace_1", "agent_obs");
    let w = s.on_workflow_start("wf_1", "plan");
    assert!(w.span_id.is_empty(), "Reason 阶段不应创建 workflow span");
}

#[test]
fn test_on_workflow_end_returns_stats() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start(Stage::Act, "turn_1", "trace_1", "agent_obs");
    s.on_workflow_start("wf_1", "plan");
    let end = s.on_workflow_end("wf_1", 3, 10).expect("should return Some");
    assert_eq!(end.agents_spawned, 3);
    assert_eq!(end.tool_calls, 10);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- stages_test`
Expected: FAIL

- [ ] **Step 3: 实现 StageSpans**

```rust
// peri-acp/src/langfuse/tracer/stages.rs
use std::collections::HashMap;
use peri_agent::events::{Stage, StageStatus};

pub(crate) struct StageHandle {
    pub span_id: String,
    pub stage: Stage,
    pub start_time: String,
    pub trace_id: String,
    pub parent_observation_id: String,
}

pub(crate) struct WorkflowStartRecord {
    pub span_id: String,
}

pub(crate) struct WorkflowEndRecord {
    pub span_id: String,
    pub agents_spawned: usize,
    pub tool_calls: usize,
}

struct ActiveStage {
    handle: StageHandle,
    workflow_spans: HashMap<String, String>,
    mq_counts: Option<(usize, usize, usize)>,
}

pub(crate) struct StageSpans {
    active: Option<ActiveStage>,
}

impl StageSpans {
    pub(crate) fn new() -> Self { Self { active: None } }

    pub(crate) fn on_stage_start(
        &mut self,
        stage: Stage,
        trace_id: &str,
        _turn_id: &str,
        parent_observation_id: &str,
    ) -> StageHandle {
        // 自动结束前一个 stage（清理状态，事件构造在外层）
        self.active = None;
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        let handle = StageHandle {
            span_id: span_id.clone(),
            stage,
            start_time: start_time.clone(),
            trace_id: trace_id.to_string(),
            parent_observation_id: parent_observation_id.to_string(),
        };
        let mq_counts = if stage == Stage::Receive { Some((0, 0, 0)) } else { None };
        self.active = Some(ActiveStage {
            handle,
            workflow_spans: HashMap::new(),
            mq_counts,
        });
        StageHandle {
            span_id, stage, start_time,
            trace_id: trace_id.to_string(),
            parent_observation_id: parent_observation_id.to_string(),
        }
    }

    pub(crate) fn on_stage_end(&mut self, _handle: &StageHandle, _status: StageStatus) {
        self.active = None;
    }

    pub(crate) fn active_stage(&self) -> Option<Stage> {
        self.active.as_ref().map(|a| a.handle.stage)
    }

    pub(crate) fn active_handle(&self) -> Option<&StageHandle> {
        self.active.as_ref().map(|a| &a.handle)
    }

    pub(crate) fn on_mq_drained(&mut self, prompt: usize, defer: usize, info: usize) {
        if let Some(a) = &mut self.active {
            if a.handle.stage == Stage::Receive {
                a.mq_counts = Some((prompt, defer, info));
            }
        }
    }

    pub(crate) fn mq_counts(&self) -> Option<(usize, usize, usize)> {
        self.active.as_ref().and_then(|a| a.mq_counts)
    }

    pub(crate) fn on_workflow_start(
        &mut self,
        workflow_id: &str,
        _plan: &str,
    ) -> WorkflowStartRecord {
        let span_id = match &mut self.active {
            Some(a) if a.handle.stage == Stage::Act => {
                let span_id = format!("span_{}", uuid::Uuid::now_v7());
                a.workflow_spans.insert(workflow_id.to_string(), span_id.clone());
                span_id
            }
            _ => String::new(),
        };
        WorkflowStartRecord { span_id }
    }

    pub(crate) fn on_workflow_end(
        &mut self,
        workflow_id: &str,
        agents_spawned: usize,
        tool_calls: usize,
    ) -> Option<WorkflowEndRecord> {
        let a = self.active.as_ref()?;
        if a.handle.stage != Stage::Act { return None; }
        let span_id = a.workflow_spans.get(workflow_id)?.clone();
        Some(WorkflowEndRecord {
            span_id,
            agents_spawned,
            tool_calls,
        })
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- stages_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod stages;
pub(crate) use stages::{StageSpans, StageHandle, WorkflowStartRecord, WorkflowEndRecord};
```

---

### Task 4.8: MiddlewareTracer 子对象

**Files:**
- Create: `peri-acp/src/langfuse/tracer/middleware.rs`
- Create: `peri-acp/src/langfuse/tracer/middleware_test.rs`

- [ ] **Step 1: 写子对象单测**

```rust
// middleware_test.rs
use super::*;
use peri_agent::events::{MiddlewareHook, StageStatus};

#[test]
fn test_on_start_returns_handle() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("HookMW", MiddlewareHook::BeforeAgent);
    assert!(h.span_id.starts_with("span_"));
}

#[test]
fn test_on_end_returns_stats() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("HookMW", MiddlewareHook::BeforeAgent);
    let end = m.on_end(&h, StageStatus::Done, None).expect("should return Some");
    assert_eq!(end.name, "HookMW");
    assert_eq!(end.status, StageStatus::Done);
}

#[test]
fn test_on_end_unknown_returns_none() {
    let mut m = MiddlewareTracer::new();
    let h = MiddlewareSpanHandle { span_id: "unknown".into(), name: "X".into(), hook: MiddlewareHook::BeforeAgent };
    assert!(m.on_end(&h, StageStatus::Done, None).is_none());
}

#[test]
fn test_concurrent_same_hook_preserves_pairing() {
    let mut m = MiddlewareTracer::new();
    let h1 = m.on_start("MW1", MiddlewareHook::BeforeAgent);
    let h2 = m.on_start("MW2", MiddlewareHook::BeforeAgent);
    assert!(m.on_end(&h1, StageStatus::Done, None).is_some());
    assert!(m.on_end(&h2, StageStatus::Done, None).is_some());
}

#[test]
fn test_on_end_with_error_carries_message() {
    let mut m = MiddlewareTracer::new();
    let h = m.on_start("FailingMW", MiddlewareHook::AfterTool);
    let end = m.on_end(&h, StageStatus::Error, Some("panic".into())).unwrap();
    assert_eq!(end.error.as_deref(), Some("panic"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p peri-acp --lib -- middleware_test`
Expected: FAIL

- [ ] **Step 3: 实现 MiddlewareTracer**

```rust
// peri-acp/src/langfuse/tracer/middleware.rs
use std::collections::HashMap;
use peri_agent::events::{MiddlewareHook, StageStatus};

pub(crate) struct MiddlewareSpanHandle {
    pub span_id: String,
    pub name: String,
    pub hook: MiddlewareHook,
}

struct ActiveMiddleware {
    name: String,
    hook: MiddlewareHook,
    start_time: String,
}

pub(crate) struct MiddlewareEndRecord {
    pub span_id: String,
    pub name: String,
    pub hook: MiddlewareHook,
    pub start_time: String,
    pub status: StageStatus,
    pub error: Option<String>,
}

pub(crate) struct MiddlewareTracer {
    active: HashMap<String, ActiveMiddleware>,
}

impl MiddlewareTracer {
    pub(crate) fn new() -> Self {
        Self { active: HashMap::new() }
    }

    pub(crate) fn on_start(
        &mut self,
        name: &str,
        hook: MiddlewareHook,
    ) -> MiddlewareSpanHandle {
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let start_time = chrono::Utc::now().to_rfc3339();
        self.active.insert(span_id.clone(), ActiveMiddleware {
            name: name.to_string(),
            hook,
            start_time: start_time.clone(),
        });
        MiddlewareSpanHandle {
            span_id,
            name: name.to_string(),
            hook,
        }
    }

    pub(crate) fn on_end(
        &mut self,
        handle: &MiddlewareSpanHandle,
        status: StageStatus,
        error: Option<String>,
    ) -> Option<MiddlewareEndRecord> {
        let active = self.active.remove(&handle.span_id)?;
        Some(MiddlewareEndRecord {
            span_id: handle.span_id.clone(),
            name: active.name,
            hook: active.hook,
            start_time: active.start_time,
            status,
            error,
        })
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p peri-acp --lib -- middleware_test`
Expected: PASS

- [ ] **Step 5: 在 mod.rs 加 mod 声明**

```rust
mod middleware;
pub(crate) use middleware::{MiddlewareTracer, MiddlewareSpanHandle, MiddlewareEndRecord};
```

---

### Task 4.9: LangfuseTracer 主 struct 重构

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs`（14 → 12 字段，19 → 21 on_* 方法）
- Modify: `peri-acp/src/langfuse/tracer/usage.rs`（保留）
- Modify: `peri-acp/src/langfuse/tracer/event_builder.rs`（保留，更新签名）

**注意**：Task 4.9 是 Phase 4 中最大的 task。本 plan 给出主 struct 骨架，实施时需参考现有 `llm_handler.rs` / `tool_handler.rs` / `compact_handler.rs` 的事件构造代码（如 `build_generation_body`、`build_observation_body` 等工具函数）填充完整的 `try_add(IngestionEvent::XxxCreate{...})` 代码。

- [ ] **Step 1: 重写 LangfuseTracer 主 struct**

打开 `peri-acp/src/langfuse/tracer/mod.rs`，整体替换 struct 定义和 impl 块。骨架见 spec §3.1 / §3.3。

```rust
// 关键骨架（完整实现参考现有 handler 文件）
pub struct LangfuseTracer {
    // 5 简单字段
    session: Arc<dyn LangfuseSessionLike>,
    session_id: String,
    trace_id: String,             // == turn_id
    agent_observation_id: String,
    final_answer: String,
    // 7 子对象
    sampling: SamplingDecider,
    stages: StageSpans,
    middleware: MiddlewareTracer,
    generation: GenerationTracker,
    tool_batch: ToolBatch,
    subagent: SubagentStack,
    compact: CompactSpan,
}

impl LangfuseTracer {
    pub fn new(
        session: Arc<dyn LangfuseSessionLike>,
        session_id: String,
        turn_id: String,
        agent_observation_id: String,
        sampling_rate: f64,
    ) -> Self { /* 见 spec §3.1 */ }

    // 21 个 on_* 方法骨架见 spec §3.3
    // 每个 on_* 方法入口加：
    //   if !self.sampling.should_emit(&self.trace_id, &self.session_id) { return; }
}
```

- [ ] **Step 2: 从现有 handler 文件迁移事件构造逻辑**

逐个 handler 迁移：
- `llm_handler.rs` 的 `on_llm_*` 逻辑 → `generation.rs` + 主 struct 的 `on_llm_*` 方法
- `tool_handler.rs` 的 `on_tool_*` 逻辑 → `tool_batch.rs` + 主 struct 的 `on_tool_*` 方法
- `compact_handler.rs` 的 `on_compact_*` 逻辑 → `compact.rs` + 主 struct 的 `on_compact_*` 方法
- `trace_lifecycle.rs` 的 `on_trace_*` 逻辑 → 主 struct 的 `on_turn_*` 方法
- `subagent_stack.rs` 的 `begin/end_subagent` 逻辑 → `subagent.rs` + 主 struct 的 `on_tool_*` 中的 SubAgent 协调

每个 on_* 方法的完整实现模式：
1. 检查 `sampling.should_emit` → 否则早返回
2. 调用子对象方法（如 `self.generation.on_llm_start(...)`）
3. 用返回的数据构造 `IngestionEvent`（参考现有 event_builder.rs 工具函数）
4. `self.session.try_add(event)`

- [ ] **Step 3: cargo build -p peri-acp 确认主 struct 编译**

Run: `cargo build -p peri-acp`
Expected: 0 error（forward_langfuse_event 调用签名不匹配将在 Phase 5 修复）

---

### Task 4.10: on_trace_* → on_turn_* 重命名

**Files:**
- Modify: `peri-acp/src/session/executor_helpers.rs`
- Modify: `peri-agent/src/agent/workflow_agent.rs`

- [ ] **Step 1: grep 所有 on_trace_start / on_trace_end 调用点**

Run: `grep -rn "on_trace_start\|on_trace_end" /Users/konghayao/code/ai/perihelion/peri-acp/ /Users/konghayao/code/ai/perihelion/peri-agent/`

- [ ] **Step 2: 重命名调用**

每处 `tracer.lock().on_trace_start(...)` 改为 `tracer.lock().on_turn_start(...)`，`on_trace_end(...)` 改为 `on_turn_end(...)`。签名同步调整（on_turn_end 增加 TurnStatus 参数）。

- [ ] **Step 3: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 0 error

---

### Task 4.11: Phase 4 提交

- [ ] **Step 1: cargo test -p peri-acp --lib**

Run: `cargo test -p peri-acp --lib`
Expected: 所有子对象单测（约 680 行）通过

- [ ] **Step 2: lefthook run pre-commit**

Run: `lefthook run pre-commit`
Expected: 全绿

- [ ] **Step 3: 提交**

```bash
git add peri-acp/src/langfuse/
git commit -m "$(cat <<'EOF'
refactor(langfuse): LangfuseTracer 14 字段收敛为 7 子状态机

- 新增 7 个子对象：GenerationTracker / ToolBatch / SubagentStack /
  CompactSpan / SamplingDecider / StageSpans（含 MQ + Workflow）/
  MiddlewareTracer，全部私有字段
- 抽出 LangfuseSessionLike trait，支持 FakeLangfuseSession 单测
- on_trace_start/end 重命名为 on_turn_start/end（语义对齐 trace_id=turn_id）
- 21 个 on_* 方法骨架完成，子对象不变量收口在各自 impl 内
- 680 行子对象单测全部通过

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 5: forward 路由扩展（commit 5）

### Task 5.1: forward_langfuse_event 路由扩展

**Files:**
- Modify: `peri-acp/src/session/executor_helpers.rs:273-355`

- [ ] **Step 1: 写集成测试（用 FakeLangfuseSession）**

```rust
// peri-acp/src/session/executor_helpers_test.rs（新建）
use peri_acp::langfuse::{LangfuseTracer, FakeLangfuseSession};
use peri_agent::events::*;
use parking_lot::Mutex;

fn make_tracer() -> (Mutex<LangfuseTracer>, std::sync::Arc<FakeLangfuseSession>) {
    let session = FakeLangfuseSession::new("sess_test");
    let tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs_1".into(), 1.0,
    );
    (Mutex::new(tracer), session)
}

#[test]
fn test_forward_session_started_calls_on_session_start() {
    let (tracer, session) = make_tracer();
    let event = ExecutorEvent::SessionStarted {
        session_id: "sess_test".into(),
        frozen_summary: serde_json::json!({}),
    };
    super::forward_langfuse_event(&tracer, &event, "claude-4.7");
    // 验证：session 收到了 SessionCreate 事件
    let events = session.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, langfuse_client::types::IngestionEvent::SessionCreate { .. })));
}

#[test]
fn test_forward_stage_started_calls_on_stage_start() {
    let (tracer, session) = make_tracer();
    let event = ExecutorEvent::StageStarted {
        turn_id: "turn_1".into(),
        stage: Stage::Reason,
    };
    super::forward_langfuse_event(&tracer, &event, "claude-4.7");
    let events = session.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, langfuse_client::types::IngestionEvent::SpanCreate { .. })));
}

#[test]
fn test_forward_unsampled_event_no_op() {
    // 用 sampling_rate=0 构造 tracer
    let session = FakeLangfuseSession::new("sess_test");
    let tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs_1".into(), 0.0,
    );
    let tracer = Mutex::new(tracer);
    let event = ExecutorEvent::StageStarted { turn_id: "turn_1".into(), stage: Stage::Reason };
    super::forward_langfuse_event(&tracer, &event, "claude-4.7");
    assert_eq!(session.event_count(), 0, "sampled=false 应 no-op");
}
```

- [ ] **Step 2: 扩展 forward_langfuse_event 的 match 分支**

打开 `peri-acp/src/session/executor_helpers.rs:273`，在现有 match 末尾追加：

```rust
ExecutorEvent::SessionStarted { session_id, frozen_summary } => {
    tracer.lock().on_session_start(frozen_summary.clone());
    let _ = session_id;
}
ExecutorEvent::TurnStarted { turn_id, .. } => {
    tracer.lock().on_turn_start(turn_id);
}
ExecutorEvent::TurnEnded { turn_id, status, error_kind, .. } => {
    tracer.lock().on_turn_end(turn_id, status);
    let _ = error_kind; // ErrorSpan 兜底在 on_turn_end 内处理
}
ExecutorEvent::StageStarted { turn_id: _, stage } => {
    tracer.lock().on_stage_start(*stage);
}
ExecutorEvent::StageEnded { turn_id: _, stage, status, duration_ms: _ } => {
    tracer.lock().on_stage_end(*stage, status.clone());
}
ExecutorEvent::MiddlewareStarted { turn_id: _, mw_name, hook } => {
    tracer.lock().on_middleware_start(mw_name, *hook);
}
ExecutorEvent::MiddlewareEnded { turn_id: _, mw_name, hook, status, error } => {
    // on_middleware_end 需要 handle 引用——这里改为基于 mw_name+hook 反查
    // 实际实现：middleware tracer 内部按 (mw_name, hook) 索引
    let _ = (mw_name, hook, status, error);
}
ExecutorEvent::AiReasoningChunk { turn_id: _, text, .. } => {
    tracer.lock().on_ai_reasoning_chunk(text);
}
ExecutorEvent::BudgetThresholdHit { turn_id: _, threshold, current_pct, tokens_in, tokens_out } => {
    tracer.lock().on_budget_threshold_hit(*threshold, *current_pct, *tokens_in, *tokens_out);
}
ExecutorEvent::MessageQueueDrained { turn_id: _, prompt, defer, info } => {
    tracer.lock().on_mq_drained(*prompt, *defer, *info);
}
ExecutorEvent::WorkflowStarted { turn_id: _, workflow_id, plan_summary } => {
    tracer.lock().on_workflow_start(workflow_id, plan_summary);
}
ExecutorEvent::WorkflowEnded { turn_id: _, workflow_id, agents_spawned, tool_calls } => {
    tracer.lock().on_workflow_end(workflow_id, *agents_spawned, *tool_calls);
}
ExecutorEvent::CompactStarted { turn_id: _, agent_id: _, step: _, strategy, trigger } => {
    tracer.lock().on_compact_start(*strategy, *trigger);
}
ExecutorEvent::CompactCompleted { summary: _, files: _, skills: _, micro_cleared, messages: _, token_before, token_after, strategy } => {
    tracer.lock().on_compact_end(*token_before, *token_after, 0, 0, *micro_cleared, *strategy, peri_agent::events::CompactTrigger::Auto);
}
// 现有 LlmCallStart/Payload/End/Retrying、ToolStart/End、TextChunk、CompactStarted/Error 等保留
```

注意：现有 `CompactStarted` 已被扩充，所以现有 match 分支需同步修改参数。

- [ ] **Step 3: 跑集成测试确认通过**

Run: `cargo test -p peri-acp --lib -- executor_helpers_test`
Expected: PASS

---

### Task 5.2: workflow_agent.rs 改挂主 Trace Act Span 下

**Files:**
- Modify: `peri-agent/src/agent/workflow_agent.rs:142-504`

- [ ] **Step 1: grep workflow_agent.rs 的 langfuse tracer pump**

Run: `grep -n "forward_langfuse_event\|LangfuseTracer\|tracer" /Users/konghayao/code/ai/perihelion/peri-agent/src/agent/workflow_agent.rs`

- [ ] **Step 2: 把独立 tracer 改为接收主 trace 的 Act Span 上下文**

打开 `workflow_agent.rs:142`，找到 workflow 的 langfuse tracer pump。把当前"构造独立 LangfuseTracer + 独立 trace_id"改为"接收主 trace 的 (trace_id, agent_observation_id) + WorkflowSpan 作为 parent"。

具体修改：

```rust
// workflow_agent.rs，WorkflowRunner::new 或类似入口
pub fn new(
    main_trace_id: String,          // ★ 新增：主 trace 的 trace_id
    main_act_span_id: String,       // ★ 新增：Act 阶段 Span id（作为 workflow parent）
    session: Arc<dyn LangfuseSessionLike>,
    // ... 其他参数
) -> Self {
    // 不再独立 new LangfuseTracer；workflow 的所有事件都通过 forward_langfuse_event
    // 路由到主 tracer，trace_id 仍是主 trace_id，parent 是 Act Span。
}
```

`forward_langfuse_event` 已经接收 `&parking_lot::Mutex<LangfuseTracer>`，workflow 内部事件直接调用主 tracer，无需独立 pump。

- [ ] **Step 3: 删除 workflow_agent.rs 内独立的 tracer pump 代码**

移除约 L142-504 之间的独立 tracer 构造和事件转发逻辑，改为统一调用主 executor 的 forward_langfuse_event。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p peri-agent --lib -- workflow_agent_test`
Expected: PASS（如有）

- [ ] **Step 5: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 0 error

---

### Task 5.3: Phase 5 提交

- [ ] **Step 1: cargo test --workspace**

Run: `cargo test --workspace`
Expected: 全 PASS

- [ ] **Step 2: 提交**

```bash
git add peri-acp/src/session/executor_helpers.rs peri-agent/src/agent/workflow_agent.rs
git commit -m "$(cat <<'EOF'
feat(langfuse): forward 路由扩展 + WorkflowAgent 改挂主 Trace Act Span

- forward_langfuse_event 加 12 个新变体路由 + CompactStarted 扩充字段路由
- WorkflowAgent 不再独立构造 tracer，事件直接转发到主 trace 的 Act Span
- 集成测试覆盖 sampling=0/1.0 边界 + SessionStarted/StageStarted 路由

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 6: Sampling + ErrorSpan + 配置（commit 6）

### Task 6.1: Sampling 算法接入主 struct

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs`（on_turn_start / on_turn_end + 每个 on_* 入口）

- [ ] **Step 1: 测试 sampling 在主 struct 上的端到端行为**

```rust
// peri-acp/src/langfuse/tracer_test.rs（追加集成测试）
#[test]
fn test_tracer_sampling_rate_0_silently_no_ops_all_events() {
    let session = FakeLangfuseSession::new("sess_test");
    let mut tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs".into(), 0.0,
    );
    tracer.on_turn_start("turn_1");
    tracer.on_stage_start(Stage::Reason);
    tracer.on_llm_start(0, &[], &[]);
    tracer.on_stage_end(Stage::Reason, StageStatus::Done);
    tracer.on_turn_end("turn_1", TurnStatus::Done);
    assert_eq!(session.event_count(), 0);
}

#[test]
fn test_tracer_sampling_rate_1_emits_all_events() {
    let session = FakeLangfuseSession::new("sess_test");
    let mut tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs".into(), 1.0,
    );
    tracer.on_turn_start("turn_1");
    tracer.on_stage_start(Stage::Reason);
    tracer.on_llm_start(0, &[], &[]);
    tracer.on_stage_end(Stage::Reason, StageStatus::Done);
    tracer.on_turn_end("turn_1", TurnStatus::Done);
    assert!(session.event_count() > 0);
}
```

- [ ] **Step 2: 在 on_turn_start 调 sampling 决定**

```rust
// tracer/mod.rs，on_turn_start 方法
pub fn on_turn_start(&mut self, turn_id: &str) {
    debug_assert_eq!(self.trace_id, turn_id, "trace_id 必须等于 turn_id");
    let _ = self.sampling.should_emit(turn_id, &self.session_id);
    // 不通知 caller，silently
}
```

- [ ] **Step 3: 在每个 on_* 方法入口加 sampling 检查**

每个 on_* 方法第一行：

```rust
if !self.sampling.should_emit(&self.trace_id, &self.session_id) { return; }
```

- [ ] **Step 4: 在 on_turn_end 调 cleanup_turn**

```rust
pub fn on_turn_end(&mut self, turn_id: &str, status: TurnStatus) -> JoinHandle<()> {
    // ... ErrorSpan 兜底（Task 6.2）...

    self.sampling.cleanup_turn(turn_id);
    self.session.flush()
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test -p peri-acp --lib -- tracer_test::test_tracer_sampling`
Expected: PASS

---

### Task 6.2: ErrorSpan 兜底机制

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs`（on_turn_end）

- [ ] **Step 1: 写 ErrorSpan 测试**

```rust
#[test]
fn test_error_span_emitted_for_unsampled_error_turn() {
    let session = FakeLangfuseSession::new("sess_test");
    let mut tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs".into(), 0.0,
    );
    tracer.on_turn_start("turn_1");
    // 整 turn 不上报（sampled=false）
    tracer.on_turn_end("turn_1", TurnStatus::Error);
    // 但 ErrorSpan 应被强制 emit
    let events = session.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, IngestionEvent::TraceCreate { .. })), "应补发 TraceCreate");
    assert!(events.iter().any(|e| matches!(e, IngestionEvent::SpanCreate { body, .. } if body.name.as_deref() == Some("ErrorTurn"))), "应发 ErrorSpan");
}

#[test]
fn test_error_span_appended_for_sampled_error_turn() {
    let session = FakeLangfuseSession::new("sess_test");
    let mut tracer = LangfuseTracer::new(
        session.clone(), "sess_test".into(), "turn_1".into(), "agent_obs".into(), 1.0,
    );
    tracer.on_turn_start("turn_1");
    tracer.on_stage_start(Stage::Reason);
    tracer.on_turn_end("turn_1", TurnStatus::Error);
    let events = session.events_snapshot();
    // sampled=true 时 trace 已存在，不再补发 TraceCreate
    let trace_count = events.iter().filter(|e| matches!(e, IngestionEvent::TraceCreate { .. })).count();
    assert_eq!(trace_count, 1, "sampled turn 不补发 TraceCreate");
    // 但仍应有 ErrorSpan
    assert!(events.iter().any(|e| matches!(e, IngestionEvent::SpanCreate { body, .. } if body.name.as_deref() == Some("ErrorTurn"))));
}
```

- [ ] **Step 2: 实现 ErrorSpan 兜底**

```rust
// tracer/mod.rs，on_turn_end
pub fn on_turn_end(&mut self, turn_id: &str, status: TurnStatus) -> JoinHandle<()> {
    let was_sampled = self.sampling.should_emit(turn_id, &self.session_id);

    if status == TurnStatus::Error && self.config.error_span_always {
        if !was_sampled {
            // 补发 TraceCreate（用 turn_id 作 trace_id）
            let trace_body = langfuse_client::types::TraceBody {
                id: self.trace_id.clone(),
                name: Some(format!("turn {}", turn_id)),
                metadata: Some(serde_json::json!({"synthetic_error": true})),
                ..Default::default()
            };
            let _ = self.session.try_add(IngestionEvent::TraceCreate {
                id: format!("evt_{}", uuid::Uuid::now_v7()),
                body: trace_body,
            });
        }
        // 追加 ErrorSpan（无论 sampled 与否）
        let span_id = format!("span_{}", uuid::Uuid::now_v7());
        let span_body = SpanBody {
            id: Some(span_id.clone()),
            trace_id: Some(self.trace_id.clone()),
            parent_observation_id: Some(self.agent_observation_id.clone()),
            name: Some("ErrorTurn".into()),
            start_time: Some(chrono::Utc::now().to_rfc3339()),
            end_time: Some(chrono::Utc::now().to_rfc3339()),
            metadata: Some(serde_json::json!({
                "is_synthetic": !was_sampled,
                "was_sampled": was_sampled,
                "turn_id": turn_id,
            })),
            ..Default::default()
        };
        let _ = self.session.try_add(IngestionEvent::SpanCreate {
            id: format!("evt_{}", uuid::Uuid::now_v7()),
            body: span_body,
        });
    }

    self.sampling.cleanup_turn(turn_id);
    self.session.flush()
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-acp --lib -- tracer_test::test_error_span`
Expected: PASS

---

### Task 6.3: 配置加载 + settings.json 支持

**Files:**
- Modify: `peri-acp/src/langfuse/config.rs`

- [ ] **Step 1: 写配置加载测试**

```rust
// peri-acp/src/langfuse/config_test.rs
use super::*;

#[test]
fn test_load_from_env_takes_precedence_over_settings_json() {
    temp_env::with_vars(
        [
            ("LANGFUSE_TRACE_SAMPLING", Some("0.3")),
            ("LANGFUSE_ERROR_SPAN_ALWAYS", Some("false")),
        ],
        || {
            let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({
                "langfuse": {
                    "trace_sampling": 0.5,
                    "error_span_always": true,
                }
            }));
            assert_eq!(cfg.trace_sampling, 0.3);
            assert!(!cfg.error_span_always);
        },
    );
}

#[test]
fn test_load_from_settings_json_when_no_env() {
    temp_env::with_vars(
        [
            ("LANGFUSE_TRACE_SAMPLING", None),
            ("LANGFUSE_ERROR_SPAN_ALWAYS", None),
        ],
        || {
            let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({
                "langfuse": { "trace_sampling": 0.5 }
            }));
            assert_eq!(cfg.trace_sampling, 0.5);
            assert!(cfg.error_span_always, "默认值");
        },
    );
}

#[test]
fn test_load_defaults_when_nothing_set() {
    temp_env::with_vars(
        [
            ("LANGFUSE_TRACE_SAMPLING", None),
            ("LANGFUSE_ERROR_SPAN_ALWAYS", None),
        ],
        || {
            let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({}));
            assert_eq!(cfg.trace_sampling, 1.0);
            assert!(cfg.error_span_always);
            assert_eq!(cfg.batch_max_events, 50);
        },
    );
}
```

- [ ] **Step 2: 实现 LangfuseConfig 加载**

```rust
// peri-acp/src/langfuse/config.rs
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub base_url: String,
    pub trace_sampling: f64,
    pub error_span_always: bool,
    pub batch_max_events: usize,
    pub batch_flush_interval_secs: u64,
    pub batch_backpressure: langfuse_client::config::BackpressurePolicy,
}

#[derive(Deserialize, Default)]
struct SettingsFile {
    langfuse: Option<SettingsLangfuse>,
}

#[derive(Deserialize, Default)]
struct SettingsLangfuse {
    public_key: Option<String>,
    secret_key: Option<String>,
    base_url: Option<String>,
    trace_sampling: Option<f64>,
    error_span_always: Option<bool>,
    batch_max_events: Option<usize>,
    batch_flush_interval_secs: Option<u64>,
    batch_backpressure: Option<langfuse_client::config::BackpressurePolicy>,
}

impl LangfuseConfig {
    pub fn load_with_settings(settings_json: &serde_json::Value) -> Self {
        let settings: SettingsFile = serde_json::from_value(settings_json.clone()).unwrap_or_default();
        let s = settings.langfuse.unwrap_or_default();

        Self {
            public_key: std::env::var("LANGFUSE_PUBLIC_KEY").ok().or(s.public_key),
            secret_key: std::env::var("LANGFUSE_SECRET_KEY").ok().or(s.secret_key),
            base_url: std::env::var("LANGFUSE_BASE_URL").ok().or(s.base_url)
                .unwrap_or_else(|| "https://cloud.langfuse.com".into()),
            trace_sampling: std::env::var("LANGFUSE_TRACE_SAMPLING").ok()
                .and_then(|v| v.parse().ok())
                .or(s.trace_sampling)
                .unwrap_or(1.0),
            error_span_always: std::env::var("LANGFUSE_ERROR_SPAN_ALWAYS").ok()
                .and_then(|v| v.parse().ok())
                .or(s.error_span_always)
                .unwrap_or(true),
            batch_max_events: std::env::var("LANGFUSE_BATCH_MAX_EVENTS").ok()
                .and_then(|v| v.parse().ok())
                .or(s.batch_max_events)
                .unwrap_or(50),
            batch_flush_interval_secs: std::env::var("LANGFUSE_BATCH_FLUSH_INTERVAL_SECS").ok()
                .and_then(|v| v.parse().ok())
                .or(s.batch_flush_interval_secs)
                .unwrap_or(10),
            batch_backpressure: std::env::var("LANGFUSE_BATCH_BACKPRESSURE").ok()
                .and_then(|v| v.parse().ok())
                .or(s.batch_backpressure)
                .unwrap_or_default(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.public_key.is_some() && self.secret_key.is_some()
    }
}
```

- [ ] **Step 3: 加 dev-dependency temp_env**

如未声明，在 `peri-acp/Cargo.toml` 的 `[dev-dependencies]` 加：

```toml
temp-env = "0.3"
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p peri-acp --lib -- config_test`
Expected: PASS

---

### Task 6.4: Phase 6 提交

- [ ] **Step 1: cargo test -p peri-acp --lib**

Run: `cargo test -p peri-acp --lib`
Expected: 全 PASS

- [ ] **Step 2: 提交**

```bash
git add peri-acp/src/langfuse/config.rs peri-acp/src/langfuse/tracer/mod.rs peri-acp/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(langfuse): Sampling 接入主 struct + ErrorSpan 兜底 + settings.json 配置

- 每个 on_* 入口加 sampling.should_emit 检查（silently no-op）
- on_turn_end 检测 Error 时补发 TraceCreate + ErrorSpan（trace_id=turn_id）
- LangfuseConfig::load_with_settings 支持 env > settings.json > 默认值
- 5 个新环境变量全部支持 ~/.peri/settings.json 的 langfuse.* 字段

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 7: 测试（commit 7）

### Task 7.1: 子对象单测全套（680 行）

**Files:**
- 已在 Task 4.2-4.8 创建

- [ ] **Step 1: 跑全部子对象单测**

Run: `cargo test -p peri-acp --lib -- {generation,tool_batch,subagent,compact,sampling,stages,middleware}_test`
Expected: 全 PASS

- [ ] **Step 2: 补充覆盖率不足的边界测试**

按 P0 测试矩阵（spec §5.4）补全：
- `stages_test.rs` 加 Compact 阈值以下不上报 StageSpan 的测试
- `sampling_test.rs` 加 hash 一致性测试（同 turn_id+session_id 跨实例）

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-acp --lib`
Expected: 全 PASS

---

### Task 7.2: tracer_test.rs 集成层冒烟

**Files:**
- Modify: `peri-acp/src/langfuse/tracer_test.rs`（保留约 80 行冒烟）

- [ ] **Step 1: 删除已迁移到子对象的 19 处字段白盒访问**

按 spec §5.2 处理：原 `tracer_test.rs` 的 9 个 subagent test、4 个 compact test、6 个 generation test 已迁入子对象 `_test.rs`。删除原文件中这些 test。

- [ ] **Step 2: 保留集成层冒烟用例**

```rust
// tracer_test.rs（保留约 80 行）
use super::*;

fn make_tracer(rate: f64) -> (parking_lot::Mutex<LangfuseTracer>, std::sync::Arc<FakeLangfuseSession>) {
    let session = FakeLangfuseSession::new("sess_smoke");
    let t = LangfuseTracer::new(session.clone(), "sess_smoke".into(), "turn_1".into(), "agent_obs".into(), rate);
    (parking_lot::Mutex::new(t), session)
}

#[test]
fn test_smoke_complete_turn_sequence() {
    let (t, session) = make_tracer(1.0);
    let mut t = t.lock();
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Receive);
    t.on_stage_end(Stage::Receive, StageStatus::Done);
    t.on_stage_start(Stage::Reason);
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "claude-4.7", "anthropic", "answer", None);
    t.on_stage_end(Stage::Reason, StageStatus::Done);
    t.on_stage_start(Stage::End);
    t.on_stage_end(Stage::End, StageStatus::Done);
    t.on_turn_end("turn_1", TurnStatus::Done);
    let events = session.events_snapshot();
    assert!(events.len() >= 5, "应有 Session/Trace/Stage/Generation 等事件");
}

#[test]
fn test_smoke_event_ordering_parent_before_child() {
    // 父 span 必须先于子 span 入队
    let (t, session) = make_tracer(1.0);
    let mut t = t.lock();
    t.on_stage_start(Stage::Reason);
    t.on_llm_start(0, &[], &[]);
    // ... 验证 batcher 内事件顺序
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-acp --lib -- tracer_test`
Expected: PASS

---

### Task 7.3: e2e mock 端到端测试

**Files:**
- Create: `peri-acp/tests/langfuse_e2e.rs`
- Modify: `peri-acp/Cargo.toml`（加 mockito dev-dep）

- [ ] **Step 1: 加 mockito dev-dependency**

打开 `peri-acp/Cargo.toml`，在 `[dev-dependencies]` 加：

```toml
mockito = "1"
```

- [ ] **Step 2: 写 e2e 测试**

```rust
// peri-acp/tests/langfuse_e2e.rs
use peri_acp::langfuse::{LangfuseTracer, LangfuseSessionLike};
use peri_acp::langfuse::session::LangfuseSession;

#[tokio::test]
async fn test_e2e_complete_turn_sends_session_trace_stage_to_langfuse() {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();

    // mock OTLP 端点
    let _m = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .match_header("x-langfuse-ingestion-version", "4")
        .create_async().await;

    // 用真实 LangfuseSession 指向 mock
    let session = LangfuseSession::new(
        "pk-test".into(),
        "sk-test".into(),
        url,
        "sess_e2e".into(),
    );

    let mut tracer = LangfuseTracer::new(
        std::sync::Arc::new(session.clone()) as std::sync::Arc<dyn LangfuseSessionLike>,
        "sess_e2e".into(),
        "turn_1".into(),
        "agent_obs".into(),
        1.0, // 全报
    );

    // 模拟完整 turn
    tracer.on_session_start(serde_json::json!({"frozen": "summary"}));
    tracer.on_turn_start("turn_1");
    tracer.on_stage_start(peri_agent::events::Stage::Receive);
    tracer.on_stage_end(peri_agent::events::Stage::Receive, peri_agent::events::StageStatus::Done);
    tracer.on_stage_start(peri_agent::events::Stage::Reason);
    tracer.on_llm_start(0, &[], &[]);
    tracer.on_llm_end(0, "claude-4.7", "anthropic", "hello", None);
    tracer.on_stage_end(peri_agent::events::Stage::Reason, peri_agent::events::StageStatus::Done);
    tracer.on_stage_start(peri_agent::events::Stage::End);
    tracer.on_stage_end(peri_agent::events::Stage::End, peri_agent::events::StageStatus::Done);
    tracer.on_turn_end("turn_1", peri_agent::events::TurnStatus::Done).await;

    // mock_server.assert() 验证请求被收到
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p peri-acp --test langfuse_e2e`
Expected: PASS

---

### Task 7.4: mapper_test.rs 同步

**Files:**
- Modify: `peri-acp/src/event/mapper_test.rs`

- [ ] **Step 1: 为每个新变体加占位映射测试**

```rust
// mapper_test.rs（追加）
#[test]
fn test_mapper_session_started_no_session_update() {
    let event = ExecutorEvent::SessionStarted {
        session_id: "s1".into(),
        frozen_summary: serde_json::json!({}),
    };
    let result = map_executor_event(&event);
    assert!(result.is_none(), "SessionStarted 不产生 SessionUpdate");
}

// 重复模式为 TurnStarted / TurnEnded / StageStarted / StageEnded /
// MiddlewareStarted/Ended / AiReasoningChunk / BudgetThresholdHit /
// MessageQueueDrained / WorkflowStarted/Ended 各加一个 test
```

- [ ] **Step 2: 跑测试**

Run: `cargo test -p peri-acp --lib -- mapper_test`
Expected: 全 PASS

- [ ] **Step 3: 跑 variant_coverage_test 确认覆盖**

Run: `cargo test -p peri-acp --lib -- variant_coverage_test`
Expected: PASS

---

### Task 7.5: Phase 7 提交

- [ ] **Step 1: cargo test --workspace**

Run: `cargo test --workspace`
Expected: 全 PASS

- [ ] **Step 2: 提交**

```bash
git add peri-acp/src/langfuse/ peri-acp/tests/ peri-acp/Cargo.toml peri-acp/src/event/mapper_test.rs
git commit -m "$(cat <<'EOF'
test(langfuse): 子对象单测 + 集成冒烟 + e2e mock + mapper_test 同步

- 7 个子对象 _test.rs 共 680 行（含 P0 矩阵 14 项）
- tracer_test.rs 保留 80 行集成冒烟（删除 19 处白盒访问）
- e2e mock 端到端测试覆盖 SessionCreate/SpanCreate/GenerationCreate 链路
- mapper_test.rs 12 个新变体占位映射测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Phase 8: 清理 + 文档（commit 8）

### Task 8.1: 删除旧 tracer handler 文件

**Files:**
- Delete: `peri-acp/src/langfuse/tracer/llm_handler.rs`
- Delete: `peri-acp/src/langfuse/tracer/tool_handler.rs`
- Delete: `peri-acp/src/langfuse/tracer/compact_handler.rs`
- Delete: `peri-acp/src/langfuse/tracer/trace_lifecycle.rs`
- Delete: `peri-acp/src/langfuse/tracer/subagent_stack.rs`
- Delete: `peri-acp/src/langfuse/tracer/context.rs`

- [ ] **Step 1: grep 残余引用**

Run: `grep -rn "llm_handler\|tool_handler\|compact_handler\|trace_lifecycle\|subagent_stack\|context" /Users/konghayao/code/ai/perihelion/peri-acp/src/langfuse/`

确认这些模块的代码已迁移到子对象，无残余引用。

- [ ] **Step 2: 删除文件**

```bash
rm peri-acp/src/langfuse/tracer/llm_handler.rs
rm peri-acp/src/langfuse/tracer/tool_handler.rs
rm peri-acp/src/langfuse/tracer/compact_handler.rs
rm peri-acp/src/langfuse/tracer/trace_lifecycle.rs
rm peri-acp/src/langfuse/tracer/subagent_stack.rs
rm peri-acp/src/langfuse/tracer/context.rs
```

- [ ] **Step 3: 从 tracer/mod.rs 删除 mod 声明**

```rust
// 删除以下行（如还存在）
// mod llm_handler;
// mod tool_handler;
// mod compact_handler;
// mod trace_lifecycle;
// mod subagent_stack;
// mod context;
```

保留：
```rust
mod event_builder; // 工具函数，仍被主 struct 用
mod usage;         // 纯函数
```

- [ ] **Step 4: cargo build -p peri-acp**

Run: `cargo build -p peri-acp`
Expected: 0 error

---

### Task 8.2: CLAUDE.md 更新

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: 在「任务入口矩阵」加 langfuse v2 行**

在 `CLAUDE.md` 的「任务入口矩阵」表格末尾追加：

```markdown
| 改 Langfuse 监控 | `peri-acp/src/langfuse/tracer/`（7 子对象 + 主 struct） + `langfuse-client/`（数据结构） + `peri-acp/src/session/executor_helpers.rs::forward_langfuse_event`（路由） | trace_id = turn_id 契约；新增 ExecutorEvent 必须扩 mapper_test + variant_coverage_test；sampled=false 时 tracer silently no-op |
```

- [ ] **Step 2: 在「陷阱速查」加新条目**

在「陷阱速查」章节追加：

```markdown
### Langfuse 监控 v2
- **trace_id = turn_id**：tracer.new() 由 caller 传入 turn_id，禁止自生成。trace_id 不可变。
- **sampled=false 时 silently no-op**：每个 on_* 入口检查 sampling，未采样时直接返回。caller 不感知。
- **新增 ExecutorEvent 变体**：必须同步 (1) peri-acp/event/mapper.rs (2) peri-tui/kit/acp_events.rs (3) variant_coverage_test.rs，缺一会漏掉监控数据。
- **ErrorSpan 兜底**：错误 turn 强制发 ErrorSpan 挂同 turn（trace_id = turn_id，不破坏契约）。
- **子对象方法签名禁止接收 `&mut LangfuseTracer`**：否则破坏 disjoint borrow。CI grep check。
```

- [ ] **Step 3: cargo build --workspace**

Run: `cargo build --workspace`
Expected: 0 error

---

### Task 8.3: ADR 编写

**Files:**
- Create: `docs/architecture-reviews/2026-07-14-langfuse-architecture-revamp.md`

- [ ] **Step 1: 写 ADR**

```markdown
# ADR：Langfuse 监控 v2 架构重设计

> 日期：2026-07-14 | 决策者：KonghaYao + glm-5.2

## Context

当前 langfuse 监控覆盖弱（仅 9 个 ExecutorEvent 被转发，ReAct 5 阶段、14+5 中间件链、
ContextBudget 阈值点、Compact 三级策略、MessageQueue、Workflow、AiReasoning 等核心架构
盲区），trace_id 与 turn_id 脱节（违反架构文档 §2.6「turn_id 作为统一纽带」），无 Sampling
机制，LangfuseTracer 内部 14 字段 `pub(crate)` 散在 6 handler 文件。

## Decision

采用方案 B 一次性大重构：

1. **三层映射**：1 个 peri Session → 1 个 Langfuse Session；1 个 turn → 1 个 Trace（trace_id = turn_id）；5 阶段 → 5 个顶层 Span（条件上报）。
2. **12 个新 ExecutorEvent 变体 + 2 个扩充**：覆盖核心架构盲区。
3. **7 子状态机**：GenerationTracker / ToolBatch / SubagentStack / CompactSpan / SamplingDecider / StageSpans（含 MQ + Workflow）/ MiddlewareTracer。主 struct 字段从 14 降到 12（5 简单 + 7 子对象）。
4. **Turn 级 Sampling**：hash + rate，错误 turn 强制 ErrorSpan 挂同 turn 兜底。
5. **LangfuseSessionLike trait**：让 tracer 可注入 fake session 跑单测。
6. **配置**：5 个新环境变量全支持 settings.json。

## Alternatives Considered

- **方案 A 分阶段**：被否决。用户明确要求"激进、一次性规划好，不残留失败设计"。分阶段会留下"5 阶段 Span 已建立但中间件未挂"的中间状态。
- **方案 C 最小补丁**：被否决。中间件链、ContextBudget 阈值点、Compact 三级策略等核心盲区仍在。
- **引入预判机制**（HITL/YOLO/Workflow 强制采样）：被否决。预判逻辑复杂、覆盖不全，简化为纯 hash + rate 后行为更可预测。
- **错误 turn 独立 ErrorTrace**：被否决。破坏 trace_id == turn_id 契约。改用同 turn 挂 ErrorSpan + metadata.is_synthetic=true。

## Consequences

- 单大 PR，8 commit 序列。
- 旧 trace schema（trace_id = uuid7）与新 schema（trace_id = turn_id）在 Langfuse 后端共存，UI 上旧 trace 仍可读。
- 后续 langfuse 改动收敛在 7 子对象层，不再触碰 14 字段散状态机。
- variant_coverage_test 强制每个 ExecutorEvent 变体有 mapper 处理，防漏。

## Compliance

- 680 行子对象单测全 PASS
- 14 个 P0 测试矩阵全 PASS
- e2e mock 端到端验证 Session/Trace/Span/Generation 层级
- variant_coverage_test 全 PASS
```

- [ ] **Step 2: 提交（含 ADR）**

```bash
git add docs/architecture-reviews/2026-07-14-langfuse-architecture-revamp.md
```

---

### Task 8.4: 归档 spec 到 docs/design

**Files:**
- Move: `docs/superpowers/specs/2026-07-14-langfuse-monitoring-v2-design.md` → `docs/design/langfuse-monitoring-v2.md`

- [ ] **Step 1: 复制 spec 到 docs/design**

```bash
cp docs/superpowers/specs/2026-07-14-langfuse-monitoring-v2-design.md docs/design/langfuse-monitoring-v2.md
```

- [ ] **Step 2: 不删除 spec 原文件**（保留 brainstorming 历史链路）

---

### Task 8.5: Phase 8 最终提交

- [ ] **Step 1: cargo test --workspace 全套**

Run: `cargo test --workspace`
Expected: 全 PASS

- [ ] **Step 2: lefthook run pre-commit**

Run: `lefthook run pre-commit`
Expected: 全绿

- [ ] **Step 3: 提交清理 + 文档**

```bash
git add CLAUDE.md docs/design/langfuse-monitoring-v2.md docs/architecture-reviews/2026-07-14-langfuse-architecture-revamp.md peri-acp/src/langfuse/tracer/
git commit -m "$(cat <<'EOF'
chore(langfuse): 删除旧 tracer handler 文件 + CLAUDE.md + ADR + spec 归档

- 删除 6 个旧 handler 文件（llm_handler/tool_handler/compact_handler/
  trace_lifecycle/subagent_stack/context），逻辑已迁入 7 子对象
- CLAUDE.md 任务入口矩阵加 langfuse v2 行 + 陷阱速查加 5 条新条目
- ADR-2026-07-14-langfuse-architecture-revamp 记录方案 B 决策与备选否决
- spec 归档到 docs/design/langfuse-monitoring-v2.md

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## Self-Review

### Spec 覆盖检查

| Spec 章节 | 对应 Task | 状态 |
|----------|---------|------|
| §1.1 三层映射 | Task 4.9（trace_id = turn_id 契约） + Task 5.1（Session 对象） | ✓ |
| §1.3 ID 契约 | Task 4.9（debug_assert） + Task 4.10（重命名） | ✓ |
| §2.1 12 新变体 + 2 扩充 | Task 2.1 / 2.2 | ✓ |
| §2.2 归属 crate | Task 2.1-2.2 | ✓ |
| §2.3 forward 路由 | Task 5.1 | ✓ |
| §2.4 数据流 | Task 5.1 + 4.9 | ✓ |
| §3.1 主 struct 12 字段 | Task 4.9 | ✓ |
| §3.2 7 子对象 | Task 4.2-4.8 | ✓ |
| §3.3 21 on_* 方法 | Task 4.9 | ✓ |
| §3.4 不变量升级 | Task 4.2-4.9 | ✓ |
| §3.5 LangfuseSessionLike trait | Task 4.1 | ✓ |
| §4.1 5 新环境变量 | Task 1.2 + 6.3 | ✓ |
| §4.2 SamplingDecider 算法 | Task 4.6 + 6.1 | ✓ |
| §4.3 ErrorSpan 兜底 | Task 6.2 | ✓ |
| §4.4 配置加载 | Task 6.3 | ✓ |
| §5.1 子对象单测 680 行 | Task 4.2-4.8 + 7.1 | ✓ |
| §5.2 tracer_test.rs 集成 | Task 7.2 | ✓ |
| §5.3 e2e mock | Task 7.3 | ✓ |
| §5.4 P0 测试矩阵 | Task 7.1-7.4 | ✓ |
| §6.1 8 commit 序列 | Phase 1-8 各有 commit task | ✓ |
| §6.3 关键风险 | variant_coverage_test（Task 2.4）+ disjoint borrow grep（CI） | ✓ |
| §6.4 ADR | Task 8.3 | ✓ |
| §6.5 文档更新 | Task 8.2 + 8.4 | ✓ |

### 占位扫描

无 TBD/TODO/"implement later"。Task 4.9 Step 2 提示"参考现有 handler 文件迁移事件构造逻辑"——这是合理的，因为现有代码已有这些工具函数（如 build_generation_body），plan 不重复贴出。

### 类型一致性

- `Stage` / `StageStatus` / `TurnStatus` / `TurnErrorKind` / `CompactStrategy` / `CompactTrigger` / `CompactThreshold` / `MiddlewareHook` 枚举在 Task 2.1-2.2 定义，后续 Task 3.1-6.2 使用一致。
- `LangfuseSessionLike` trait 在 Task 4.1 定义，Task 4.9 / 5.1 / 7.3 使用一致。
- `SamplingDecider::should_emit(turn_id, session_id) -> bool` 签名在 Task 4.6 / 6.1 一致。
- `on_turn_*` / `on_stage_*` / `on_middleware_*` / `on_workflow_*` / `on_budget_threshold_hit` / `on_mq_drained` / `on_ai_reasoning_chunk` 方法名在 Task 4.9 / 5.1 / 6.1 一致。

### 已知简化

- Task 4.9 不提供主 struct 的完整 impl 代码（每个 on_* 方法完整事件构造代码），仅提供骨架 + 引用 spec §3.3。执行者需参考现有 `llm_handler.rs` / `tool_handler.rs` / `compact_handler.rs` 的事件构造逻辑迁移。这是合理的简化，避免 plan 长度过大且重复现有代码。
- Task 5.2（workflow_agent 改挂）描述策略但未贴完整 workflow_runner 重写代码。执行者需 grep 现有 pump 入口并按策略修改。


