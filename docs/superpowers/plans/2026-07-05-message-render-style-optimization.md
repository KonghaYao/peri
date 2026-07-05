# 消息渲染样式优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 v2 kit 路径的消息渲染代码对齐 TUI-PAGE.md §2.4 的渲染样式规格，修复 6 项 P0 正确性问题 + 8 项 P1 完整性缺失 + 8 项 P2 性能/细节偏差。

**Architecture:** 主要修改集中在 `peri-tui/src/kit/view_render.rs`（渲染逻辑）和 `peri-tui/src/kit/theme.rs`（颜色系统），少量触及 `view_model.rs`（DTO 字段）、`view_mapper.rs`（ACP 映射）、`message_area.rs`（布局）、`text_selection.rs`（选区色）。所有改动严格遵从 ACP-only Data Flow 原则，不引入新的数据通道。`bg_hash` 和 `batch_agents` 因 AgentEvent 层缺少数据源头，不在本 plan 范围内（见 [Issue: SubAgent 批量数据通道](#后续议题)）。

**Tech Stack:** Rust 2021 + ratatui + ratatui-kit

**文件总览：**

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `peri-tui/src/kit/view_render.rs` | 7 种 ViewModel → `Vec<Line>` 转换 | 重写/扩展 |
| `peri-tui/src/kit/theme.rs` | ThemeDefinition + Semantic/Component Tokens | 扩展 8 个 token |
| `peri-tui/src/kit/message_area.rs` | 消息区布局 + ScrollView + Spinner + Todo | 扩展 |
| `peri-tui/src/kit/text_selection.rs` | 鼠标文本选区高亮 | 修复 |
| `peri-tui/src/kit/render_bridge.rs` | MessagePipeline ↔ ViewModel 桥接 | 修复 |
| `peri-acp-types/src/view_model.rs` | ViewModel DTO 类型定义 | 扩展字段 |
| `peri-acp/src/event/view_mapper.rs` | ACP 事件 → ViewModel 映射 | 扩展解析逻辑 |
| `peri-widgets/src/spinner/` | Spinner 动画组件 | 增加紧凑态 |

---

## Phase 1: 渲染正确性修复（P0）

### Task 1.1: 移除 AssistantBubble 多余 `● ` 前缀

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:128-136`

- [ ] **Step 1: 读取当前代码确认修改范围**

读取 `view_render.rs` 中 `render_assistant_bubble` 函数（约 125-195 行），确认当前 `● ` 前缀添加逻辑。

- [ ] **Step 2: 移除 assistant 消息的 `● ` 前缀**

```rust
// 修改前 (line ~130-137):
fn render_assistant_bubble(bubble: &AssistantBubbleData, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let comp = theme.component();
    let mut lines = Vec::new();

    let prefix = Span::styled("● ", Style::default().fg(sem.text).add_modifier(Modifier::BOLD));
    for (i, block) in bubble.blocks.iter().enumerate() {
        // ...
    }
}

// 修改后:
fn render_assistant_bubble(bubble: &AssistantBubbleData, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let comp = theme.component();
    let mut lines = Vec::new();

    // AI 回复无前缀符号，直接渲染 markdown 块
    for (i, block) in bubble.blocks.iter().enumerate() {
        // 移除 prefix 注入，lines 直接收集渲染后的 Line
    }
}
```

- [ ] **Step 3: 运行构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```
期望：编译通过，无 warning。

- [ ] **Step 4: 运行现有测试确认无回归**

```bash
cargo test -p peri-tui --lib -- view_render
```
期望：全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): remove extraneous ● prefix from AssistantBubble rendering

AssistantBubble had a ● prefix that conflicts with the symbol hierarchy
defined in TUI-PAGE.md §2.4.6. Spec (§2.4.2) requires no prefix for AI
responses - ● is reserved for ToolBlock/ToolCallGroup only."
```

---

### Task 1.2: 修复 SubAgentGroup 前缀 `◆` → `❯` + loading 色

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:425-436`

- [ ] **Step 1: 定位 SubAgent header 行渲染逻辑**

读取 `render_subagent_group` 函数 header 部分（约 420-445 行）。

- [ ] **Step 2: 替换前缀符号和颜色**

```rust
// 修改前 (line ~425-436):
fn render_subagent_group(group: &SubAgentGroupData, theme: &ThemeDefinition, ...) -> Vec<Line> {
    let sem = theme.semantic();
    let comp = theme.component();

    // Header line: prefix + agent name
    let header = Span::styled(
        format!("◆ Agent ({})", group.agent_name),
        Style::default().fg(comp.message.ai_prefix),  // TEXT 白 - 错误
    );
    // ...
}

// 修改后:
fn render_subagent_group(group: &SubAgentGroupData, theme: &ThemeDefinition, ...) -> Vec<Line> {
    let sem = theme.semantic();
    let comp = theme.component();

    // Header line: ❯ Agent(agent_id)
    let prefix = Span::styled(
        "❯ ",
        Style::default().fg(sem.loading),  // #93A5FF 蓝紫
    );
    let label = Span::styled(
        "Agent",
        Style::default().fg(sem.success),  // 正常绿色
    );
    let id = Span::styled(
        format!("({})", group.agent_id),
        Style::default().fg(sem.muted),
    );
    // 错误态：label 颜色切换为 error
    let label_color = if group.is_error { sem.error } else { sem.success };
    // ...
}
```

