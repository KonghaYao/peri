# P5 v1 删除手工检查清单

> 自动化删除由 workflow `p5-v1-removal` 处理；本文档列出**必须人工操作**的点。
> 接手者按顺序勾选；每项完成后跑 `cargo build --workspace` 确认无回归。
> 创建于 2026-06-25，分支 `feature/v2-architecture`。

---

## 自动化执行结果（workflow `p5-v1-removal`，2026-06-25）

### 已完成
- 删除 v1 executor 7 文件（约 3500 行）
- SubAgent 5 调用点 stub 化（待 §A.1 人工迁移）
- Hook 1 调用点 stub 化（待 §A.2 人工迁移）
- PERI_USE_V1 双轨清理
- react.rs 中 ReActAgent struct + impl 删除（保留 Reasoning/ToolCall/ToolResult/ReactLLM）

### 编译状态
`cargo build --workspace` **失败 4 处**（`build_ok=false`），集中在 `peri-middlewares` crate：

1. `peri-middlewares/src/subagent/spawner.rs` — `unresolved import peri_agent::agent::AgentCancellationToken`
2. `peri-middlewares/src/subagent/tool/define.rs` — `unresolved import peri_agent::agent::AgentCancellationToken`
3. `peri-middlewares/src/subagent/tool/execute_bg.rs` — `unresolved imports AgentCancellationToken, ReActAgent`
4. `peri-middlewares/src/subagent/tool/execute_fork.rs` — `unresolved imports AgentCancellationToken, ReActAgent`

根因：v1 `AgentCancellationToken` / `ReActAgent` 已随 executor 模块删除，stub 仍引用旧类型别名。
6 条 warning（unreachable statement / unused variable）位于上述 stub，迁移完成后会消失。

### 待人工处理（按本 checklist §A/§B/§C 顺序）
- §A SubAgent/Hook 迁移到 v2 stages（先修复上述 4 处编译错误）
- §B 测试迁移
- §C examples 评估

---

## A. SubAgent / Hook 迁移到 v2 stages（P5.1 + P5.2，核心工作）

v1 executor 删除后，SubAgent/Hook 的 `.execute()` 入口消失。workflow 会把这些调用点
**stub 化**（`unimplemented!("P5.1 待迁移")` 或类似）。本节任务：把它们改回真正可工作的
v2 stages 调用。

### A.1 SubAgent

- [ ] **`peri-middlewares/src/subagent/spawner.rs`** — 后台 task spawner
  - 当前（v1）：构造 `AgentState` → `agent_builder.execute(...)` → 收集 messages → 返回 `BackgroundTaskResult`
  - 目标（v2）：用 `build_and_execute_agent_v2(...)` 替换 `.execute()` 调用，处理 `ExecOutcome` 返回
  - 关键：`spawn_thread_store` / `spawn_parent_messages` / `spawn_child_thread_id` 这些 fork 语义需要在 v2 transcript 层重新设计
  - 参考：`peri-acp/src/session/executor.rs::build_and_execute_agent_v2`

- [ ] **`peri-middlewares/src/subagent/tool/define.rs`** — SubAgent 工具定义
  - 当前：`.execute(AgentInput::text(prompt), &mut state, Some(child_cancel))`
  - 目标：v2 stages 调用；cancel 通过 `Session::new_with_cancel` 父子链接
  - 测试：`define.rs` 相关单元测试需要重写

- [ ] **`peri-middlewares/src/subagent/tool/execute_bg.rs`** — 后台 Fork
  - 当前：在 tokio task 内 `.execute()` + 通过 `BackgroundTaskCompleted` 事件回传
  - 目标：v2 stages 在独立 Session + 独立 EventBus 上跑
  - 注意：v2 `MessageTranscript` 需要支持父子链接（ancestor 字段）

- [ ] **`peri-middlewares/src/subagent/tool/execute_fork.rs`** — 同步 Fork
  - 当前：`.execute()` 阻塞等待结果
  - 目标：v2 stages 同步调用 + 父 transcript 合并子结果
  - 测试：fork 场景 cancel policy 父→子传播验证

### A.2 Hook

- [ ] **`peri-middlewares/src/hooks/executor.rs:340-375`** — hook agent 执行
  - 当前：`let agent = ReActAgent::new(llm).max_iterations(max_turns); agent.execute(...)`
  - 目标：v2 stages 调用；注意超时 `tokio::time::timeout` 包装保留
  - 简化路径：hook agent 不需要 SubAgent 那么复杂，可以直接调 `build_and_execute_agent_v2` 单 turn

### A.3 SubAgent 共享数据

- [ ] **`SubAgentMiddleware::with_frozen_data`** — frozen 透传链
  - 当前：frozen CLAUDE.md / Skills 从 main agent 透传到子 agent
  - 目标：v2 stages 的 `StageContext` 也需要支持 frozen data 注入（检查 `build_stage_context` 是否已支持）

---

## B. 测试迁移（P5.4，约 91 个测试）

v1 executor 测试文件 workflow 会直接删除（与 executor 一起）。但部分测试逻辑（如 tool_dispatch 行为、HITL 审批、cancel policy）需要迁移到 v2 stages 测试。

### B.1 删除（workflow 自动处理，无需人工）

- [x] `peri-agent/src/agent/executor/mod_test.rs`（53KB，自动删除）
- [x] `peri-agent/src/agent/executor/tool_dispatch_test.rs`（46KB，自动删除）

