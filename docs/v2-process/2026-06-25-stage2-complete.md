# 2026-06-25 快照：Stage 2 完成（高危修复）

**日期**：2026-06-25
**分支**：`feature/v2-architecture`
**Workflow run**：`wb11noou1`（Wave 1: Top 2/3/6 并行 + Wave 2: Top 7 串行）
**基线**：2924 passed / 0 failed / 4 ignored（v1 + v2 等价）/ clippy 零 warning / fmt 零 diff
**vs Stage 1 基线**：2912 → 2924（+12 测试）

---

## 1. Stage 2 概览

| Top | 状态 | Commit | 文件数 | 行数 | 新增测试 |
|-----|------|--------|--------|------|---------|
| Top 2 MessageQueue 共享 | ✅ | `004bdeab` | 4 | +135/-4 | 2 |
| Top 3 transcript 标志 | ✅ | `e0dfda30` | 1 | +105/-18 | 3 |
| Top 6 StateSnapshot 映射 | ✅ | `e11c79a7` | 11 | +262/-11 | 4 |
| Top 7 recall_items 丢失 | ✅ | `18903fd4` | 3 | +109/-1 | 3 |

**关键差异 vs roadmap 描述**（workflow Explore 验证发现）：
- Top 3：范围比文档**小**——`transcript.rs:344` 已有 `clear_flags`，只需在 `compact_v2.rs:run_compact` Full 分支入口加 5-10 行（实际 +27 行含测试）
- Top 6：roadmap 字段描述**错**——v2 `StateSnapshot` 当前只有 4 个字段，mapper 直接返回 `None` 完全丢弃。需扩展字段 + 新增独立事件变体 `StateSnapshotMeta`
- Top 7：范围比文档**大**——`build_and_execute_agent_v2` 签名不接收 recall，需改 v2 StageContext 加 `recall_buffer` 字段（最终方案比改函数签名更优雅）

---

## 2. 各 Top 修复详情

### Top 2 — session 共享 MessageQueue（`004bdeab`）

**问题**：v2 路径每 turn 新建 `MessageQueue`，SubAgent/Hook 与 main agent 互不可见 deferred/info 消息。

**修复路径**：
- `peri-agent/src/session/mod.rs`：新增 `Session::new_with_cancel_and_queue`，接收外部 `v2 MessageQueue`
- `peri-acp/src/session/mod.rs`：`AcpSession` 加 `v2_message_queue: peri_agent::session::MessageQueue` 字段
- `peri-acp/src/agent/builder_v2.rs`：`build_stage_context` 加 `shared_queue` 参数
- `peri-acp/src/session/executor.rs`：v2 dispatch 点取 `AcpSession.v2_message_queue`（缺失降级独立 queue）

**保留**：`StageContext.queue` 仍为 `MessageQueue` 类型（内部已 Arc 共享，无需改 `Arc<MessageQueue>`）。

**已知 gap（非本任务范围）**：SubAgent/Hook 实际仍在 v1 `ReActAgent::execute()` 上运行（CLAUDE.md 双轨），其 push 目标是 v1 `AgentState.message_queue`；本修复为 P5 迁移预先就位基础设施。TUI 的 v1 `MessageQueue`（不同类型）也未动。

### Top 3 — transcript 标志清除（`e0dfda30`）

**问题**：Full Compact 失败后重跑时，旧 `excluded` 标记残留会污染 `visible_messages()`。

**修复路径**：
- `peri-agent/src/agent/compact_v2.rs` `CompactStrategy::Full` 分支入口加重跑保护：当 `*consecutive_failures > 0` 时遍历 transcript，把 `excluded=true` 的消息通过 `set_excluded(id, false)` 重置
- 关键决策：**只清 excluded，保留 truncated**（truncated 属 Micro Compact 状态，不可误清）
- 首次运行（failures=0）不触发，无副作用

**生产率**：原本只需 5-10 行的改动，含 3 个测试实际 +105/-18 行。

### Top 6 — StateSnapshot 映射 + 字段扩展（`e11c79a7`）

**问题**：v2 `StateEvent::StateSnapshot` 经 `state_event_to_executor` 返回 `None`，TUI 完全收不到状态快照。

**修复路径**：
1. **字段扩展**（`events_v2.rs`）：`StateSnapshot` 加 4 字段——`current_step` / `consecutive_failures` / `budget_pct: Option<f64>` / `context_total_tokens: Option<u64>`
2. **emit 点填充**（`stages/act.rs`）：current_step 来自 turn，consecutive_failures 来自 `AtomicU32::load`，context_total_tokens 来自 `ContextBudget.context_window`；budget_pct 暂为 None（`StageContext` 无 token_tracker）
3. **新增独立事件变体**（`events.rs`）：`ExecutorEvent::StateSnapshotMeta`——结构与 v2 字段 1:1。**关键设计决策**：不重用 v1 `StateSnapshot(Vec<BaseMessage>)`，因 v2 快照设计上不携带消息历史（避免 transcript 锁开销），复用 v1 会让 TUI 误把空消息列表当完整快照，清空 `MessagePipeline::completed`
4. **mapper 补全**（`mapper_v2.rs` / `mapper.rs` / `mod.rs`）：v2 StateSnapshot → `ExecutorEvent::StateSnapshotMeta` → 新增 `AcpEvent::StateSnapshotMeta` DTO
5. **TUI 侧**（`message_pipeline/mod.rs` / `agent_ops/mod.rs`）：收到时返回 `PipelineAction::None`（不清空状态），仅打 debug 日志

