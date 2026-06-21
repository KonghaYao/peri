# Design: Compact 摘要 `<system-reminder>` 包装 + TUI 折叠

**日期**: 2026-06-02
**对应 Issue**: `spec/issues/2026-06-02-system-reminder-compact-summary.md`

> **注（2026-06-21 更新）**：本文档示例中出现的中文 CONTINUATION_HINT 文本
> （如 `[上下文已压缩，请根据摘要继续工作]`）和中文渲染提示（`📋 上下文已压缩`）
> 已统一迁移为英文。单一事实源为 `peri_agent::agent::compact::CONTINUATION_HINT`
> 共享常量，由 `/compact` 命令、自动 compact、TUI 识别层三方引用。设计原理不变。

## 1. 概述

Compact 后的对话摘要作为 Human 消息注入消息列表，TUI 以完整 UserBubble 渲染，占用大量空间。将其包裹在 `<system-reminder>` 标签内，使其能被 TUI 识别并简化为一行提示。

项目已有 `<system-reminder>` 机制（`recall_items` 用其向 LLM 注入跨轮次状态，`14_system_reminder.md` 指导 LLM 静默处理），本设计复用此标签语义。

## 2. 行为变更

### LLM 侧

- **前**：Compact 后 Human 消息为纯文本摘要（"此会话从之前的对话延续..." + summary + "[上下文已压缩...]"）
- **后**：上述文本包裹在 `<system-reminder>...</system-reminder>` 标签内。LLM 行为不变——已有 `14_system_reminder.md` 提示词指导其静默读取。

### TUI 侧

- **前**：compact 摘要作为完整 UserBubble 渲染（数十行 markdown）
- **后**：检测到 `<system-reminder>` → 显示单行 `📋 上下文已压缩`，不渲染正文。不可展开。

### 不受影响

- re_inject 的 `[最近读取的文件]` / `[激活的 Skill 指令]` System 消息——独立消息，不纳入标签
- Micro compact——不产生摘要文本

## 3. 设计细节

### 3.1 消息构造（`CompactMiddleware`）

**文件**: `peri-middlewares/src/compact_middleware.rs:227-231`

当前代码：

```rust
let summary_content = format!(
    "{}\n\n[上下文已压缩，请根据摘要继续工作]",
    compact_result.summary
);
let mut new_messages = vec![BaseMessage::human(summary_content)];
```

改为在 summary_content 外层包裹标签：

```rust
let summary_content = format!(
    "<system-reminder>\n{}\n\n[上下文已压缩，请根据摘要继续工作]\n</system-reminder>",
    compact_result.summary
);
let mut new_messages = vec![BaseMessage::human(summary_content)];
```

`compact_result.summary` 已包含前缀 `此会话从之前的对话延续。以下是之前对话的摘要。\n\n`（由 `postprocess_summary()` 生成），无需重复添加。

### 3.2 ViewModel 转换（`MessageViewModel`）

**文件**: `peri-tui/src/ui/message_view/mod.rs`

**新增字段**：`UserBubble` 加一个布尔标记：

```rust
UserBubble {
    content: String,
    rendered: Text<'static>,
    content_hash: u64,
    system_reminder: bool,  // 新增
}
```

**转换逻辑**（`from_base_message_with_cwd`）：

```rust
BaseMessage::Human { content, .. } => {
    let raw = content.text_content();
    let (display_text, system_reminder) = if raw.contains("<system-reminder>") {
        let cleaned = raw
            .replace("<system-reminder>\n", "")
            .replace("\n</system-reminder>", "")
            .trim()
            .to_string();
        (cleaned, true)
    } else {
        (raw, false)
    };
    let rendered = parse_markdown_default(&display_text);
    MessageViewModel::UserBubble {
        content: display_text,
        rendered,
        content_hash: 0,
        system_reminder,
    }
}
```

**partialEq**：`system_reminder` 参与比较（语义差异——折叠态 vs 完整内容在 Done 语义比较时不同）。

**Hash**：`system_reminder` 参与 hash（影响渲染输出）。

**不在 Hash/PartialEq 中**：`rendered`（依赖宽度缓存）、无需 `collapsed` 字段（不可切换）。

### 3.3 渲染（`message_render.rs`）

**文件**: `peri-tui/src/ui/message_render.rs`

渲染 `UserBubble` 时判断 `system_reminder`：

- `true` → 渲染一行 `📋 上下文已压缩`（灰色/低权重样式），跳过 markdown 正文
- `false` → 现有逻辑不变

### 3.4 消息格式示例

LLM 收到的 Human 消息：

```
<system-reminder>
此会话从之前的对话延续。以下是之前对话的摘要。

## Summary
- 用户请求实现用户登录功能
- 创建了 auth.rs 模块
- 遇到 CORS 错误，已修复

[上下文已压缩，请根据摘要继续工作]
</system-reminder>
```

TUI 渲染输出：

```
📋 上下文已压缩
```

## 4. 边缘情况

| 场景 | 行为 |
|------|------|
| compact 失败，summary 为空 | 不走标签包裹路径（当前已处理——返回 Err 时 own_messages 放回 state） |
| Human 消息含 `<system-reminder>` 但非 compact 产生 | 同样折叠处理——与 recall_items 机制一致 |
| 标签内无换行 | `replace` 仍匹配，结果正确 |
| `<system-reminder>` 在段中而非开头 | `contains` 匹配，折叠处理 |

## 5. 测试策略

| 层 | 测试点 |
|------|--------|
| `compact_middleware.rs` | 验证 compact 后 Human 消息内容包含 `<system-reminder>` 标签 |
| `message_view/mod.rs` | `from_base_message_with_cwd` 识别标签并设置 `system_reminder=true`；普通 Human 消息不受影响 |
| `message_render.rs` | `system_reminder` 消息渲染为一行提示而非多行 markdown |
| 集成 | 触发 compact → 验证 TUI 状态中的消息携带 `system_reminder=true` |

## 6. 文件变更

| 文件 | 变更 |
|------|------|
| `peri-middlewares/src/compact_middleware.rs` | summary 文本包裹 `<system-reminder>` 标签 |
| `peri-tui/src/ui/message_view/mod.rs` | UserBubble +`system_reminder` 字段；`from_base_message_with_cwd` 检测/剥离标签；更新 PartialEq/Hash |
| `peri-tui/src/ui/message_render.rs` | UserBubble 渲染分支：系统提醒 → 单行提示 |