### B.2 需要人工迁移的测试场景

- [ ] **HITL 审批路径** — v1 在 `executor/tool_dispatch.rs` 处理 `is_edit_tool` + 审批列表；v2 在 `stages/tool_dispatch.rs` 处理，但相关测试需要重新构造
- [ ] **Cancel policy** — v1 在 `executor/mod.rs` 测 cancel 父→子传播；v2 stages 的 cancel 测试可能不完整
- [ ] **Max iterations** — v1 默认 10，TUI 覆盖为 500；v2 stages 是否有同样限制？
- [ ] **ErrorSuggest 注入** — v1 在 `tool_dispatch.rs::collect_tool_results` 注入；v2 在 `stages/tool_dispatch.rs` 注入，测试需迁移
- [ ] **Goal steering** — `goal_middleware.rs` 注入 `<goal-message>` 标签的测试

### B.3 集成测试

- [ ] `peri-agent/tests/agent_tests.rs` — 评估：是否还用 ReActAgent？是→迁移 v2 / 否→保留
- [ ] `peri-agent/tests/integration_tests.rs` — 同上
- [ ] `peri-agent/tests/middleware_tests.rs` — middleware 测试可能不依赖 ReActAgent，但需要确认

---

## C. 示例代码（examples）

- [ ] `peri-agent/examples/basic_agent.rs` — 入门示例，应改成 v2 stages 入口
- [ ] `peri-agent/examples/tool_integration.rs` — 工具集成示例
- [ ] `peri-agent/examples/custom_middleware.rs` — 自定义 middleware 示例

**决策点**：是否保留 examples？选项：
1. 删除（用户不用）
2. 改为 v2 stages（教育用户如何用 v2）
3. 改为最简 LLM 调用（不依赖 stages）

建议：选项 2，作为 v2 文档的一部分。

---

## D. PERI_USE_V1 双轨清理（workflow 自动处理 + 人工验证）

### D.1 workflow 自动删除

- [x] `peri-acp/src/session/executor.rs:1111` — 双轨分支删除，v2 成为唯一路径
- [x] `peri-acp/src/session/mod.rs:62` — v2_message_queue 注释更新

### D.2 人工验证

- [ ] `CLAUDE.md` — 「v2 架构状态」段落更新：移除「PERI_USE_V1=1 回退」说明，强调「v2 单路径」
- [ ] `docs/v2-process/2026-06-25-v2-default.md` — 在底部加「后续：v1 已物理删除，本快照保留作历史」
- [ ] `docs/v2-process/verification.md` — 移除 `PERI_USE_V1=1 cargo test` 步骤

---

## E. 类型边界审查（防止误删 v1+v2 共享类型）

以下类型**必须保留**（v1+v2 共享，删除会破坏 v2）：

- [ ] **确认保留** `Reasoning` / `ToolCall` / `ToolResult` / `ReactLLM`（`react.rs`）
  - v2 stages 引用：`stages/{act,mod,reason,tool_dispatch}.rs`
  - **只删除 `ReActAgent` struct + `impl ReActAgent`**
- [ ] **确认保留** `AgentEvent`（`agent/events.rs`）
  - ACP/TUI 全在用（mapper_v2 / event_sink / session/command 等）
- [ ] **确认保留** `AgentState` / `State` trait（`agent/state.rs`）
  - v2 stages 内部 middleware_runner / compact / act 都用
  - 注意：可能需要把 v1 executor 专用的方法从 AgentState 移除（如果有）
- [ ] **确认保留** `MessageQueue`（`messages/message_queue.rs`）
  - goal_middleware / hooks / TUI 都在用

---

## F. 验证（每完成一节都跑）

- [ ] `cargo build --workspace` — 编译过
- [ ] `cargo test --workspace --lib` — 测试数 ≥ 切默认基线（2929 - 删除的 v1 测试数）
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — 零 warning
- [ ] `cargo fmt --all -- --check` — 零 diff
- [ ] `grep -r 'ReActAgent' --include='*.rs'` — **零结果**（最终目标）
- [ ] `grep -r 'trait Middleware<S' --include='*.rs'` — **零结果**（最终目标）
- [ ] `grep -r 'PERI_USE_V1' --include='*.rs'` — **零结果**
- [ ] `grep -r 'use peri_agent::agent::events::AgentEvent' --include='*.rs'` — **可以非零**（AgentEvent 是共享类型）

---

## G. 完成后

- [ ] 更新 `CLAUDE.md`：移除「合理双轨」段落，强调 v2 单路径
- [ ] 写快照 `docs/v2-process/YYYY-MM-DD-p5-complete.md`
- [ ] 更新 `docs/v2-process/README.md` 一句话状态
- [ ] 更新 `docs/v2-process/roadmap.md` 标 P5 完成
- [ ] dogfood 1 周（开发者本地跑 v2-only 路径）

---

## 执行顺序建议

1. ✅ workflow `p5-v1-removal` 自动：删 executor / stub SubAgent/Hook / 删双轨
2. ⏳ 人工 A：迁移 SubAgent/Hook 到 v2 stages（最大工作量）
3. ⏳ 人工 B：迁移关键测试场景到 v2 stages 测试
4. ⏳ 人工 C：评估 examples
5. ⏳ 人工 D：验证最终 grep 零结果
6. ⏳ 写完成快照 + 更新 CLAUDE.md
