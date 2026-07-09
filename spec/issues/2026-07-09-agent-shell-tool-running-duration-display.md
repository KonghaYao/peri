# Agent/Shell 工具运行计时和 tool calls 计数显示改进

**状态**：Open
**优先级**：中
**类型**：Enhancement
**创建日期**：2026-07-09

## 问题描述

三个工具卡片的显示需要改进：

### 问题 1：Shell 工具超过 1 分钟丢秒数

当前 Shell（Bash）工具运行中显示 `Running (Xs)` 或 `Running (Xmin)`，一旦超过 1 分钟就只显示整数分钟，丢失秒数精度。例如运行 85 秒，实际显示 `Running (1min)`，无法感知剩余时间。

### 问题 2：Agent 工具缺少运行计时和 tool calls 统计

Agent 工具调用作为 TuiToolCard 显示为 `● Agent (task description)`，但没有运行时间和 tool calls 计数。期望在 Agent 工具卡片下仿照 Shell 增加 `⎿ n tool calls, running Xmin Xs` 行。

### 问题 3：Agent 工具有两套渲染，箭头前缀需删除

同一个 Agent 工具调用在界面上出现了**两个渲染实体**：

| 来源 | 格式 | 前缀 | 视觉效果 |
|------|------|------|----------|
| `TuiToolCard` → `render_tool_card` | `● Agent (task description)` | ● 原点 | 正确 |
| `TuiSubAgentGroup` → `render_subagent_group` | `❯ Agent(agent_id) name · ⏳ 3 步` | ❯ 箭头 | **多余、需删除** |

Agent 工具派发 SubAgent 后，TUI 同时渲染了 ToolCard（原点）和 SubAgentGroup 头行（箭头），造成同一件事显示两次。应删除箭头开头的 SubAgentGroup 头行，统一用原点开头的 ToolCard 显示。

### 现状 vs 期望

**现状**（问题 2+3）：
```
● Agent (search rust async patterns)           ← ToolCard，原点开头
❯ Agent(sub-search) search rust async patterns · ⏳ 3 步    ← SubAgentGroup 头，箭头开头（多余）
  ▶ 2 collapsed tools                         ← SubAgentGroup 子内容
  ● Read (src/lib.rs)
  ⎿ 15 lines
  ● Grep (pattern: async fn)
  ⎿ 8 results
```

**期望**：
```
● Agent (search rust async patterns)           ← 只有 ToolCard，原点开头
  ⎿ 5 tool calls, running 1min 23s             ← 运行中显示：计数 + 计时
  ▶ 2 collapsed tools                         ← SubAgent 子内容（无独立头行）
  ● Read (src/lib.rs)
  ⎿ 15 lines
  ● Grep (pattern: async fn)
  ⎿ 8 results
```

## 期望行为

### Shell 工具
- 保持 `Xmin Xs` 格式（与 Spinner 的 `Xm Xs` 区分，用 `min` 后缀保持可读性）
- 示例：`Running (45s)` → `Running (1min 23s)` → `Running (5min 10s)`

### Agent 工具
- 在 ToolCard 标题下仿 Shell 加独立行 `⎿ n tool calls, running Xmin Xs`
- 删除 SubAgentGroup 的 `❯ Agent(...)` 头行，子内容（tool cards、final_result）从紧接 ToolCard 的下一行开始渲染
- 计时取 SubAgent 第一个子 card 的 `started_at`，TUI 层统计 subagent 子 card 中 ToolCard 数量

### 格式统一性

| 组件 | 格式 | 示例 |
|------|------|------|
| Spinner 加载计时 | `Xm Xs` / `Xs` | `3m 25s`, `45s` |
| Shell Running 行 | `Xmin Xs` / `Xs` | `1min 23s`, `45s` |
| Agent Running 行 | `Xmin Xs` / `Xs` | `1min 23s`, `12s` |

## 涉及文件

| 文件 | 变更内容 |
|------|---------|
| `peri-tui/src/kit/view_render.rs:330-337` | `format_running_duration` 格式从 `Xmin` 改为 `Xmin Xs` |
| `peri-tui/src/kit/view_render.rs:227-327` | `render_tool_card`：当 `tool_name == "Agent"` 且运行中时，追加 running 行 |
| `peri-tui/src/kit/view_render.rs:540-714` | `render_subagent_group`：删除 `❯ Agent(...)` 头行（保留子内容渲染） |
| `peri-tui/src/kit/view_render.rs:32-57` | `SubAgentRenderInfo` 新增 `tool_calls_count` / `started_at` 字段 |

## 技术细节

### Shell 计时格式改动点

**文件** `peri-tui/src/kit/view_render.rs:330-337`：
```rust
// 当前
fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}min", secs / 60)  // ← 丢秒数
    }
}

// 改为
fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}
```

### Agent 运行行渲染

**在 `render_tool_card` 中**，紧接现有 Bash Running 行逻辑之后，增加 Agent 判断：

```rust
// 现有：Bash running 行（保留）
if data.tool_name == "Bash" && data.is_running && !data.is_error { ... }

// 新增：Agent running 行
if data.tool_name == "Agent" && data.is_running && !data.is_error {
    // tool_calls_count 从 SubAgentRenderInfo 探针获取
    // running_duration_ms 同 Bash 逻辑
    lines.push(Line::from(vec![
        Span::styled("  ⎿ ", ...),
        Span::styled(format!("{} tool calls, running ({})",
            tool_count, format_running_duration(duration)), ...),
    ]));
}
```

### SubAgentGroup 头行删除

**`render_subagent_group`（约第 553-607 行）**：删除构建 `❯ Agent(agent_id) name · ⏳` 头行的代码块。只保留子内容渲染（折叠摘要、children 遍历、final_result 预览），子内容紧接 ToolCard 的 `⎿ running 行` 之后渲染。

### SubAgentRenderInfo 扩展

```rust
pub struct SubAgentRenderInfo {
    // 现有字段
    pub is_running: bool,
    pub is_error: bool,
    pub total_steps: usize,
    pub final_result: Option<String>,
    pub recent_messages: Vec<TuiRenderUnit>,
    // 新增字段
    pub tool_calls_count: usize,     // subagent 内部 tool card 总数
    pub started_at: Option<Instant>,  // subagent 启动时间
}
```

`tool_calls_count` 在 TUI 层填充（遍历 recent_messages 统计 ToolCard），或在 app 层注入时预先计算。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |

## 修复记录

（待修复）
