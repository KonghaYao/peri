> 归档于 2026-07-18，原路径 spec/issues/2026-07-18-ask-user-migration.md
# ask_user → interaction 类型迁移——消除 24 个 deprecation 警告

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18
**父 issue**：`spec/issues/residual-code-scan-20260718.md` (P0-4)

## 背景

Phase 3 中对 `peri-agent/src/ask_user/mod.rs` 的 3 个类型添加了 `#[deprecated]`，引导下游切换到新的 `interaction` 统一方案。但下游代码尚未迁移，导致 `cargo check --workspace` 产生 24 个 deprecation 警告。

### 旧类型 vs 新类型

| 旧 (ask_user) | 新 (interaction) |
|--------------|-----------------|
| `AskUserQuestionData` | `QuestionItem` |
| `AskUserOption` | `QuestionOption` |
| `AskUserBatchRequest` | `InteractionContext::Questions` |
| `AskUserBatchRequest::response_tx` | `InteractionContext` 的 reply channel |

## 消费方（需迁移的文件）

| 文件 | 使用方式 | 迁移难度 |
|------|---------|:--:|
| `peri-tui/src/kit/panel_registry.rs` | 构造 `AskUserQuestionData` 列表 | 低 |
| `peri-middlewares/src/ask_user/mod.rs` | `pub use peri_agent::ask_user::*` + `parse_ask_user()` 返回旧类型 | 高 |
| `peri-middlewares/src/tools/ask_user_tool.rs` | 工具定义引用旧类型 | 中 |
| `peri-middlewares/src/lib.rs` | re-export 旧类型（2 处） | 低 |
| `peri-agent/src/lib.rs` | prelude re-export 旧类型 | 低 |

## 迁移步骤

### 1. peri-middlewares 核心迁移（主战场）

`peri-middlewares/src/ask_user/mod.rs` 需要重写 `parse_ask_user()`：
- 输入不变（仍是 `&ToolCall`）
- 输出从 `Vec<AskUserQuestionData>` 改为 `Vec<QuestionItem>`
- `QuestionItem` 字段名从旧 `question`/`options` 改为新字段名（需确认 interaction 的类型定义）
- `AskUserOption` 简化为 `QuestionOption`

`peri-middlewares/src/tools/ask_user_tool.rs`：
- 工具 ID `ask_user_question` 保持不变
- 参数解析逻辑可能需调整

### 2. peri-tui 渲染层

`peri-tui/src/kit/panel_registry.rs`：
- 接收的 `AskUserQuestionData` 替换为 `QuestionItem`
- `AskUserOption.label/description` 替换为 `QuestionOption` 对应字段

### 3. re-export 更新

| 文件 | 旧 | 新 |
|------|----|----|
| `peri-middlewares/src/lib.rs` | `pub use ... AskUserBatchRequest, AskUserOption, AskUserQuestionData` | `pub use ... QuestionItem, QuestionOption` |
| `peri-agent/src/lib.rs` | `ask_user::{AskUserBatchRequest, AskUserOption, AskUserQuestionData}` | 移除或替换 |

### 4. ask_user 模块退役

迁移完成后：
- `peri-agent/src/ask_user/mod.rs` 保留（含 deprecated 类型），等待最终删除
- `peri-middlewares/src/ask_user/mod.rs` 保留但内部的 `parse_ask_user` 已改

## 验证标准

- [ ] `cargo check --workspace` 零 deprecation 警告
- [ ] `cargo test --workspace` 全过
- [ ] AskUser 弹窗功能正常（HITL 审批模式下测试）
- [ ] ask_user 工具 LLM 调用返回结果正确解析
