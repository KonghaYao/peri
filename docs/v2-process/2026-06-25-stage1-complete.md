# 2026-06-25 快照：P1–P4 完成 + Stage 1 紧急修复

**日期**：2026-06-25
**分支**：`feature/workflow-ultracode`
**基线**：2912 测试全过 / `cargo build --workspace` 绿 / clippy 零 warning / fmt 零 diff
**v2 opt-in 开关**：环境变量 `PERI_USE_V2=1`（未设置则走 v1 完全向后兼容）

---

## 1. 总体位置

```
┌────────────────────────────────────────────────────────────────┐
│  v2 重做路线图                                                  │
├────────────────────────────────────────────────────────────────┤
│  P1 Middleware trait 切换            ✅ 98626062               │
│  P2 Stages 真实化                    ✅ 177cc517               │
│  P3 ACP Executor 切换                ✅ f02fc9ef + 7b8d9761    │
│  P4 TUI/Stdio DTO 化                 ✅ 88ab538b + 9432186b    │
│  ─────────────────────────────────────────────                  │
│  Workflow 7 维度审计 + 对抗验证       ✅ run wacd75vgz（16 bugs）│
│  Stage 1 紧急修复（Top 1/4/5/8）     🟡 本日完成，待提交        │
│  Stage 2 高危修复（Top 2/3/6/7）     ⏳ 待办（~135 行）         │
│  Stage 3 遥测修复（Top 9/10）        ⏳ 待办（~45 行）          │
│  手动 smoke test                     ⏳ 待办                    │
│  切默认（翻转 PERI_USE_V2）          ⏳ 依赖以上                 │
│  ─────────────────────────────────────────────                  │
│  P5 v1 物理删除                      ⏸ 阻塞（SubAgent/Hook）   │
└────────────────────────────────────────────────────────────────┘
```

**当前合理双轨**：
- **主路径**（main agent）：v2 stages（`run_react_loop`），`PERI_USE_V2=1` opt-in
- **子路径**（SubAgent fork/background/cancel + Hook agent 执行）：仍走 v1 `ReActAgent::execute()`
- v1 无法物理删除的根因：`SubAgentMiddleware` + `HookMiddleware` 直接调用 `executor.execute()`，91 个测试覆盖

---

## 2. 已完成（P1–P4 主路线图）

### 2.1 P1 — Middleware Trait 切换 ✅
**Commit**：`98626062`
- `trait Middleware` 移除 `<S: State>` 泛型
- 新增 `MiddlewareContext` / `MiddlewareContextMut` / `MiddlewareInner`（`peri-agent/src/middleware/context.rs`）
- 20 个中间件全部迁移到新签名（`impl Middleware` 而非 `impl<S: State> Middleware<S>`）
- `MiddlewareChain` 不再泛型，16 个 `run_*` 方法用新 context
- v1 executor 临时桥接（从 `AgentState` 字段构造 `MiddlewareInner`）

### 2.2 P2 — Stages 真实化 ✅
**Commit**：`177cc517`
- `StageContext` 扩展为 12 字段：`turn` / `transcript` / `queue` / `llm` / `tools` / `middleware_chain` / `event_bus` / `inner` / `context_budget` / `compact_config` / `compact_llm` / `shared_tools` / `error_suggest_registry` / `tool_registry_snapshot` / `consecutive_failures`
- `reason.rs` 接入 `before_model` + LLM + `after_model`，emit `LlmCallStart/End`
- `act.rs` 实现 3 阶段工具分发（`before_tools_batch` → 并发 invoke → `after_tool` + `after_tools_batch`）
- `tool_dispatch.rs` 移植 v1 的延迟写入不变量（before_tool/after_tool 期间 transcript 不含本轮 AI 消息）
- `compact.rs` 接入 `compact_v2::run_compact`
- `receive.rs` / `end.rs` 完成（先前阶段已就位）

### 2.3 P3 — ACP Executor 切换 ✅
**Commits**：`f02fc9ef`（P3.2 builder_v2）+ `7b8d9761`（P3.3+3.4 run_react_loop 接入）
- `builder_v2.rs::build_stage_context` 包装 v1 `build_agent`，通过 `into_parts()` 拆解出 LLM/chain/shared_tools
- `executor.rs::build_and_execute_agent_v2` 实现 9 个 Phase：
  1. 构造 `AcpAgentConfig`
  2. spawn bg event pump
  3. spawn todo forwarder
  4. spawn EventBus forwarder（v2 → v1 ExecutorEvent）
  5. seed transcript（history append_batch）
  6. push 用户输入到 v2 queue（`MessageKind::Prompt`）
  7. 运行 `run_react_loop`（max_iterations=500）
  8. 从 transcript 提取最终消息列表
  9. `LoopResult` 映射到 `PromptResult`（含 stop_reason 判定）
- `Session::new_with_cancel` 持有 linked `CancellationToken`，父级 cancel 传播
- `PERI_USE_V2=1` 环境变量切换；未设置走 v1 完全兼容