**已知 gap**：budget_pct 暂为 None，未来需在 StageContext 暴露 token_tracker 才能填。

### Top 7 — recall_items 不再丢失（`18903fd4`）

**问题**：v2 路径下 middleware hook（before_agent/before_model/after_tool 等）在临时 AgentState 上 `push_recall`，restore 时 recall 被 `into_messages` 消费丢弃。

**修复路径**（采用比 roadmap 更优雅的方案）：
- `peri-agent/src/agent/stages/mod.rs`：`StageContext` 加 `recall_buffer: Arc<RwLock<Vec<String>>>` 字段
- `peri-agent/src/agent/stages/middleware_runner.rs`：`restore_from_agent_state` 签名改 `mut state`，在 `into_messages` 消费前 `drain_recall()` 并 extend 到 buffer
- `peri-acp/src/session/executor.rs`：
  - Phase 6.5：clone recall_buffer 的 Arc（在 context 被 `run_react_loop` 消费前）
  - Phase 8.5：buffer drain 出的 recall 灌入 `agent_state.push_recall`，使下游 `collect_result` 的 `drain_recall()` 取得本轮新生成的 recall（对齐 v1 路径）

**关键决策**：路线图说的「transcript ancestor 标记」方向不适用——本仓库 `incoming_recalls` 嵌入 user prompt 的 ContentBlock（`executor.rs:454-464` 共享路径），不是 transcript ancestor 消息；ancestor 标记仅用于 Fork/Background Agent。本次未改 transcript 注入逻辑，避免破坏 prompt cache 稳定性。

---

## 3. 验证（提交后重跑）

```
cargo build --workspace                                    ✅
cargo test --workspace --lib                               ✅ 2924 passed / 0 failed / 4 ignored
PERI_USE_V2=1 cargo test --workspace --lib                 ✅ 2924 passed / 0 failed / 4 ignored
cargo clippy --workspace --all-targets -- -D warnings      ✅ 零 warning
cargo fmt --all -- --check                                 ✅ 零 diff
```

v1/v2 路径**完全等价**（同一组测试，环境变量切换）。

---

## 4. 工作流执行反思

### 成功的部分
- **Wave 1 三 agent 并行**：Top 2/3/6 改动文件零重叠，并行无冲突
- **结构化输出 schema**：强制 agent 返回 `{top, status, files_changed, diff_summary, ...}`，方便汇总
- **Wave 2 携带 Top 2 上下文**：Top 7 agent 看到 Top 2 的 `files_changed` + `diff_summary`，决策更优

### 失败的部分
- **Top 3 agent hallucination**：报告 status: success + tests_added: 3，但实际**根本没改 `compact_v2.rs`**。我手动重新实施并加测试，3 测试全过
- **Top 6 agent 中间状态误报**：报告「peri-agent lib test 被 Stage 1 WIP 的 BaseMessage 导入缺失阻塞」——这是 Top 2 同时间在改 `session/mod.rs` 的中间态，最终 working tree 不存在该问题
- **agent 报告的"测试通过"不可信**：必须 grep 关键标识符 / 跑测试独立验证

### 经验教训
1. **workflow 结果必须独立验证**——不能只看 agent 自报告的 status。每个 Top 至少要 grep 关键改动 + 跑相关单测
2. **多 agent 并行改 working tree 时，agent A 看到的"pre-existing issue"可能是 agent B 的中间态**——最终态需要主流程统一验证
3. **复杂改动（如 Top 2/6/7 涉及多文件 + 跨 crate）**workflow 表现良好；**简单改动（如 Top 3 单文件 5 行）反而易被 agent 跳过**——可能因为 agent 觉得「太简单不需要测试」或漏掉了 Edit 步骤
4. **下一个 workflow 应在 agent prompt 中强制要求**：「最后必须显示 `git diff <file>` 的实际输出作为证据」

---

## 5. 接手者下一步

### 短期（1–2 天）
1. **Stage 3 — 遥测修复**（详见 `roadmap.md` §Stage 3）：
   - Top 9 [Medium] — ToolEnded output 丢失（`peri-agent/src/agent/stages/act.rs`）
   - Top 10 [Medium] — LlmCallEnd 缺 step（`peri-agent/src/agent/stages/reason.rs`）
   - 共 ~45 行，建议派遣 workflow（2 agent 并行）
2. **手动 smoke test 矩阵**（详见 `verification.md`）：跑一轮「对话 + 工具调用 + Compact + Cancel + SubAgent + DTO 渲染」

### 中期（1 周）
1. 翻转 `PERI_USE_V2` 默认值，v1 改 opt-in（`PERI_USE_V1=1`）
2. 更新 `CLAUDE.md` 「v2 架构状态」段落
3. 至少 1 周 dogfood

### 长期（独立里程碑）
- **P5** SubAgent/Hook/测试迁移 + 物理删除 v1（4–6 周，详见 `roadmap.md` §P5）

---

## 6. 索引

- 仓库根 `CLAUDE.md` —— 项目工作守则（v2 架构状态段落）
- `docs/v2-process/2026-06-25-stage1-complete.md` —— Stage 1 快照（Stage 1 实际已合并入早期 commit，本快照为历史记录）
- `docs/v2-process/roadmap.md` —— 剩余路线（Stage 3 + P5）
- `docs/v2-process/files-index.md` —— 相关文件索引
- `docs/v2-process/verification.md` —— 验证步骤
- 本快照（`2026-06-25-stage2-complete.md`）—— Stage 2 完成状态