- [ ] **Step 3: 运行构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 4: 运行测试**

```bash
cargo test -p peri-tui --lib -- subagent
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): correct SubAgentGroup prefix ◆→❯ with loading color

Aligns with TUI-PAGE.md §2.4.2: ❯ prefix in loading (#93A5FF) color,
agent label color toggles between success/error based on is_error field."
```

---

### Task 1.3: UserBubble system_reminder 特殊渲染

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:108-136`
- Modify: `peri-acp-types/src/view_model.rs`（如需要 `is_system_reminder` 字段）

- [ ] **Step 1: 检查 UserBubbleData 是否有 is_system_reminder 标记**

读取 `view_model.rs` 中 `UserBubbleData` 定义。

```bash
grep -n "UserBubbleData" peri-acp-types/src/view_model.rs
```

- [ ] **Step 2: 若无字段，添加 `is_system_reminder` 到 UserBubbleData**

```rust
// view_model.rs - UserBubbleData 结构体中添加字段:
pub struct UserBubbleData {
    pub content: String,
    pub content_hash: u64,
    #[serde(default)]
    pub is_system_reminder: bool,  // 新增
}
```

- [ ] **Step 3: 修改 render_user_bubble 分支逻辑**

```rust
// view_render.rs render_user_bubble 函数开头:
fn render_user_bubble(bubble: &UserBubbleData, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();

    // system_reminder: 特殊渲染，仅显示一行 dim+italic
    if bubble.is_system_reminder {
        return vec![Line::from(Span::styled(
            "📋 Context compacted",
            Style::default().fg(sem.dim).add_modifier(Modifier::ITALIC),
        ))];
    }

    // 正常用户消息：❯ 前缀 + user_bg 底色
    let prefix = Span::styled("❯ ", Style::default().fg(sem.accent).add_modifier(Modifier::BOLD));
    // ... 原有逻辑
}
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui -p peri-acp-types 2>&1 | tail -20
cargo test -p peri-tui --lib -- user_bubble
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs peri-acp-types/src/view_model.rs
git commit -m "fix(tui): handle UserBubble is_system_reminder with dim+italic style

Spec §2.4.2: system_reminder messages render as '📋 Context compacted'
in dim color with italic, no ❯ prefix, no user_bg background."
```

---

### Task 1.4: 修复 ToolBlock 指示器符号（emoji → Unicode）

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:283-295`

- [ ] **Step 1: 定位 tool_display / render_tool_card 中的指示器逻辑**

读取 `render_tool_card` 函数（约 200-330 行）。

- [ ] **Step 2: 替换 emoji 为 spec Unicode 符号**

```rust
// 修改前 (line ~283-295):
fn render_tool_card(card: &ToolCardData, theme: &ThemeDefinition, ...) -> Vec<Line> {
    let status_prefix = match card.status {
        ToolStatus::Running => "⏳",   // emoji - 错误
        ToolStatus::Completed => "",
        ToolStatus::Failed => "❌",    // emoji - 错误
    };
    // ...
}

// 修改后:
fn render_tool_card(card: &ToolCardData, theme: &ThemeDefinition, tick: u64) -> Vec<Line> {
    let sem = theme.semantic();

    let (prefix, color) = match card.status {
        ToolStatus::Running => {
            // 800ms 切换 ● ↔ 空格，1600ms 周期
            let visible = ((tick / 4) % 2) == 0;
            (if visible { "●" } else { " " }, sem.success)
        }
        ToolStatus::Completed => ("●", sem.success),
        ToolStatus::Failed => ("✗", sem.error),
    };

    let indicator = Span::styled(prefix, Style::default().fg(color));
    let name = Span::styled(
        format!(" {}", format_tool_name(&card.tool_name)),
        Style::default().fg(sem.text).add_modifier(Modifier::BOLD),
    );
    // ...
}
```

- [ ] **Step 3: 确认 tick 参数传入路径**

检查 `render_v2_vm` 函数签名，确认是否需要添加 `tick` 参数。如需要，从 `render_bridge` 或 `message_area` 传入当前帧号。

```rust
// render_v2_vm 函数签名可能需要改为:
pub fn render_v2_vm(vm: &MessageViewModel, theme: &ThemeDefinition, tick: u64) -> Vec<Line<'static>>
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
cargo test -p peri-tui --lib -- tool_card
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): replace tool status emoji with spec Unicode indicators

● ✗ instead of ⏳ ❌. Running indicator uses 800ms pulse per §2.4.2."
```

---

### Task 1.5: 修复 text_selection 硬编码颜色 → Theme Token

**Files:**
- Modify: `peri-tui/src/kit/text_selection.rs:172`
- Reference: `peri-tui/src/kit/theme.rs:391`

- [ ] **Step 1: 定位硬编码处**