### 2.4 P4 — TUI/Stdio DTO 化 ✅
**Commits**：`88ab538b`（P4.1 DTO 层）+ `9432186b`（P4.2 TodoItemDto 迁移）
- DTO 层：`CompactFileInfoDto` / `WorkflowProgressDto` / `TokenUsageDto` / `StopReasonDto` / `TodoItemDto` / `TodoStatusDto`
- TUI 不再依赖 `peri_middlewares::tools::todo::TodoItem` 运行时类型
- `peri-tui/src/app/chat_session.rs`：`todo_items: Vec<TodoItemDto>`
- `acp_bridge.rs` 桥接层使用 `TodoItemDto` / `TodoStatusDto`
- 类型依赖（`BaseMessage` / `ContentBlock`）保留，运行时 DTO 化完成

### 2.5 Workflow 7 维度审计 ✅（run `wacd75vgz`）
**7 个维度并行审计 + 3 skeptic 对抗投票（≥2 票确认）**：
- `eventbus-events`：EventBus 三层 + mapper_v2 覆盖
- `transcript-races`：transcript 读写竞态
- `cancel-propagation`：cancel token 传播完整性
- `builder-field-mapping`：v1 → v2 字段映射
- `compact-parity`：v1/v2 compact 触发逻辑一致性
- `tool-dispatch-invariants`：tool_dispatch 不变量
- `peri-use-v2-switch`：`build_and_execute_agent_v2` 9 Phase 正确性

**审计结果**：16 个 confirmed findings，按修复优先级分三阶段。

---

## 3. 已完成（Stage 1 紧急修复 — 本日工作）🟡 待提交

**目标**：解锁 v2 路径可用性。4 个修复，约 35 行，无架构风险。

### Top 1 [Critical] — Phase 4 双推 sink ✅
**文件**：`peri-acp/src/session/executor.rs`（行 ~1325–1376）
**问题**：EventBus forwarder 同时调 `sink_for_v2.push_event(...)` 和 `tx_for_v2.send(exec_ev)`。后者会被 `spawn_event_pump`（行 :498）消费并推送 sink + Langfuse trace + pump_done 同步。直推造成 **TUI 双重渲染 + Langfuse 双计数**。
**修复**：删除 3 处 `sink_for_v2.push_event(...)` 调用，仅保留 `tx_for_v2.send(exec_ev)`。移除不再使用的 `sid_for_v2` / `sink_for_v2` / `cw_for_v2` 三个 let 绑定。

### Top 4 [High] — compact disable 检查 ✅
**文件**：`peri-agent/src/agent/stages/compact.rs`（行 ~30–39）
**问题**：v1 通过 `CompactMiddleware::is_disabled()` 在 `before_model` 钩子判定 `DISABLE_COMPACT` / `DISABLE_AUTO_COMPACT` / `auto_compact_enabled=false`。v2 stage 入口无此检查，且 `compact_v2::run_compact` 内部也不检查 `auto_compact_enabled`，导致 **v2 路径在用户禁用 compact 时仍触发**。
**修复**：在 `(budget, config)` 解构之后、token_tracker 读取之前插入检查：
```rust
let is_disabled = std::env::var("DISABLE_COMPACT").is_ok()
    || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
    || !config.auto_compact_enabled;
if is_disabled { return Ok(CompactOutput { compacted: false }); }
```

### Top 5 [High] — compact_llm 注入 ✅
**文件**：`peri-acp/src/agent/builder_v2.rs`（行 ~65–71, ~136–138）
**问题**：`ReActAgentParts` 不暴露 `compact_llm`（v1 把它存在 `CompactMiddleware` 内部），`build_stage_context` 未通过 `with_compact_llm` 注入 → v2 StageContext 的 `compact_llm` 始终为 `None` → **Full Compact 无可用 LLM，跳过摘要**。
**修复**：在 `cfg` 被 `build_agent` 消费前 clone：
```rust
let compact_llm_for_v2 = cfg
    .compact
    .model
    .clone()
    .or_else(|| cached_llm.map(|c| c.auxiliary_model.clone()));
```
然后在 builder 链加 `if let Some(llm) = compact_llm_for_v2 { builder = builder.with_compact_llm(llm); }`。

### Top 8 [Medium] — compact cancel select ✅
**文件**：`peri-agent/src/agent/stages/compact.rs`（行 ~70–90）
**问题**：`compact_v2::run_compact` 内部不感知 turn cancel_token，Full Compact 的长 LLM 调用会阻塞用户中断（Ctrl+C 无响应）。
**修复**：包进 `tokio::select! { biased; _ = ctx.turn.cancel_token.cancelled() => { 放回 transcript + 存 consecutive_failures + return AgentError::Interrupted } r = run_compact(...) => r }`。

### Stage 1 验证（本机已跑）
- `cargo build --workspace` ✅
- `cargo test --workspace --lib` ✅ 2912 passed / 0 failed / 4 ignored
- `PERI_USE_V2=1 cargo test --workspace --lib` ✅ 2912 passed / 0 failed / 4 ignored
- `cargo clippy --workspace --all-targets -- -D warnings` ✅ 零 warning
- `cargo fmt --all -- --check` ✅ 零 diff
- 独立 verifier：已派遣后台，**接手者请复核其结果**（若未完成则自行重跑）

