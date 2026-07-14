# Langfuse v2 层次结构修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 v2 实现中的 4 个层次结构偏差：Generation/Tool 父 span 指向错误、0ms stage span 占位、trace 名称语义不清。对齐设计 spec §1.2（Langfuse UI 上的最终视图）。

**Architecture:** 单文件修复（`peri-acp/src/langfuse/tracer/mod.rs`），不引入新文件。核心策略：`on_llm_end` 和 `on_tool_end` 用 `self.stages.active_handle()` 动态获取当前 stage span ID 作为 parent；`on_stage_start` 改为延迟创建 span（只在 `on_stage_end` 时检查 duration > 0 才发事件）。

**Tech Stack:** Rust 2021 / serde / parking_lot

**Spec:** `docs/superpowers/specs/2026-07-14-langfuse-monitoring-v2-design.md`

---

## 问题清单

| # | 问题 | 位置 | 期望 |
|---|------|------|------|
| 1 | Generation parent = agent-run | `mod.rs:343-356` | parent = stage-reason span ID |
| 2 | SubAgent Observation parent = agent-run | `mod.rs:437` | parent = stage-act span ID |
| 3 | Compact span parent = agent-run | `mod.rs:490` | parent 保持 agent-run（Compact 是顶层 stage） |
| 4 | 0ms stage span 仍上报 | `mod.rs:582-639` | duration == 0ms 时不上报 |
| 5 | Trace name = "agent-run" | `mod.rs:125` | 保持 "agent-run"（与 agent Observation 名称一致，OK） |

---

## File Structure

| 文件 | 改动 |
|------|------|
| `peri-acp/src/langfuse/tracer/mod.rs` | `on_llm_end` parent 修复 / `on_stage_start` 延迟创建 / `on_stage_end` 条件上报 / `on_tool_end` parent 修复 |

无新建文件，无删除文件。

---

### Task 1: Generation parent → stage-reason span

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs:312-382`

- [ ] **Step 1: 在 `on_llm_end` 中获取父 span ID**

当前代码（`mod.rs:343`）：
```rust
let current_agent_id = self.subagent.current_agent_id(&self.agent_observation_id);
```
替换为动态查找 stage span：
```rust
// 优先使用当前活跃 stage span 作为父 observation
// Reason stage → Generation 挂 stage-reason 下
let parent_id = self
    .stages
    .active_handle()
    .map(|h| h.span_id.clone())
    .unwrap_or_else(|| self.agent_observation_id.clone());
```

- [ ] **Step 2: 更新 GenerationCreate 的 parent_observation_id**

当前代码（`mod.rs:356`）：
```rust
parent_observation_id: Some(current_agent_id),
```
替换为：
```rust
parent_observation_id: Some(parent_id),
```

- [ ] **Step 3: cargo build -p peri-acp 确认编译通过**

Run: `cargo build -p peri-acp`
Expected: 0 error

- [ ] **Step 4: cargo test -p peri-acp --lib 确认测试通过**

Run: `cargo test -p peri-acp --lib`
Expected: 全部 PASS

---

### Task 2: Tool / SubAgent parent → stage-act span

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs:425-464`

- [ ] **Step 1: 在 `on_tool_end` (SubAgent 分支) 中获取父 span ID**

当前代码（`mod.rs:437`）：
```rust
parent_observation_id: Some(self.agent_observation_id.clone()),
```
替换为动态查找 stage span：
```rust
parent_observation_id: Some(
    self.stages
        .active_handle()
        .map(|h| h.span_id.clone())
        .unwrap_or_else(|| self.agent_observation_id.clone())
),
```

- [ ] **Step 2: cargo build -p peri-acp 确认编译通过**

Run: `cargo build -p peri-acp`
Expected: 0 error

- [ ] **Step 3: 测试**

Run: `cargo test -p peri-acp --lib`
Expected: 全部 PASS

---

### Task 3: 0ms stage span 条件上报

**设计决策**：将 stage span 从"先创建后更新"改为"延迟创建"——`on_stage_start` 只通知子对象，不发送 SpanCreate。`on_stage_end` 检查 duration > 0，是则发送 SpanCreate（含 start + end 时间），否则静默跳过。

**Files:**
- Modify: `peri-acp/src/langfuse/tracer/mod.rs:565-640`

- [ ] **Step 1: 修改 `on_stage_start` — 移除 SpanCreate 发送**

当前代码（`mod.rs:565-598`）在 `on_stage_start` 末尾会构造 `SpanBody` 并发送 `IngestionEvent::SpanCreate`。改为：只调用 `self.stages.on_stage_start()` 注册 span，**不发送任何事件**。

替换 `on_stage_start` 方法体为：

```rust
pub fn on_stage_start(&mut self, stage: Stage, turn_id: &str) {
    if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
        return;
    }
    let _handle = self.stages.on_stage_start(
        stage,
        &self.trace_id,
        turn_id,
        &self.agent_observation_id,
    );
    // SpanCreate 延迟到 on_stage_end：仅在 duration > 0 时发送
}
```

- [ ] **Step 2: 修改 `on_stage_end` — 条件发送 SpanCreate + SpanUpdate**

当前代码（`mod.rs:601-639`）总是发送 `SpanUpdate`。改为：从 `self.stages` 取出 active handle（on_stage_end 会清空 active），计算 duration，如果 `duration > 0` 则发送 SpanCreate（合并 start + end），否则静默跳过。

完整替换 `on_stage_end`：