```rust
// text_selection.rs:172 (约):
const SELECTION_BG: Color = Color::Rgb(60, 60, 60);  // 硬编码 - 错误
```

- [ ] **Step 2: 修改为从 ThemeDefinition 参数获取**

```rust
// 修改后:
use crate::kit::theme::ThemeDefinition;

// 移除 const 定义，改为接受 theme 参数
pub fn apply_selection_highlight<'a>(
    line: Line<'a>,
    selection: &TextSelection,
    theme: &ThemeDefinition,
) -> Line<'a> {
    let selection_bg = theme.surface().selection;  // #264F78
    // ... 高亮逻辑使用 selection_bg
}
```

- [ ] **Step 3: 更新所有调用点**

搜索 `apply_selection_highlight` 或 `SELECTION_BG` 的引用，确保传入 theme 参数。

```bash
grep -rn "SELECTION_BG\|apply_selection_highlight" peri-tui/src/kit/
```

- [ ] **Step 4: 确认 ThemeDefinition surface.selection 字段存在**

读取 `theme.rs` 中 `SurfaceTokens` 结构体，确认有 `selection` 字段。

- [ ] **Step 5: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
cargo test -p peri-tui --lib -- selection
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/text_selection.rs
git commit -m "fix(tui): use ThemeDefinition.selection_bg instead of hardcoded gray

