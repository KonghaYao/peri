# 2026-06-25 快照：Stage 3 完成（遥测修复）

**日期**：2026-06-25
**分支**：`feature/v2-architecture`
**Workflow run**：`w8tb17ryr`（Top 9 + Top 10 并行）
**基线**：2927 passed / 0 failed / 4 ignored（v1 + v2 等价）/ clippy 零 warning / fmt 零 diff
**vs Stage 2 基线**：2924 → 2927（+3 测试）

---

## 1. Stage 3 概览

| Top | 状态 | Commit | 文件数 | 行数 | 新增测试 |
|-----|------|--------|--------|------|---------|
| Top 9 ToolEnded output | ✅ | `531a8d82` | 3 | +66/-1 | 2 |
| Top 10 LlmCallEnd.step | ✅ | `35911e7b` | 2 | +68/-3 | 1 |

**关键发现 vs roadmap 描述**：
- **Top 10 emit 层早已修复**（`reason.rs:17` 已取 `step`，两个 emit 点都填）——workflow agent 只补了测试，没改代码
- **Top 10 mapper 层是新发现的 bug**——`mapper_v2.rs:111` 把 v2 step **硬编码为 0**，导致 TUI/Langfuse 拿到的 step 永远是 0。roadmap 完全没提这一层，主流程在验证阶段发现并修复

---

## 2. 各 Top 修复详情

### Top 9 — ToolEnded 携带 output（`531a8d82`）

**问题**：v2 `RenderEvent::ToolEnded` 缺 `output` 字段，`mapper_v2.rs:49` 用 `String::new()` 硬编码 → TUI 在 `PERI_USE_V2=1` 下工具输出区始终为空。

**修复路径（3 文件）**：
1. **`peri-agent/src/agent/events_v2.rs`**：`ToolEnded` 加 `output: String` 字段 + docstring 说明 emit 时机在 error_suggest 注入之前（TUI 看到的是原始输出，与 v1 一致）
2. **`peri-agent/src/agent/stages/tool_dispatch.rs`**：4 个 emit 点全部填入 output
   - 行 207 cancel 补发：`"interrupted by user"`
   - 行 237 HITL rejection：`rejection_result.output.clone()`
   - 行 250 其他错误补发：`e.to_string()`
   - 行 372 主路径：`result.output.clone()`（在 error_suggest 注入前）
3. **`peri-acp/src/event/mapper_v2.rs`**：删除 `String::new()` 硬编码 + TODO 注释，直接透传 v2 output 到 `ExecutorEvent::ToolEnd.output`

**截断策略**：对齐 v1——v1 executor tool_dispatch.rs 也未截断，全量透传。

### Top 10 — LlmCallEnd.step 端到端透传（`35911e7b`）

**问题分两层**：
1. **emit 层**（已在 Stage 1 之前的 commit 修复）：`reason.rs:17` 已取 `step = ctx.turn.current_step()`，错误路径（`:66`）与成功路径（`:88`）均 emit
2. **mapper 层（本 commit 修复，workflow 未发现）**：`mapper_v2.rs:111` 用 `..` 忽略 v2 `ObserveEvent::LlmCallEnd.step`，硬编码 `step: 0` 写入 `ExecutorEvent::LlmCallEnd` → TUI/Langfuse 拿到的 step 永远是 0

**修复路径（2 文件）**：
1. **`peri-acp/src/event/mapper_v2.rs`**：显式从 v2 `ObserveEvent::LlmCallEnd` 解构 `step`，透传到 `ExecutorEvent::LlmCallEnd.step`
2. **`peri-agent/src/agent/stages/reason.rs`**：workflow agent 新增 e2e 测试 `test_run_reason_emits_llm_call_end_with_correct_step`——通过注入可观测 EventBus，连续两次 `run_reason`（step=0 → advance_step → step=1），用 `matches!` 断言 `LlmCallStart.step` 与 `LlmCallEnd.step` 随 `turn.current_step()` 递增

**关键增强**：主流程同步增强既有测试 `test_observe_llm_call_end_maps_with_usage`——构造 `step: 7`（非零），断言映射后 `ExecutorEvent::LlmCallEnd.step == 7`，锁定 mapper 透传。

---

## 3. 验证（提交后重跑）

```
cargo build --workspace                                    ✅
cargo test --workspace --lib                               ✅ 2927 passed / 0 failed / 4 ignored
PERI_USE_V2=1 cargo test --workspace --lib                 ✅ 2927 passed / 0 failed / 4 ignored
cargo clippy --workspace --all-targets -- -D warnings      ✅ 零 warning
cargo fmt --all -- --check                                 ✅ 零 diff
```

---

## 4. 工作流执行反思

