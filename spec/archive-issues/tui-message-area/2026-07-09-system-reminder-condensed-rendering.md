# 消息区 system-reminder 内容冗余，需改为缩略两行渲染


> 归档于 2026-07-20，原路径 spec/issues/2026-07-09-system-reminder-condensed-rendering.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-09

## 问题描述

当前 `<system-reminder>` 包裹的内容（compact 摘要、channel 消息、workflow 通知等）在 TUI 消息区以完整文本形式渲染在 user/assistant 气泡中，占用大量消息区空间，干扰用户查看正常对话内容。

期望所有 system-reminder 统一改为缩略两行渲染——第一行类型标签，第二行一行数据摘要（类似 ToolCard 风格），不可展开。

## 症状详情

### 完整 system-reminder 注入点清单

代码搜索确认了以下所有 `<system-reminder>` 注入位置：

| # | 来源 | 注入位置 | MessageKind | 当前渲染 | 典型内容长度 |
|---|------|---------|-------------|---------|------------|
| 1 | compact 全文摘要 | `compact_v2.rs:390,443` | Human append | UserBubble 内完整 markdown | ~500-2000 字符 |
| 2 | compact 文件 re-inject | `compact_v2.rs:849` | Human append | UserBubble 内 "[最近读取的文件: x]" + 文件内容 | ~200-2000 字符 |
| 3 | compact skill re-inject | `compact_v2.rs:887` | Human append | UserBubble 内 "[激活的 Skill 指令: x]" + skill 内容 | ~200-2000 字符 |
| 4 | channel 消息（微信/Slack） | `channel_owner.rs:102` | Defer → Human | UserBubble 内 channel 元信息 + 用户消息混合 | ~50-500 字符 |
| 5 | goal steering 反馈 | `goal_middleware.rs:109` | Info → Human | AssistantBubble 内 markdown | ~100-500 字符 |
| 6 | workflow 完成通知 | `registry.rs:62` | Defer → Human | UserBubble 内多行文本（workflow 名、状态、耗时、agent 数等） | ~200-500 字符 |
| 7 | stop hook 反馈 | `hooks/middleware.rs` | Info → Human | AssistantBubble 内 markdown | ~100-300 字符 |
| 8 | 工具连续失败警告 | `tool_dispatch.rs:553` | Info → Human | AssistantBubble 内文本 | ~50-200 字符 |
| 9 | recall items | `executor.rs:648` | 嵌入下一条 user input | UserBubble 的 ContentBlock 拼接 | ~50-500 字符 |
| 10 | 通用 Info/Defer 管道 | `stages/mod.rs:502` | Info/Defer → Human | 取决于上游来源 | 不定 |

**封装机制**：所有非 Prompt 消息（Info / Defer）统一经过 `append_messages_to_transcript`（`stages/mod.rs:500-504`）包裹 `<system-reminder>` 后写入 transcript。

### 期望行为

所有 system-reminder 统一缩略渲染（不用 emoji，样式可重新设计）：

```
Context compacted              (第一行：类型标签，dim 色)
  ⎿ <一行摘要：首句或 truncated 数据>   (第二行：数据摘要)

Channel (微信)                 (第一行：来源标签)
  ⎿ <用户消息首行 truncated>           (第二行：消息摘要)

Workflow complete (foobar)     (第一行：workflow 名称 + 状态)
  ⎿ 3 agents, 12 tool calls, 4500ms  (第二行：关键数据)

Goal steering                  (第一行：类型标签)
  ⎿ 目标: 完成重构               (第二行：摘要)

Tool failure warning           (第一行：类型标签)
  ⎿ Read failed 5 consecutive times  (第二行：摘要)
```

设计约束：
- 不使用 emoji 装饰
- 两行不可展开
- 整体风格类似 ToolCard（第一行标签 + 第二行 `⎿` 数据），具体样式（颜色、前缀符、间距）可重新鼓捣
- 第一行 dim 色（降低视觉权重），第二行 muted 色

### 现状与参考

- `TuiUserBubble.is_system_reminder` 字段已存在（`tui_render_unit.rs:60`），当前定义了单行 "Context compacted" 渲染路径（`view_render.rs:113-121`），但所有创建点都硬编码为 `false`，此路径是 **dead code**（且是 emoji 风格，需重做）
- `<system-reminder>` 标签在 markdown 渲染层被剥离（`coordinator.rs:402-412`），内部文本以正常 markdown 渲染
- 参考目标：ToolCard 两行渲染（`view_render.rs:207-231`）——第一行 `● ToolName (param)` + 第二行 `⎿ output`

### bg agent 完成通知的冗余详情（2026-07-13 追加）

**问题场景**：bg agent（`/bg`、`bg-fork`、background subagent）完成时，`BackgroundTaskResult::to_notification()`（`peri-agent/src/agent/events.rs:18-31`）把 agent 的**完整输出文本**塞进通知信息：