Replaces hardcoded Rgb(60,60,60) with surface.selection token (#264F78)
per TUI-PAGE.md §2.4.4 selection highlight spec."
```

---

### Task 1.6: Diff 渲染添加行级背景色

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:357-398`
- Modify: `peri-tui/src/kit/theme.rs:62-66` + `theme.rs:108-113`（添加 diff_add_bg / diff_remove_bg token）

- [ ] **Step 1: 在 ThemeDefinition 中添加 Diff 背景色 Token**

```rust
// theme.rs SemanticTokens 中添加:
pub struct SemanticTokens {
    // ... 现有字段 ...
    pub diff_add: Color,
    pub diff_remove: Color,
    pub diff_hunk: Color,
    pub diff_add_bg: Color,       // 新增: #12341A
    pub diff_remove_bg: Color,    // 新增: #371412
    pub diff_add_word_bg: Color,  // 新增: #1A4E24
    pub diff_remove_word_bg: Color, // 新增: #4E1C16
}
```

- [ ] **Step 2: 在 DEFAULT_THEME 中赋值**

```rust
diff_add: Color::Rgb(63, 185, 80),
diff_remove: Color::Rgb(248, 81, 73),
diff_hunk: Color::Rgb(87, 143, 169),
diff_add_bg: Color::Rgb(18, 52, 26),
diff_remove_bg: Color::Rgb(55, 20, 18),
diff_add_word_bg: Color::Rgb(26, 78, 36),
diff_remove_word_bg: Color::Rgb(78, 28, 22),
```

- [ ] **Step 3: 修改 render_diff_block 应用背景色**

```rust
// view_render.rs render_diff_block 中:
fn render_diff_block(block: &DiffBlockData, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();

    for line in &block.lines {
        let rendered = match line.line_type {
            DiffLineType::Add => {
                Line::styled(line.content.clone(),
                    Style::default()
                        .fg(sem.diff_add)
                        .bg(sem.diff_add_bg),  // 新增背景色
                )
            }
            DiffLineType::Remove => {
                Line::styled(line.content.clone(),
                    Style::default()
                        .fg(sem.diff_remove)
                        .bg(sem.diff_remove_bg),  // 新增背景色
                )
            }
            // ...
        };
    }
}
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
cargo test -p peri-tui --lib -- diff
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs peri-tui/src/kit/theme.rs
git commit -m "feat(tui): add diff line-level background colors per §2.4.3

Add tokens: diff_add_bg (#12341A), diff_remove_bg (#371412),
diff_add_word_bg, diff_remove_word_bg. Apply bg to add/remove lines."
```

---

## Phase 2: 渲染完整性补充（P1）

### Task 2.1: 实现 format_tool_name 工具名映射

**Files:**
- Create: `peri-tui/src/kit/tool_display.rs`（或附加到现有文件）

- [ ] **Step 1: 创建工具显示名映射函数**

```rust
// peri-tui/src/kit/tool_display.rs (新文件)

/// 将原始 tool_name 映射为用户友好的显示名
pub fn format_tool_name(raw: &str) -> &str {
    match raw {
        "Bash" => "Shell",
        "Read" => "Read",
        "Write" => "Write",
        "Edit" => "Edit",
        "Glob" => "Glob",
        "Grep" => "Grep",
        "folder_operations" => "Folder",
        "TodoWrite" => "Todo",
        "AskUserQuestion" => "Ask",
        "Agent" => "Agent",
        "WebSearch" => "Research",
        "WebFetch" => "Browse",
        "AgentResult" => "SubAgent",
        "LSP" => "LSP",
        "artifact" => "ArtUp",
        other => {
            // PascalCase 转换回退: 其他工具名保持不变
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tool_name_bash_maps_to_shell() {
        assert_eq!(format_tool_name("Bash"), "Shell");
    }

    #[test]
    fn test_format_tool_name_websearch_maps_to_research() {
        assert_eq!(format_tool_name("WebSearch"), "Research");
    }

    #[test]
    fn test_format_tool_name_unknown_passthrough() {
        assert_eq!(format_tool_name("CustomTool"), "CustomTool");
    }
}
```

- [ ] **Step 2: 在 kit/mod.rs 中注册模块**

```rust
// peri-tui/src/kit/mod.rs:
pub mod tool_display;
```

- [ ] **Step 3: 在 view_render.rs 的 tool card 渲染中调用**

将 `view_render.rs:300` 左右的 `card.tool_name` 替换为 `format_tool_name(&card.tool_name)`。

- [ ] **Step 4: 运行测试**

```bash
cargo test -p peri-tui --lib -- tool_display
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/tool_display.rs peri-tui/src/kit/mod.rs peri-tui/src/kit/view_render.rs
git commit -m "feat(tui): add tool display name mapping (format_tool_name)

Maps raw tool names to friendlier display names per TUI-PAGE.md §2.4.2:
Bash→Shell, WebSearch→Research, folder_operations→Folder, etc."
```

---

### Task 2.2: 实现 format_tool_args 工具参数摘要

**Files:**
- Modify: `peri-tui/src/kit/tool_display.rs`（追加内容）

- [ ] **Step 1: 添加参数摘要格式化函数**

```rust
// 追加到 peri-tui/src/kit/tool_display.rs

/// 从工具参数 JSON 中提取摘要
pub fn format_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    let truncate = |s: &str, max: usize| -> String {
        if s.chars().count() > max {
            format!("{}...", s.chars().take(max).collect::<String>())
        } else {
            s.to_string()
        }
    };

    match tool_name {
        "Bash" => {
            args.get("command")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 400))
                .unwrap_or_default()
        }
        "Read" | "Write" | "Edit" => {
            args.get("file_path")
                .and_then(|v| v.as_str())
                .map(|s| relatify_path(s))
                .unwrap_or_default()
        }
        "Glob" | "Grep" => {
            args.get("pattern")
                .and_then(|v| v.as_str())
                .map(|s| truncate(&relatify_pattern(s), 200))
                .unwrap_or_default()
        }
        "folder_operations" => {
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("folder_path").and_then(|v| v.as_str()).unwrap_or("");
            format!("{} {}", op, relatify_path(path))
        }
        "WebSearch" | "WebFetch" => {
            let key = if tool_name == "WebSearch" { "query" } else { "url" };
            args.get(key)
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 60))
                .unwrap_or_default()
        }
        "ExecuteExtraTool" | "SearchExtraTools" => {
            let key = if tool_name == "ExecuteExtraTool" { "tool_name" } else { "query" };
            args.get(key)
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 40))
                .unwrap_or_default()
        }
        "AgentResult" => {
            args.get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 12))
                .unwrap_or_default()
        }
        "artifact" => {
            args.get("file_path")
                .and_then(|v| v.as_str())
                .map(|s| relatify_path(s))
                .unwrap_or_default()
        }
        "LSP" => {
            args.get("operation")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 40))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn relatify_path(path: &str) -> String {
    // 尝试相对于 cwd 简化路径
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(cwd.to_string_lossy().as_ref()) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    path.to_string()
}

fn relatify_pattern(pattern: &str) -> String {
    pattern.to_string() // glob pattern 保持原样
}
```

- [ ] **Step 2: 在 view_render.rs 中集成**

在 `render_tool_card` 函数的参数摘要生成处调用 `format_tool_args`。

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-tui --lib -- tool_display
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/tool_display.rs peri-tui/src/kit/view_render.rs
git commit -m "feat(tui): add tool argument summary formatting per §2.4.2

Extracts relevant fields from args JSON: command for Bash,
file_path for Read/Write/Edit, pattern for Glob/Grep, etc."
```

---

### Task 2.3: 实现工具折叠/展开逻辑

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:200-271` (render_tool_card)

> 注：`collapsed` 是纯 UI 渲染控制，不储存在 ViewModel DTO 中。折叠/展开由 `view_render.rs` 根据 `tool_name` 匹配 `COLLAPSED_BY_DEFAULT` / `AUTO_EXPAND` / `FORCE_EXPAND` 列表决策。

- [ ] **Step 1: 定义默认折叠/展开工具列表**

```rust
// view_render.rs:
const COLLAPSED_BY_DEFAULT: &[&str] = &["Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];
```

- [ ] **Step 2: 修改 render_tool_card 实现折叠逻辑**

```rust
fn render_tool_card(card: &ToolCardData, theme: &ThemeDefinition, tick: u64) -> Vec<Line<'static>> {
    let sem = theme.semantic();

    // 确定有效折叠态（纯 UI 决策，不依赖 ViewModel 字段）
    let effective_collapsed = if card.is_error {
        true // 错误不自动展开
    } else if AUTO_EXPAND.contains(&card.tool_name.as_str()) {
        false // AgentResult/ExecuteExtraTool 自动展开
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&card.tool_name.as_str()) && !matches!(card.status, ToolStatus::Running) {
        false // Write/Edit 完成后强制展开
    } else {
        COLLAPSED_BY_DEFAULT.contains(&card.tool_name.as_str()) // Read/Glob/Grep/AskUserQuestion 默认折叠
    };

    if effective_collapsed {
        return render_collapsed_header(card, theme, tick);
    } else {
        return render_expanded_card(card, theme, tick);
    }
}
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui -p peri-acp-types 2>&1 | tail -20
cargo test -p peri-tui --lib -- tool_card
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/view_render.rs peri-acp-types/src/view_model.rs
git commit -m "feat(tui): implement tool collapse/expand logic per §2.4.2

Read/Glob/Grep/AskUserQuestion default collapsed.
AgentResult/ExecuteExtraTool auto-expand. Write/Edit force expand on complete."
```

---

### Task 2.4: 修复 CollapsedGroup emoji → `● ` 前缀

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:541-546`

- [ ] **Step 1: 替换 📦 emoji**

```rust
// 修改前:
let prefix = Span::styled("📦 ", Style::default().fg(sem.text));

// 修改后:
let prefix = Span::styled("● ", Style::default().fg(sem.success));
```

- [ ] **Step 2: 构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): replace 📦 emoji with ● prefix in CollapsedGroup

Aligns with TUI-PAGE.md §2.4.6 prefix hierarchy: ● for aggregation groups."
```

---

### Task 2.5: 修复 SystemNote 多行前缀分类

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:401-412`

- [ ] **Step 1: 重写 render_system_note 支持前缀分类**

```rust
fn render_system_note(note: &SystemNoteData, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let mut lines = Vec::new();

    for line_text in note.content.lines() {
        if line_text.starts_with('✻') {
            // ✻ 开头行 — dim 色，无额外前缀
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(sem.dim),
            )));
        } else if line_text.starts_with('⎿') {
            // ⎿ 开头行 — muted 色，无额外前缀
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(sem.muted),
            )));
        } else if line_text.starts_with("  ⎿") {
            // 已缩进的 ⎿ — 保留原样，error 色（错误摘要行）
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(sem.error),
            )));
        } else {
            // 其余行 — · 前缀 + 自动检测错误/警告
            let (prefix_color, content_color) = if line_text.contains("❌")
                || line_text.contains("失败")
                || line_text.to_lowercase().contains("error")
            {
                (sem.dim, sem.error)
            } else if line_text.contains('⚠') || line_text.contains("已中断") {
                (sem.dim, sem.warning)
            } else {
                (sem.dim, sem.muted)
            };

            let prefix = Span::styled("· ", Style::default().fg(prefix_color));
            let content = Span::styled(line_text.to_string(), Style::default().fg(content_color));
            lines.push(Line::from(vec![prefix, content]));
        }
    }

    lines
}
```

- [ ] **Step 2: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
cargo test -p peri-tui --lib -- system_note
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): classify SystemNote lines by prefix (✻/⎿/·) per §2.4.2

✻→dim, ⎿→muted, others→· prefix with auto error/warning detection."
```