### 成功的部分
- **diff_evidence schema 字段生效**：本 workflow 在 RESULT_SCHEMA 中新增必填 `diff_evidence` 字段，强制 agent 粘贴 `git diff` 实际输出（含 +/- 行）。两个 agent 都提供了完整证据，**无 hallucination**
- **2 agent 真并行**：Top 9（events_v2 + tool_dispatch + mapper_v2）与 Top 10（reason.rs 测试）文件零重叠
- **精确行号提示**：prompt 中预先给出 4 个 emit 点行号 + 已有测试构造行号，agent 改动精准

### 仍存在的问题
- **agent 没发现 mapper 层 bug**（Top 10）：workflow agent 只补了 emit 端测试，没看 mapper_v2 是否透传。**主流程在独立 grep 验证时发现 `mapper_v2.rs:111 step: 0`**——这是 workflow 第 2 个被发现的盲区（Stage 2 Top 3 是第 1 个）
- **agent 不主动 fmt**：所有 agent 改完都没跑 `cargo fmt`，主流程统一处理

### 经验教训（累积）
1. **diff_evidence 字段是必要的**——成功阻止 Stage 2 那种 hallucination 重现
2. **agent 倾向于"按字面任务执行"**——roadmap 写「emit 时填 step」，agent 就只查 emit 端，不查 mapper；下次 prompt 应明确「端到端：emit → mapper → ExecutorEvent 字段透传全链路」
3. **主流程的独立 grep 验证不可省**——agent 报告 success 不代表任务真正完整，必须有主流程的「关键标识符 grep + 测试结果数字」交叉验证

---

## 5. Stage 1–3 累积成果

```
┌────────────────────────────────────────────────────────────────┐
│  v2 重做路线图                                                  │
├────────────────────────────────────────────────────────────────┤
│  P1 Middleware trait 切换            ✅                        │
│  P2 Stages 真实化                    ✅                        │
│  P3 ACP Executor 切换                ✅                        │
│  P4 TUI/Stdio DTO 化                 ✅                        │
│  ─────────────────────────────────────────────                  │
│  Stage 1 紧急修复（Top 1/4/5/8）     ✅ 已合并入早期 commit     │
│  Stage 2 高危修复（Top 2/3/6/7）     ✅ 4 commits（本日）       │
│  Stage 3 遥测修复（Top 9/10）        ✅ 2 commits（本日）       │
│  手动 smoke test                     ⏳ 待办                    │
│  切默认（翻转 PERI_USE_V2）          ⏳ 依赖以上                 │
│  ─────────────────────────────────────────────                  │
│  P5 v1 物理删除                      ⏸ 阻塞（SubAgent/Hook）   │
└────────────────────────────────────────────────────────────────┘
```

**所有 workflow 审计的 16 个 confirmed findings 已全部修复**（Stage 1: 4 + Stage 2: 4 + Stage 3: 2 = 10；早期 commit 已涵盖其余 6 个）。

**剩余前置工作**：
1. 手动 smoke test 矩阵（`PERI_USE_V2=1 cargo run -p peri-tui -- -a`，详见 `verification.md`）
2. 翻转 `PERI_USE_V2` 默认值

---

## 6. 接手者下一步

### 短期（1 天）
1. **手动 smoke test**（切默认前的最后 gate）：跑 `verification.md` 的测试矩阵——对话 + 工具调用 + Compact + Cancel + SubAgent + DTO 渲染
2. **结果对比 v1**：用相同 prompt 跑 v1（`PERI_USE_V2` 未设置）和 v2（`PERI_USE_V2=1`），观察 TUI 渲染、Langfuse trace、状态栏显示是否一致

### 中期（1 周）
1. **翻转 `PERI_USE_V2` 默认值**（`peri-acp/src/session/executor.rs::run_session_loop`）
2. 改 `PERI_USE_V1=1` 为回退开关
3. 更新 `CLAUDE.md` 「v2 架构状态」段落
4. 至少 1 周 dogfood（开发者本地默认走 v2，监控回归）

### 长期（独立里程碑）
- **P5** SubAgent/Hook/测试迁移 + 物理删除 v1（4–6 周，详见 `roadmap.md` §P5）

---

## 7. 索引

- 仓库根 `CLAUDE.md` —— 项目工作守则
- `docs/v2-process/2026-06-25-stage2-complete.md` —— Stage 2 快照（前置）
- `docs/v2-process/roadmap.md` —— 剩余路线（切默认 + P5）
- `docs/v2-process/files-index.md` —— 相关文件索引
- `docs/v2-process/verification.md` —— 验证步骤（含 smoke test 矩阵）
- 本快照（`2026-06-25-stage3-complete.md`）—— Stage 3 完成状态