```rust
// 当前格式（events.rs:21-24）
format!(
    "[后台任务 {} 已完成] Agent: {} | 工具调用: {} | 耗时: {}ms\n结果:\n{}",
    short_id, self.agent_name, self.tool_calls_count, self.duration_ms, self.output,
)
```

`self.output` 是 bg agent 的完整回复文本（可能数千字符），导致 UserBubble 在消息区占据大量空间。

**数据流**（双通道——TUI 和 LLM 走不同路径）：

```
bg agent 完成（to_notification() 生成纯文本，无 <system-reminder> 标签）
    │
    ├─ [LLM 通道] push_defer → append_messages_to_transcript → <system-reminder> 包裹 → LLM 上下文
    │
    └─ [TUI 通道] SyntheticUserMessage → MessageAdded → UserMessageChunk
                   → LocalUserBubble { text } → TuiUserBubble::new(text)
                   → detect_reminder() 返回 None（文本不含 <system-reminder> 标签）
                   → 走普通 UserBubble 渲染 ❌
```

**关键发现**（2026-07-13 代码探索确认）：
1. `to_notification()` 返回的是纯文本 `[后台任务 xxx 已完成] Agent: ...`，**不含** `<system-reminder>` 标签
2. TUI 通过 `SyntheticUserMessage` → `LocalUserBubble` 独立通道接收，与 LLM 的 `<system-reminder>` 包裹路径完全分离
3. `TuiUserBubble::new()` 中的 `detect_reminder()` 无法匹配（无标签），`reminder` 字段为 `None`
4. 因此 `ReminderType::BgTaskCompleted` 和 `render_reminder_condensed()` 都是 **dead code**——它们被定义了但 bg agent 路径不走这里
5. 实际渲染：bg agent 的完整 output 作为普通 UserBubble 完整显示

**期望**：TUI 层缩略显示一句话（如 agent 名 + 耗时），LLM 层保持完整 output 用于上下文。TUI 不需要显示 `output` 全文，因为 bg agent 有独立的 subagent thread 可聚焦查看。

## 涉及文件

**渲染层**：
- `peri-tui/src/kit/view_render.rs` —— `render_user_bubble` 中 `is_system_reminder` 路径，需从单行改为两行 + 类型感知
- `peri-tui/src/kit/tui_render_unit.rs` —— `TuiUserBubble.is_system_reminder` 字段需扩展为 `ReminderStyle` 枚举（类型标签 + 摘要文本）
- `peri-tui/src/kit/acp_events.rs` —— ViewModel 构建点，需在 UserBubble 创建时检测 `<system-reminder>` 内容并提取类型+摘要

**数据识别层**（可能需要）：
- `peri-widgets/src/markdown/render_state/coordinator.rs` —— 当前剥离 `<system-reminder>` 标签后渲染内部文本，可能需要保留标签以便上层识别

**注入点**（了解即可，不修改）：
- `peri-agent/src/agent/compact_v2.rs` —— 3 个注入点
- `peri-agent/src/agent/session/channel_owner.rs` —— 1 个注入点
- `peri-agent/src/agent/stages/mod.rs` —— 通用 Info/Defer 管道
- `peri-agent/src/agent/stages/tool_dispatch.rs` —— 工具失败警告
- `peri-middlewares/src/goal_middleware.rs` —— goal steering
- `peri-middlewares/src/hooks/middleware.rs` —— stop hook 反馈
- `peri-workflow/src/registry.rs` —— workflow 通知
- `peri-acp/src/session/executor.rs` —— recall items

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | konghayao | 创建 |
| 2026-07-13 | Open | Partial | agent | bg agent 缩略渲染已修复；其他 system-reminder 类型待后续处理 |

## 修复记录

### 修复 #1（2026-07-13）bg agent 完成通知缩略渲染

- **操作人**：agent
- **用户原意**：bg agent 返回的 system reminder 文本报告很长，需简化，只缩略显示一句话
- **修复内容**：
  - **根因**：`stages/mod.rs` 中 `SyntheticUserMessage` emit 时未给 text 包裹 `<system-reminder>` 标签。LLM 通道（`append_messages_to_transcript`）有包裹，但 TUI 通道没有，导致 TUI 的 `detect_reminder()` 无法识别
  - **改动**：`peri-agent/src/agent/stages/mod.rs:612,682` 两处 emit 点——在 `SyntheticUserMessage` 的 text 外用 `format!("<system-reminder>\n{}\n</system-reminder>", raw_text)` 包裹，与 LLM 通道一致
  - **效果**：TUI 收到 `<system-reminder>` 包裹的文本 → `detect_reminder()` 自动识别 → `ReminderType::BgTaskCompleted` → `render_reminder_condensed()` 两行缩略渲染
- **涉及文件**：`peri-agent/src/agent/stages/mod.rs`
- **验证状态**：已验证（`cargo test -p peri-agent --lib -- stages` 53/53 pass，`cargo test -p peri-tui --lib` 437/437 pass）