---

### Task 2.6: 添加 AskUserBlock 渲染

**Files:**
- Modify: `peri-acp-types/src/view_model.rs` (添加 AskUserBlock 变体)
- Modify: `peri-acp/src/event/view_mapper.rs` (解析 AskUserQuestion 工具结果 → AskUserBlock)
- Modify: `peri-tui/src/kit/view_render.rs` (添加 render_ask_user_block)

- [ ] **Step 1: 在 ViewModel 枚举中添加 AskUserBlock 变体**

```rust
// view_model.rs MessageViewModel:
pub enum MessageViewModel {
    // ... 现有变体 ...
    AskUserBlock {
        items: Vec<AskUserItem>,
        is_error: bool,
        content_hash: u64,
    },
}

pub struct AskUserItem {
    pub header: String,
    pub answer: String,
}
```

- [ ] **Step 2: 实现 render_ask_user_block**

```rust
// view_render.rs:
fn render_ask_user_block(items: &[AskUserItem], is_error: bool, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let mut lines = Vec::new();

    let title_color = if is_error { sem.error } else { sem.success };
    lines.push(Line::from(Span::styled(
        "● User answered Peri's questions:",
        Style::default().fg(title_color),
    )));

    for item in items {
        let prefix = Span::styled("  ⎿ ", Style::default().fg(sem.dim));
        let content = Span::styled(
            format!("{} → {}", item.header, item.answer),
            Style::default().fg(if is_error { sem.error } else { sem.muted }),
        );
        lines.push(Line::from(vec![prefix, content]));
    }

    lines
}
```

- [ ] **Step 3: 在 render_v2_vm 中添加分发分支**

```rust
// render_v2_vm match 中添加:
MessageViewModel::AskUserBlock { items, is_error, content_hash: _ } => {
    render_ask_user_block(items, *is_error, theme)
}
```

- [ ] **Step 4: 在 view_mapper.rs 中添加 AskUserQuestion → AskUserBlock 映射**