```rust
pub(crate) fn on_stage_end(
    &mut self,
    handle: &crate::langfuse::tracer::stages::StageHandle,
    status: StageStatus,
) {
    if !self.sampling.should_emit(&self.trace_id, &self.session_id) {
        return;
    }

    let end_time = now_rfc3339();
    let duration_ms = calculate_duration_ms(&handle.start_time, &end_time);

    // 0ms stage span 不上报（条件上报：Compact 阈值以下 / Act 无工具调用 等）
    if duration_ms == 0 {
        self.stages.on_stage_end(handle, status);
        return;
    }

    let level = match status {
        StageStatus::Error => Some(ObservationLevel::Error),
        _ => Some(ObservationLevel::Default),
    };

    // 合并 SpanCreate + SpanUpdate 为单个 SpanCreate（含 end_time）
    let span_body = SpanBody {
        id: Some(handle.span_id.clone()),
        trace_id: Some(handle.trace_id.clone()),
        name: Some(format!("stage-{:?}", handle.stage).to_lowercase()),
        start_time: Some(handle.start_time.clone()),
        end_time: Some(end_time.clone()),
        input: None,
        output: Some(serde_json::json!({
            "status": format!("{:?}", status),
            "duration_ms": duration_ms,
        })),
        metadata: None,
        level,
        status_message: None,
        version: Some(VERSION.to_string()),
        environment: None,
        parent_observation_id: Some(handle.parent_observation_id.clone()),
        session_id: Some(self.session_id.clone()),
    };
    let event = IngestionEvent::SpanCreate {
        id: new_uuid(),
        timestamp: end_time,
        body: span_body,
        metadata: None,
    };
    try_add_or_warn_via_session(&*self.session, event, &self.trace_id, "Stage SpanCreate");

    self.stages.on_stage_end(handle, status);
}
```

- [ ] **Step 3: 添加 `calculate_duration_ms` 辅助函数**

在 `mod.rs` 文件末尾（`impl LangfuseTracer` 之后）添加：

```rust
/// 计算两 RFC3339 时间戳之间的毫秒差。
/// parse 失败时返回 0（保守：不上报 0ms span）。
fn calculate_duration_ms(start: &str, end: &str) -> u64 {
    let s = chrono::DateTime::parse_from_rfc3339(start).unwrap_or_default();
    let e = chrono::DateTime::parse_from_rfc3339(end).unwrap_or_default();
    let dur = e.signed_duration_since(s);
    if dur.num_milliseconds() > 0 {
        dur.num_milliseconds() as u64
    } else {
        0
    }
}
```

- [ ] **Step 4: 清理 `on_stage_end` 中不再需要的导入**

`SpanUpdate` 不再使用，确认 `mod.rs` 顶部 `use langfuse_client::IngestionEvent` 没有报 unused variant warning。如有，加 `#[allow(unused_imports)]` 或移除 `SpanUpdate` 引用（但 `SpanUpdate` 可能在其他地方使用——`on_compact_end` 仍用 `SpanUpdate`，所以不需要改）。

- [ ] **Step 5: cargo build -p peri-acp 确认编译通过**

Run: `cargo build -p peri-acp`
Expected: 0 error

- [ ] **Step 6: cargo test -p peri-acp --lib 确认测试通过**

Run: `cargo test -p peri-acp --lib`
Expected: 全部 PASS

---

### Task 4: Trace 名称修正

**设计决策**：Trace 的 name 在 Langfuse 中由 Observation name 推断。当前为 "agent-run"，符合 Agent 类型语义。**无需修改**。如需区分 turn，可通过 trace 级 metadata 添加 `turn_number`，但在当前设计中 trace_id = turn_id 已可区分，不增加复杂度。

- [ ] **Step 1: 确认不改**（已与设计 spec 对齐）

---

### Task 5: 全量回归测试 + 提交

- [ ] **Step 1: cargo test --workspace**

Run: `cargo test --workspace`
Expected: 全 PASS（无回归）

- [ ] **Step 2: cargo check --workspace**

Run: `cargo check --workspace`
Expected: 0 error

- [ ] **Step 3: lefthook run pre-commit**

Run: `lefthook run pre-commit`
Expected: typos ✅ / check（warning OK）

- [ ] **Step 4: 提交**

```bash
git add peri-acp/src/langfuse/tracer/mod.rs
git commit -m "$(cat <<'EOF'
fix(langfuse): 修复 v2 层次结构 — Generation/Tool 父 span + 0ms 条件上报

- on_llm_end: parent 从 agent-run 改为当前活跃 stage span（stage-reason）
- on_tool_end: SubAgent parent 从 agent-run 改为当前活跃 stage span（stage-act）
- on_stage_start/end: 延迟创建 span，duration==0 时不上报（条件上报）
- 新增 calculate_duration_ms 辅助函数

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>
EOF
)"
```

---

## 附录：修复前后层次对比

### 修复前

```
agent-run
├── GENERATION step-1     parent=agent-run ❌
├── GENERATION step-2     parent=agent-run ❌
├── SPAN stage-compact    parent=agent-run
├── SPAN stage-receive    parent=agent-run (0ms ⚠️)
├── SPAN stage-reason     parent=agent-run
├── SPAN stage-act        parent=agent-run (0ms ⚠️)
└── SPAN stage-end        parent=agent-run
```

### 修复后

```
agent-run
├── SPAN stage-compact    parent=agent-run (仅 duration > 0)
├── SPAN stage-reason     parent=agent-run
│   ├── GENERATION step-1 parent=stage-reason ✅
│   └── GENERATION step-2 parent=stage-reason ✅
└── SPAN stage-end        parent=agent-run
```

Receive/Act 如果 duration == 0 则不出现在树上（条件上报）。