### Stage 1 提交计划
**建议 commit message**：
```
fix(v2): Stage 1 紧急修复 — Phase 4 双推 sink / compact disable / compact_llm 注入 / cancel select

- executor.rs: 删除 Phase 4 forwarder 3 处 sink_for_v2.push_event（spawn_event_pump 已订阅 event_tx）
- stages/compact.rs: 顶部加 disable 检查（对齐 CompactMiddleware::is_disabled）
- builder_v2.rs: 提取 cfg.compact.model 并通过 with_compact_llm 注入
- stages/compact.rs: run_compact 包进 select! biased 与 cancel_token.cancelled()

Refs: workflow run wacd75vgz, Stage 1 (Top 1/4/5/8)
```

---

## 4. 当前合理双轨（接手者必读）

```
┌─────────────────────────────────────────────────────────────┐
│  主路径（PERI_USE_V2=1）                                     │
│  run_session_loop                                           │
│    → build_and_execute_agent_v2                             │
│      → builder_v2::build_stage_context                      │
│        → v1 build_agent（仅复用装配，不 execute）            │
│          → ReActAgentParts                                  │
│      → run_react_loop（v2 stages: reason/act/compact/end）  │
│      → EventBus forwarder → ExecutorEvent → spawn_event_pump│
│                                                             │
│  子路径（无论 PERI_USE_V2 是否设置，SubAgent/Hook 都走这里）│
│  SubAgentMiddleware / HookMiddleware                        │
│    → ReActAgent::execute()（v1）                            │
│    → 91 个测试覆盖                                          │
└─────────────────────────────────────────────────────────────┘
```

**为什么不能现在物理删除 v1**：
- `peri-middlewares/src/subagent/tool/{define,execute_bg,execute_fork}.rs` 调用 `executor.execute()`
- `peri-middlewares/src/hooks/executor.rs` 调用 `executor.execute()`
- 91 个 v1 executor 测试覆盖上述路径
- 迁移工作量评估：>2000 行（4–6 人周），属独立里程碑

---

## 5. 关键陷阱（CLAUDE.md 摘录）

- **[TRAP]** 中间件 `before_tool`/`after_tool`/`on_error` 不读 `state.messages()` —— `tool_dispatch.rs` 延迟写入要求 collect/dispatch 两阶段间 state 不被读取
- **[TRAP]** 新增 `AgentEvent` 变体必须同步更新 TUI 侧 `map_executor_event` 映射
- **[TRAP]** `Interrupted`/`Error` 与 `Done` 互斥：前者先 `request_rebuild()` + `reconcile_already_done=true`
- **[TRAP]** 中途纠正消息必须用 `BaseMessage::human(...)`（`<system-reminder>` 标签），禁止 `BaseMessage::system(...)` —— invoke.rs 会 hoist 污染 frozen prompt
- **[TRAP]** Prompt Cache 前缀稳定性：非 System 消息必须用 `add_message`（尾部追加），禁止 `prepend_message`
- **[TRAP]** SubAgent 中间件链必须复用 main agent `session/new` 时 frozen 的 CLAUDE.md/Skills 数据
- **[TRAP]** `PromptFeatures::detect()` 仍每轮读取 `YOLO_MODE`/`is_git_repo`，未 frozen —— SubAgent 可能漂移
- **[TRAP]** Immediate 命令绕过 agent event pump，必须手动 `sink.push_done()`
- **[TRAP]** `std::sync::RwLockReadGuard` 不是 `Send` —— async 中不能跨 `.await` 持有，用 `parking_lot::RwLock`

---

## 6. 接手者下一步建议

### 短期（1–3 天）
1. 复核 Stage 1 verifier 输出（若已结束）；若 verifier 给 PARTIAL/FAIL，按反馈修正
2. 提交 Stage 1（commit message 见 §3.5）
3. 跑手动 smoke test：`PERI_USE_V2=1 cargo run -p peri-tui -- -a`，跑一轮「对话 + 工具调用 + Compact + Cancel」
4. 进入 Stage 2（详见 `roadmap.md`）

### 中期（1–2 周）
1. 完成 Stage 2 + Stage 3 修复
2. 翻转 `PERI_USE_V2` 默认（v1 改 opt-in）
3. 在 main 分支公告 v2 切换

### 长期（独立里程碑）
1. P5 完全消除双轨（详见 `roadmap.md` §P5）
2. 物理删除 `executor/mod.rs` + `state.rs::State` + `events.rs::AgentEvent`

---

## 7. 索引

- 仓库根 `CLAUDE.md` —— 项目工作守则（必读）
- `~/.claude/plans/majestic-zooming-haven.md` —— P1–P5 原始计划
- `docs/superpowers/plans/2026-06-24-v2-architecture-status.md` —— 6-24 历史快照
- `docs/design/peri-agent-*.md` —— v2 设计文档（10 份）
- `docs/v2-process/roadmap.md` —— 剩余路线
- `docs/v2-process/files-index.md` —— 相关文件索引
- `docs/v2-process/verification.md` —— 验证步骤