```rust
// view_mapper.rs convert_tool() 函数中，在 Agent 特判之后添加:
if tool_name == "AskUserQuestion" {
    // 解析 tool input 提取 questions header
    let items: Vec<AskUserItem> = /* 从 raw_input JSON 解析 questions 数组 */;
    // 解析 tool output 提取 answers
    // ... 将 question header 与 answer 文本配对
    return MessageViewModel::AskUserBlock {
        items,
        is_error: false,
        content_hash: hash(&raw_content),
    };
}
```

- [ ] **Step 5: 构建和测试**

```bash
cargo build -p peri-tui -p peri-acp-types -p peri-acp 2>&1 | tail -20
cargo test -p peri-tui --lib -- ask_user
```

- [ ] **Step 6: Commit**

```bash
git add peri-acp-types/src/view_model.rs peri-acp/src/event/view_mapper.rs peri-tui/src/kit/view_render.rs
git commit -m "feat(tui): add AskUserBlock ViewModel + view_mapper + render per §2.4.2

● User answered Peri's questions: header + ⎿ header→answer items.
view_mapper parses AskUserQuestion tool result into AskUserBlock."
```

---

### Task 2.7: 添加 Diff 特殊文件处理

**Files:**
- Modify: `peri-acp-types/src/view_model.rs` (DiffBlock 添加元数据字段)
- Modify: `peri-acp/src/event/view_mapper.rs` (build_diff_block 提取 is_binary/is_too_large/is_new_file)
- Modify: `peri-tui/src/kit/view_render.rs:357-398`

- [ ] **Step 1: 在 DiffBlock 中添加元数据字段**

```rust
// view_model.rs:
pub struct DiffBlock {
    pub file_path: String,
    pub hunks: Vec<DiffHunk>,
    #[serde(default)]
    pub is_binary: bool,       // 二进制文件
    #[serde(default)]
    pub is_too_large: bool,    // 超长 diff
    #[serde(default)]
    pub is_new_file: bool,     // 新文件（限制 6 行）
}
```

- [ ] **Step 2: 在 view_mapper.rs 的 build_diff_block 中提取元数据**

```rust
// view_mapper.rs build_diff_block():
let is_new_file = name == "Write" || (name == "Edit" && old_string.is_empty());
let is_binary = raw_content.contains("Binary");
let is_too_large = raw_content.contains("too large");

DiffBlock {
    file_path,
    hunks,
    is_binary,
    is_too_large,
    is_new_file,
}
```

- [ ] **Step 3: 扩展 render_diff_block 支持特殊文件消息**

```rust
fn render_diff_block(block: &DiffBlock, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();

    // 特殊文件处理
    if block.is_binary {
        return vec![Line::from(Span::styled(
            format!("  Binary {} - cannot display diff", block.file_path),
            Style::default().fg(sem.dim),
        ))];
    }

    if block.is_too_large {
        return vec![Line::from(Span::styled(
            format!("  Diff too large for {} - changes not displayed", block.file_path),
            Style::default().fg(sem.dim),
        ))];
    }

    let mut lines = Vec::new();
    let max_context = if block.is_new_file { 6 } else { usize::MAX };

    for (i, line) in block.lines.iter().enumerate() {
        if block.is_new_file && i >= max_context {
            lines.push(Line::from(Span::styled(
                format!("... {} more lines not shown", block.lines.len() - max_context),
                Style::default().fg(sem.dim),
            )));
            break;
        }

        let rendered = match line.line_type {
            DiffLineType::Add => {
                Line::styled(line.content.clone(),
                    Style::default().fg(sem.diff_add).bg(sem.diff_add_bg))
            }
            DiffLineType::Remove => {
                Line::styled(line.content.clone(),
                    Style::default().fg(sem.diff_remove).bg(sem.diff_remove_bg))
            }
            DiffLineType::Context => {
                Line::styled(line.content.clone(), Style::default())
            }
            // ... hunks, headers
        };
        lines.push(rendered);
    }

    lines
}
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui -p peri-acp-types -p peri-acp 2>&1 | tail -20
cargo test -p peri-tui --lib -- diff
```

- [ ] **Step 5: Commit**

```bash
git add peri-acp-types/src/view_model.rs peri-acp/src/event/view_mapper.rs peri-tui/src/kit/view_render.rs
git commit -m "feat(tui): add diff special file handling per §2.4.3

DiffBlock gains is_binary/is_too_large/is_new_file fields.
view_mapper extracts metadata from tool input/output.
Renderer handles binary/oversized/new file cases."
```

---

## Phase 3: 性能与细节优化（P2）

### Task 3.1: 添加 Theme 缺失 Token

**Files:**
- Modify: `peri-tui/src/kit/theme.rs`

- [ ] **Step 1: 在 SemanticTokens 中添加新字段**

```rust
// theme.rs SemanticTokens:
pub struct SemanticTokens {
    // ... 现有字段 ...
    pub model_info: Color,     // #A0825F 棕金
    pub bash_border: Color,    // #FD5DB1 粉红
    pub selected_fg: Color,    // #B2B9F9 浅紫
    pub subagent_bg: Color,    // #1E1E26 暗蓝
}

// SurfaceTokens:
pub struct SurfaceTokens {
    // ... 现有字段 ...
    pub subagent: Color,       // #1E1E26
}
```

