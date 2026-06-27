# 2026-06-25 快照：Stage 4 完成（Langfuse Generation 端到端完整性）

**日期**：2026-06-25
**分支**：`feature/v2-architecture`
**触发**：Stage 3 verifier 对抗性探针发现 2 个 observation（切默认后升级为主路径缺陷）
**Commit**：`9d3c1bc6`（单 commit 涵盖 Top 11+12；两 Top 在同 workflow、同 3 文件、同 Langfuse Generation 主题，紧耦合）
**基线**：默认（v2）与 `PERI_USE_V1=1` 双路径测试等价 —— 2929 passed（Stage 3 的 2927 + 2 新测试）

---

## 1. Stage 4 概览

| Top | 状态 | 文件数 | 新增测试 |
|-----|------|--------|---------|
| Top 11 LlmCallEnd.output | ✅ | 3 | 1 |
| Top 12 LlmCallStart.messages+tools | ✅ | 3 | 1 |

**关键发现 vs Stage 3 verifier report**：
- Stage 3 verifier 在对抗性探针段标记 2 个 observation（不阻塞 Stage 3 PASS，记为「下一 Stage 处理」）
- 切默认后这两个 observation 立即影响所有用户的 Langfuse trace，升级为 Stage 4 必修
- 经验教训：**切默认决策点必须把 verifier observation 当作「立即修复」而非「下一里程碑」**

## 2. 各 Top 修复详情

### Top 11 — LlmCallEnd.output 端到端透传

**问题**：v2 `ObserveEvent::LlmCallEnd` 缺 `output` 字段，`mapper_v2.rs:114` 硬编码 `String::new()` → Langfuse Generation output 始终为空。

**修复（3 文件）**：
1. `events_v2.rs::ObserveEvent::LlmCallEnd` 加 `output: String` 字段 + docstring
2. `reason.rs` 错误路径：`output: format!("ERROR: {}", e)`
3. `reason.rs` 成功路径：`output: reasoning.final_answer.clone().unwrap_or_else(|| reasoning.thought.clone())`（与 v1 `llm_step.rs:92-93` 对齐）
4. `mapper_v2.rs:114` 删除 `String::new()` 硬编码，透传 `output`

### Top 12 — LlmCallStart.messages+tools 端到端透传

**问题**：v2 `ObserveEvent::LlmCallStart` 只携带 `turn_id/agent_id/step`，`mapper_v2.rs:100-104` 直接返回 `None` → Langfuse `on_llm_start` 永不触发，Generation 缺 input 端。

**修复（3 文件）**：
1. `events_v2.rs::ObserveEvent::LlmCallStart` 加 `messages: Arc<Vec<BaseMessage>>` + `tools: Vec<ToolDefinition>`
2. `reason.rs:38` emit 时填入 `start_messages`（复用已有 `messages_snapshot`）+ `start_tools`（`tool_refs.iter().map(|t| t.definition()).collect()`）
3. `mapper_v2.rs:100-104` 改为 `Some(ExecutorEvent::LlmCallStart { step, messages, tools })`，删除 TODO 注释

## 3. 验证

```
cargo build --workspace                                    ✅
cargo test --workspace --lib                               ✅ 2929 passed / 0 failed / 4 ignored（v2 默认）
PERI_USE_V1=1 cargo test --workspace --lib                 ✅ 2929 passed / 0 failed / 4 ignored（v1 回退）
cargo clippy --workspace --all-targets -- -D warnings      ✅ 零 warning
cargo fmt --all -- --check                                 ✅ 零 diff
```

测试分布：30 + 55 + 238 + 704 + 28 + 979 + 673 + 14 + 176 + 32 = 2929（与 Stage 3 基线 2927 相比 +2，对应两个新 e2e 测试）。

## 4. v2 → v1 Langfuse Generation 完整性对照

| 字段 | v1（executor/llm_step.rs） | v2（Stage 4 后） | 状态 |
|------|---------------------------|-----------------|------|
| LlmCallStart.step | ✓ | ✓ Stage 1 已修 | ✅ |
| LlmCallStart.messages | Arc<Vec<BaseMessage>> | Arc<Vec<BaseMessage>> | ✅ Stage 4 |
| LlmCallStart.tools | Vec<ToolDefinition> | Vec<ToolDefinition> | ✅ Stage 4 |
| LlmCallEnd.step | ✓ | ✓ Stage 1 已修 | ✅ |
| LlmCallEnd.model | ✓ | ✓ | ✅ |
| LlmCallEnd.output | format!("ERROR:...) 或 final_answer/thought | 同 v1 | ✅ Stage 4 |
| LlmCallEnd.usage | Option<TokenUsage> | Option<TokenUsage> | ✅ |
| LlmCallEnd.stop_reason | Option<StopReason> | 暂未携带（None） | ⏳ 未修（v2 Reasoning 已有，可后续补；不阻塞切默认） |

## 5. 工作流执行反思

### 成功的部分
- **Sequential workflow（Top 11 → Top 12）**：两 Top 共享 3 文件（events_v2/reason/mapper_v2），serial 避免并行 Edit 冲突
- **强制 diff_evidence schema**：两 agent 都提供完整 `git diff`，无 hallucination
- **主流程预设类型路径**（Reasoning.thought/final_answer、ToolDefinition、BaseTool::definition）全部存在，agent 无需探路

### 仍存在的问题
- **agent 改完不主动跑 fmt**：与 Stage 2/3 相同的固有问题。verifier agent 发现 `mapper_v2.rs:483` 单行 match arm 需折行，workflow 在 `!fmt_ok` 处正确中止 Phase 4（Docs），主流程手动 `cargo fmt --all` 后续接
- **journal 序列化的 diff_evidence hunk header 有 off-by-one**：主流程尝试用 `jq` 提取 Top 11 diff 做 patch-split，但 hunk header `-105,13 +105,14` 与实际 body 行数（-105,12 +105,13）不匹配，`git apply --check` 失败。Stage 4 最终改为单 commit（两 Top 紧耦合 + 同 workflow + 同主题，单 commit 配清晰 message 合理）

### 经验累积
1. **切默认前的 verifier observation 必须复查**：Stage 3 verifier 标 "不阻塞本 Stage" 的 observation，在切默认后立即升级为主路径缺陷。后续切默认决策点必须把 verifier observation 当作「立即修复」而非「下一里程碑」
2. **Langfuse Generation 完整性需端到端审计**：仅 mapper 层透传不够，v2 事件本身必须携带与 v1 对等的字段
3. **workflow 应在 prompt 中显式要求 agent 跑 fmt**：本次仍然依赖主流程兜底；后续 workflow prompt 模板应加「改完跑 cargo fmt --all 并把 --check 输出粘到 verification」必填项

## 6. 索引

- 仓库根 `CLAUDE.md`
- `docs/v2-process/2026-06-25-v2-default.md`（切默认快照，前置）
- `docs/v2-process/2026-06-25-stage3-complete.md`（Stage 3 快照 + verifier 观察）
- `docs/v2-process/roadmap.md`
- 本快照（`2026-06-25-stage4-complete.md`）