- [ ] **Step 2: 在 DEFAULT_THEME 中赋值**

```rust
model_info: Color::Rgb(160, 130, 95),
bash_border: Color::Rgb(253, 93, 177),
selected_fg: Color::Rgb(178, 185, 249),
subagent_bg: Color::Rgb(30, 30, 38),
```

- [ ] **Step 3: 构建验证**

```bash
cargo check -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/theme.rs
git commit -m "feat(theme): add missing tokens per §2.4.1

model_info (#A0825F), bash_border (#FD5DB1), selected_fg (#B2B9F9),
subagent_bg (#1E1E26) for SubAgent nested messages."
```

---

### Task 3.2: 修复 SubAgent final_result 前缀 `→` → `⎿`

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs:529-530`

- [ ] **Step 1: 替换前缀符号**

```rust
// 修改前:
let prefix = Span::styled("→ ", Style::default());

// 修改后:
let prefix = Span::styled("  ⎿ ", Style::default().fg(sem.dim));
```

- [ ] **Step 2: 构建和提交**

```bash
cargo build -p peri-tui 2>&1 | tail -20
git add peri-tui/src/kit/view_render.rs
git commit -m "fix(tui): replace SubAgent final_result → prefix with ⎿ per §2.4.2"
```

---

### Task 3.3: 添加渲染桥接 resize 去抖

**Files:**
- Modify: `peri-tui/src/kit/render_bridge.rs:51-55`

- [ ] **Step 1: 添加 last_resize_width 字段**

```rust
// render_bridge.rs 相关结构体:
pub struct RenderBridge {
    // ... 现有字段 ...
    last_resize_width: u16,
}
```

- [ ] **Step 2: 在 resize 处理中添加去抖**

```rust
pub fn handle_resize(&mut self, new_width: u16) {
    if new_width == self.last_resize_width {
        return; // 去抖：忽略同宽度的重复事件
    }
    self.last_resize_width = new_width;

    // 执行全量重建
    self.rebuild_all();
}
```

- [ ] **Step 3: 构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/render_bridge.rs
git commit -m "perf(tui): add resize debounce in render bridge per §2.4.4

Skip duplicate same-width resize events to avoid N/sec re-renders."
```

---

### Task 3.4: 添加 Sticky Header 渲染

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

- [ ] **Step 1: 实现 sticky header 渲染函数**

```rust
// message_area.rs:
fn render_sticky_header(area: Rect, last_user_message: Option<&str>, theme: &ThemeDefinition) -> Option<Vec<Line<'static>>> {
    let sem = theme.semantic();

    last_user_message.map(|msg| {
        let prefix = Span::styled("❯ ", Style::default().fg(sem.accent).add_modifier(Modifier::BOLD));
        let text = Span::styled(msg, Style::default().fg(sem.text).bg(sem.user_bg));

        // 根据区域宽度 wrap 文本
        let max_width = area.width.saturating_sub(2) as usize;
        let line = Line::from(vec![prefix, text]);

        // 简化实现：单行截断
        if msg.chars().count() > max_width as usize {
            vec![line]
        } else {
            vec![line]
        }
    })
}
```

- [ ] **Step 2: 在 message_area 渲染主函数中集成**

在 `MessageArea` 组件渲染开头，当 `max_scroll > 0` 时调用 `render_sticky_header`。

- [ ] **Step 3: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): add Sticky Header showing last user message per §2.4.4

Renders ❯ + user message at top of message area when scrollable.
Accepts theme for accent/bold styling."
```

---

### Task 3.5: 集成 SpinnerWidget 替换手写 Spinner

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:118-121`
- Modify: `peri-widgets/src/spinner/mod.rs`（添加紧凑态）

- [ ] **Step 1: 在 SpinnerWidget 中添加 compact 模式支持**

```rust
// spinner/mod.rs SpinnerWidget 添加:
impl SpinnerWidget {
    pub fn with_compact(mut self, compact: bool) -> Self {
        self.compact_mode = compact;
        self
    }
}

// SpinnerState 中添加:
pub struct SpinnerState {
    // ... 现有字段 ...
    pub compact_mode: bool,
}

// 渲染时 compact_mode 影响颜色选择:
fn render_frame(&self, theme: &dyn Theme) -> Vec<Span> {
    let accent = if self.compact_mode {
        theme.thinking()  // 紧凑态用 thinking 色
    } else {
        theme.accent()
    };
    // ...
}
```

- [ ] **Step 2: 在 message_area 中用 SpinnerWidget 替换手写 spinner**

```rust
// 修改前:
let spinner = Line::from(Span::styled("◜ 思考中…", Style::default().fg(theme.semantic().accent)));

// 修改后:
let spinner_state = SpinnerState::new().with_mode(SpinnerMode::Thinking);
let spinner_widget = SpinnerWidget::new(&spinner_state)
    .with_theme(&theme_adapter)
    .with_compact(is_compact);

// 渲染为 Vec<Line> 后插入消息流底部
let spinner_lines = spinner_widget.render_to_lines(area.width);
```

- [ ] **Step 3: 构建和测试**

```bash
cargo build -p peri-tui -p peri-widgets 2>&1 | tail -20
cargo test -p peri-widgets --lib -- spinner
```

- [ ] **Step 4: Commit**

```bash
git add peri-widgets/src/spinner/mod.rs peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): use SpinnerWidget with compact mode per §2.4.7

Replaces hand-coded spinner line with SpinnerWidget.
Compact mode switches color from accent→thinking."
```

---

### Task 3.6: 添加 ▲/▼ 滚动按钮

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

- [ ] **Step 1: 在消息区右侧渲染 ▲/▼ 按钮**

```rust
// message_area.rs 滚动条渲染逻辑中:
fn render_scroll_buttons(area: Rect, offset: usize, max_scroll: usize, theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let style = Style::default().fg(sem.muted).add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();

    if offset > 0 {
        lines.push(Line::from(Span::styled("▲", style)));
    }
    if offset < max_scroll {
        lines.push(Line::from(Span::styled("▼", style)));
    }

    lines
}
```

- [ ] **Step 2: 集成到消息区渲染**

在 `MessageArea` 渲染函数的右侧列中调用 `render_scroll_buttons`，放在滚动条体渲染之前。

- [ ] **Step 3: 构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): add ▲/▼ scroll buttons to message area per §2.4.4

▲ shown when offset > 0, ▼ when offset < max_scroll.
Both muted + BOLD style."
```

---

### Task 3.7: 添加 Todo 列表渲染

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`
- Reference: `peri-tui/src/kit/view_render.rs`（如需要专用渲染函数）

- [ ] **Step 1: 在 message_area 中实现 render_todo_list**

```rust
fn render_todo_list(items: &[TodoItem], theme: &ThemeDefinition) -> Vec<Line<'static>> {
    let sem = theme.semantic();
    let mut lines = Vec::new();

    let style = |icon: &str, icon_color: Color, text_color: Color, crossed: bool| {
        let prefix = Span::styled(format!("  {}  ", icon), Style::default().fg(icon_color).add_modifier(Modifier::BOLD));
        let mut text_style = Style::default().fg(text_color);
        if crossed {
            text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
        }
        (prefix, text_style)
    };

    for item in items {
        let (prefix, text_style) = match item.status {
            TodoStatus::InProgress => style("◼", sem.accent, sem.text, false),
            TodoStatus::Completed => style("✔", sem.success, sem.muted, true),
            TodoStatus::Pending => style("◻", sem.muted, sem.muted, false),
        };

        let mut content = item.content.clone();
        if item.status == TodoStatus::Pending {
            content.push_str(" (可开始)");
        }

        lines.push(Line::from(vec![prefix, Span::styled(content, text_style)]));
    }

    // 尾部 3 行空行
    for _ in 0..3 {
        lines.push(Line::from(""));
    }

    lines
}
```

- [ ] **Step 2: 集成到 message_area 渲染流程**

在 Spinner 渲染之后、消息流末尾之前插入 `render_todo_list` 调用。

- [ ] **Step 3: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
cargo test -p peri-tui --lib -- todo
```

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): add Todo list rendering per §2.4.5

◼ InProgress (accent+BOLD), ✔ Completed (success+CROSSED_OUT),
◻ Pending (muted) with (可开始) hint. 3 trailing blank lines."
```

---

## 完成标准

- [ ] `cargo check -p peri-tui -p peri-acp-types -p peri-widgets` 零错误零 warning
- [ ] `cargo test -p peri-tui --lib` 全部 PASS
- [ ] `cargo test -p peri-widgets --lib` 全部 PASS
- [ ] 所有 Phase 1 commit 完成
- [ ] Phase 2 commit 中 format_tool_name / format_tool_args / collapse logic 部分完成
- [ ] Phase 3 commit 中 theme tokens + sticky header + Todo 部分完成

## 后续议题

以下两项因 AgentEvent 层缺少数据源头，不在本 plan 范围内。需先修改 Agent 层产生对应事件后，方可实施渲染侧变更。

### SubAgent bg_hash 显示

- **阻断原因**: `bg_hash`（Agent 短哈希）在 `ExecutorEvent` / `AcpEvent` 中完全不存在
- **最近等价物**: `BackgroundTaskResult.task_id`（UUIDv7），可提取前 8 字符作为 hash
- **前置依赖**: Agent 层字段透传 或 ACP 映射层从 `task_id` 派生
- **涉及文件**: `peri-acp-types/src/view_model.rs` + `peri-tui/src/kit/view_render.rs`

### SubAgent 批次汇总（batch_agents）

- **阻断原因**: `AgentSummary { task_preview, tool_count, finished, is_error, final_result }` 所需的字段分散在 `SubagentStarted`、`SubagentStopped`、`BackgroundTaskCompleted` 三个事件中，无单一聚合事件
- **前置依赖**: Agent 层新增 `BatchCompleted` 事件，或 TUI 侧维护 `SubAgentStatusMap` 运行时聚合
- **涉及文件**: `peri-agent/src/agent/stages/` + `peri-acp/src/event/mapper.rs` + ViewModel + view_render
