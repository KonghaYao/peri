# Perihelion TUI 工具调用显示设计

> 版本: 2.0 | 日期: 2026-07-06
> 对应代码: `peri-tui/src/kit/view_render.rs` · `tool_display.rs` · `peri-acp-types/src/view_model.rs`
> 架构: v2 ratatui-kit 单路径

---

## 1. 设计总览

工具卡片由三元素构成：**状态指示器** + **工具名** + **参数摘要**，统一格式为：

```
● ToolName (参数一行摘要)        ← 头行
  ⎿ 输出行 1                     ← 可展开/折叠
  ⎿ 输出行 2
```

### 1.1 状态指示器

| 状态 | 图标 | 颜色 | 触发条件 |
|------|------|------|----------|
| 运行中 | `●`（800ms 闪烁） | 白色 | `is_running=true`, `is_error=false` |
| 完成 | `●` | 绿色 | `is_running=false`, `is_error=false` |
| 失败 | `●` | 红色 | `is_error=true` |

运行中闪烁机制：每约 50ms `RENDER_CALL_COUNT += 1`，以 `(count / 16) % 2 == 0` 判定可见/隐藏，周期约 800ms。

### 1.2 输出行

- 前缀 `⎿` 缩进 2 空格
- 正常完成：`muted` 色
- 执行失败：`error` 色
- 折叠态仍显示 **1 行**摘要（如 Read 的 "47 lines"），展开后最多展示 **4 行**，每行截断 **400 字符**，超出显示 `… N more lines`

### 1.3 折叠策略总表

| 工具 | 默认状态 | 何时展开 |
|------|---------|----------|
| **Read** | 折叠 | 用户按 Enter |
| **Glob** | 折叠 | 用户按 Enter |
| **Grep** | 折叠 | 用户按 Enter |
| **AskUserQuestion** | 折叠 | 用户按 Enter |
| **Bash** | 折叠 | 用户按 Enter |
| **TodoWrite** | 展开 | 始终 |
| **Write / Edit** | 完成后展开 | 自动；运行中折叠 |
| **AgentResult** | 展开 | 始终（自动） |
| **ExecuteExtraTool** | 展开 | 始终（自动） |
| **任何 is_error** | 展开 | 始终（强制，错误必须可见） |

### 1.4 显示名映射

TUI 内部通过 `format_tool_name()` 做少量名称简化：

| 内部名 | 显示名 | 说明 |
|--------|--------|------|
| Bash | Shell | 终端语境 |
| folder_operations | Folder | 简化 |
| 其他 | 原样 | 保留原始工具名 |

> **更新 (2026-07-06)**：已移除 TodoWrite→Todo、AskUserQuestion→Ask、WebSearch→Research、
> WebFetch→Browse、AgentResult→SubAgent、artifact→ArtUp 等多余映射。
> 大部分工具直接显示原始名称，代码更直观。

---

## 2. 各工具完整显示效果

### 2.1 Read — 文件读取

#### 运行中

```
● Read (src/main.rs)
```

#### 完成（折叠）

```
● Read (src/main.rs)
  ⎿ 47 lines
```

#### 完成（展开）

```
● Read (src/main.rs)
  ⎿ fn main() {
  ⎿     println!("Hello, world!");
  ⎿ }
  ⎿
  ⎿ … 47 more lines
```

**设计要点**：
- 参数摘要提取 `file_path`，不截断
- 折叠态显示行数摘要（"N lines"），由 ACP 层 `summarize_output` 生成
- 展开后输出摘要最多 4 行，每行最多 400 字符，超出折叠并显示剩余行数

#### 错误

```
● Read (nonexistent.txt)
  ⎿ Error: No such file or directory (os error 2)
```

**设计要点**：错误状态**不折叠**，第一行输出摘要用 error 色。

---

### 2.2 Write — 文件写入

#### 运行中

```
● Write (src/new_module.rs)
```

#### 完成（自动展开）

```
● Write (src/new_module.rs)
  ⎿ 12 lines changed
  ⎿ +12
```

**设计要点**：
- 完成后**强制展开**（`FORCE_EXPAND_ON_COMPLETE`），直接显示 output_summary
- output_summary 由 ACP 层 `summarize_output` 生成，格式为 "N lines changed"
- 额外显示 `diff_change_summary`：从 diff hunk 统计 +/- 行数（如 "+12" / "-100" / "+3 · -100"）
- diff 块渲染已于 2026-07-06 移除，不再展示逐行 diff 内容

#### 错误

```
● Write (src/main.rs)
  ⎿ Error: Permission denied (os error 13)
```

---

### 2.3 Edit — 文件编辑

#### 运行中

```
● Edit (src/main.rs)
```

#### 完成（自动展开）

```
● Edit (config.toml)
  ⎿ 3 lines changed
  ⎿ +3 · -2
```

**设计要点**：与 Write 完全一致。额外显示 `diff_change_summary` 展示增减行数。

#### 错误

```
● Edit (config.toml)
  ⎿ old_string not found in file
```

---

### 2.5 Glob — 文件匹配

#### 运行中

```
● Glob (pattern: "**/*.rs")
```

#### 完成（折叠）

```
● Glob (pattern: "**/*.rs")
  ⎿ 23 lines
```

#### 完成（展开）

```
● Glob (pattern: "**/*.md")
  ⎿ README.md
  ⎿ CHANGELOG.md
  ⎿ CLAUDE.md
  ⎿ TUI-TOOLCALL.md
  ⎿ … 12 more lines
```

#### 错误

```
● Glob (pattern: "**[invalid")
  ⎿ Error: Invalid glob pattern
```

**设计要点**：参数摘要提取 `pattern`，截断 200 字符。Glob 默认折叠（结果可能很长）。

---

### 2.6 Grep — 内容搜索

#### 运行中

```
● Grep (pattern: "fn render_tool")
```

#### 完成（折叠）

```
● Grep (pattern: "fn render_tool")
  ⎿ 8 lines
```

#### 完成（展开）

```
● Grep (pattern: "fn render_tool")
  ⎿ peri-tui/src/kit/view_render.rs:209    fn render_tool_card(
  ⎿ peri-tui/src/kit/view_render.rs:546    fn render_subagent_group(
  ⎿ peri-tui/src/kit/view_render.rs:759    mod tests {
  ⎿ … 5 more lines
```

**设计要点**：Grep 默认折叠（搜索结果可能极长）。参数 `pattern` 截断 200 字符。

---

### 2.7 folder_operations — 目录操作

#### 运行中

```
● Folder (list · /tmp/workdir)
```

#### 完成（展开）

```
● Folder (create · /tmp/workdir)
```

#### 错误

```
● Folder (create · /root/secret)
  ⎿ Error: Permission denied
```

**设计要点**：参数摘要提取 `operation` + `folder_path`。目录操作通常无复杂输出，显示名映射为 "Folder"。

---

### 2.8 Bash — Shell 执行

#### 运行中

需要计算时间的功能

```
● Shell (cargo build --release)
  ⎿  Running (1min)
```

#### 完成

```
● Shell (cargo build -p peri-tui)
  ⎿    Compiling peri-tui v0.1.0
  ⎿    Compiling peri-acp v0.1.0
  ⎿    Finished dev [unoptimized + debuginfo] in 3.45s
  ⎿
```

**设计要点**：
- 参数摘要提取 `command`，截断 400 字符
- 默认折叠（Shell 输出可能极长）
- 展开后最多 4 行 × 400 字符

#### 错误（非零退出码）

```
● Shell (rm -rf /protected)
  ⎿ rm: cannot remove '/protected': Permission denied
```

#### 超时

```
● Shell (sleep 999)
  ⎿ Command timed out after 120s
```

---

### 2.9 WebSearch — 网页搜索

#### 运行中

```
● WebSearch (query: "rust async best")
```

#### 完成（折叠）

```
● WebSearch (query: "rust async best")
```

#### 完成（展开）

```
● WebSearch (query: "rust async best practices")
  ⎿ 1. Async Programming in Rust | Official Docs
  ⎿ 2. Tokio Tutorial - Asynchronous Rust
  ⎿ 3. Rust Async Book - Comprehensive Guide
  ⎿ 4. Comparing async patterns in Rust vs Go
  ⎿ … 6 more results
```

#### 错误

```
● WebSearch (query: "")
  ⎿ Search query cannot be empty
```

**设计要点**：参数 `query` 截断 60 字符。显示名 "WebSearch"。

---

### 2.10 WebFetch — 网页抓取

#### 运行中

```
● WebFetch (url: https://docs.rs/tokio/latest/tokio/)
```

#### 完成（展开）

```
● WebFetch (url: https://docs.rs/tokio/latest/tokio/)
  ⎿ Tokio - An asynchronous runtime for Rust
  ⎿ The Tokio runtime provides I/O, networking,
  ⎿ scheduling, timers, and more. It is the
  ⎿ foundation for many async applications...
  ⎿ … 142 more lines
```

#### 完成（截断，>2000 行）

```
● WebFetch (url: https://example.com/large-doc)
  ⎿ [Content truncated: 2340 lines total]
  ⎿ First few lines of content here...
  ⎿ ...
  ⎿ Last line of visible content...
```

#### 错误

```
● WebFetch (url: https://invalid.domain/nonexistent)
  ⎿ DNS error: no such host
```

#### 超时

```
● WebFetch (url: https://slow-server.example.com)
  ⎿ Request timeout after 30s
```

**设计要点**：参数 `url` 不截断。显示名 "WebFetch"。

---

### 2.11 TodoWrite — 任务清单

#### 运行中

```
● TodoWrite
```

#### 完成（展开）

```
● TodoWrite
  ⎿ ☐ 实现登录模块 — pending
  ⎿ ☑ 添加单元测试 — completed
  ⎿ ☐ 编写 API 文档 — pending
  ⎿ [*] 代码审查 — in_progress
```

**设计要点**：
- 始终展开（用户需要看到任务列表）
- 各状态图标：`☐` pending / `[*]` in_progress / `☑` completed
- 显示名 "TodoWrite"（原样）

#### 空清单（所有任务完成后）

```
● TodoWrite
  ⎿ (无待办项)
```

---

### 2.12 AskUserQuestion — 用户问答

AskUserQuestion 使用 `AskUserBlock` ViewModel（非 ToolCard），渲染逻辑独立。

#### 问题展示

```
● User answered Peri's questions:
  ⎿ 部署方式 → Docker
  ⎿ 环境 → Production
```

#### 未回答时（运行中）

```
● AskUserQuestion选择部署方式)
  After choosing, the answer will appear here.
```

**设计要点**：
- 仅问题已回答后转换到 `AskUserBlock`，ToolCard 折叠态显示参数摘要
- 展开后每个 item 显示 `header → answer`

---

### 2.13 Agent — SubAgent 派发

SubAgent 使用 `SubAgentGroup` ViewModel，渲染逻辑独立于 ToolCard。

#### 运行中

```
❯ Agent(sub-search) 搜索 rust 异步模式… · ⏳ 5 步
    ● Read (src/search.rs)
      ⎿ pub fn search(query: &str) -> Vec<Result> { ... }
      ⎿ … 23 more lines
    ● Grep (pattern: "async fn")
      ⎿ src/search.rs:12
      ⎿ src/search.rs:45
    ● Shell (cargo test search -- --nocapture)
    … 2 more tools
```

#### 完成

```
❯ Agent(sub-search) 搜索 rust 异步模式… ✅
    ● Read (src/search.rs)
    ● Grep (pattern: "async fn")
    ● Shell (cargo test search -- --nocapture)
    ⎿ 搜索完成：在 3 个文件中找到 12 个异步函数
```

**状态指示器**：
| 状态 | 图标 |
|------|------|
| 运行中 | ⏳ + 步数 |
| 完成 | ✅ |
| 失败 | ❌ |

#### 折叠态

```
❯ Agent(sub-search) 搜索 rust 异步模式… ⏳ 5 步
```

#### 设计要点

- 嵌套 ToolCard 最多保留 **最后 5 个**
- 跳过内部 `AssistantBubble`
- 子消息缩进 **2 空格**
- 完成后显示 `final_result` 摘要（前 3 行，每行 80 字符）
- 折叠时运行中仍显示步数标签；完成后仅显示 ✅

#### 失败

```
❯ Agent(sub-search) 搜索 rust 异步模式… ❌
    ● Bash (cargo build)
      ⎿ error[E0432]: unresolved import `foo`
```

---

### 2.14 AgentResult — SubAgent 结果回传

#### 完成（自动展开）

```
● AgentResulttask_a1b2c3)
  ⎿ SubAgent subtask completed: 5 tool calls, 2 errors
  ⎿ Final output: The implementation is ready for review
```

#### 错误

```
● AgentResulttask_a1b2c3)
  ⎿ SubAgent subtask failed: context budget exceeded
```

**设计要点**：
- 自动展开（`AUTO_EXPAND`）
- 参数摘要提取 `task_id`，截断 12 字符
- 显示名原样（当前为 "AgentResult"）

---

### 2.15 ExecuteExtraTool — 延迟工具代理

#### 2.15.1 artifact — 工件上传

##### 运行中

```
● artifact (index.html)
```

##### 完成

```
● artifact (index.html)
  ⎿ Uploaded to https://artup.example.com/abc123
  ⎿ Expires in 7 days
```

##### 错误

```
● artifact (/invalid/path.html)
  ⎿ Error: file not found or not accessible
```

**设计要点**：参数摘要提取 `file_path`，不截断。显示名 "artifact"。

---

#### 2.15.2 LSP — 语言服务

##### 运行中

```
● LSP (hover)
```

##### 完成

```
● LSP (hover)
  ⎿ pub fn render_tool_card(data: &ToolCardData, ...) -> Vec<Line>
  ⎿ Renders a tool call card with status indicator,
  ⎿ parameter summary, and output block.
  ⎿ … 3 more lines
```

##### 错误

```
● LSP (hover)
  ⎿ LSP server not responding
```

**设计要点**：参数摘要提取 `operation`，截断 40 字符。LSP 操作（hover/definition/completion 等）的语义由 LLM 理解，渲染层不区分。

---

#### 2.15.3 MCP 工具

```
● github (search_repos)
  ⎿ Found 42 repositories matching "rust async"
  ⎿ 1. tokio-rs/tokio ★12.3k
  ⎿ 2. async-rs/async-std ★3.8k
  ⎿
  ⎿ … 40 more results
```

**设计要点**：显示名使用 MCP 服务名（如 `github`），参数提取 `tool_name`。

---

#### 2.15.4 Cron 工具

##### 运行中

```
● CronRegister (*/5 * * * *)
```

##### 完成

```
● CronRegister (*/5 * * * *)
  ⎿ Scheduled: health_check every 5 minutes
```

##### 错误

```
● CronList
  ⎿ Cron service not available
```

**设计要点**：Cron 系列工具（CronRegister/CronList/CronRemove）统一走 ExecuteExtraTool 分发。

---

**总设计要点**：
- 自动展开（`AUTO_EXPAND`）
- 参数提取：`ExecuteExtraTool` → `tool_name`（40 字符），`SearchExtraTools` → `query`（40 字符）
- 显示名：Bash → Shell，folder_operations → Folder，其余工具保留原始名称

---

### 2.16 SearchExtraTools — 工具搜索

```
● SearchExtraTools (query: "slack send")
  ⎿ Found 2 matching tools:
  ⎿   · slack.send_message
  ⎿   · slack.send_direct
```

**设计要点**：Meta 工具，LLM 不可直接看见；结果自动展开。

---

## 3. Diff 块渲染（已废弃）

> **更新 (2026-07-06)**：diff 块渲染已从 TUI 中完全移除。`render_diff_block()` 函数已删除，
> `Ctrl+O` 切换 diff 的功能已移除。Write/Edit 工具现在直接显示 output_summary
> （如 "12 lines changed"），不再展示 diff 内容。
>
> ACP 层 (`view_mapper.rs`) 仍在构建 `DiffBlock` 数据（供 IDE 等外部 consumer 使用），
> 但 TUI 渲染管道不再消费该字段。

---

## 4. SystemNote — 系统通知

非工具调用，但常与工具结果联动展示。

```
✻ System note (Info)
· This is a regular note line
· ⚠ 已中断 — Agent stopped by user
· ❌ Session failed to initialize
⎿ This line starts with ⎿ prefix
  ⎿ Error summary line — error colored
```

**规则**：
| 行前缀 | 颜色 | 说明 |
|--------|------|------|
| `✻` | `dim` | 元信息 |
| `⎿`（行首） | `muted` | 结果引用 |
| `  ⎿`（缩进） | `error` | 错误摘要 |
| 其他 | 行内容检测 | 含 `❌`/`失败`/`error` → error；`⚠`/`已中断` → warning；否则 muted |

---

## 5. CollapsedGroup — 聚合折叠组

```
┌ 5 tool calls collapsed ────────────────┐
│ [内部可包含多个 ToolCard]               │
└────────────────────────────────────────┘
```

**设计要点**：标题 `"N tool calls collapsed"`，内部保留完整的 `view_models`。

---

## 6. Divider — 回合分隔线

```
─────────────────── Turn 3 ───────────────────
```

**设计要点**：`label: Option<String>`，无 label 时纯分隔线。

---

## 7. 数据模型参考

### 7.1 ToolCardData

```rust
pub struct ToolCardData {
    pub tool_id: String,          // 与 AI 消息中 tool_use 的 id 配对
    pub tool_name: String,        // 原始工具名（Bash/Read/Write/Edit/...）
    pub input_summary: String,    // ACP 层预摘要的参数文本
    pub output_summary: String,   // 工具执行结果文本
    pub is_error: bool,           // 执行失败标志
    pub is_running: bool,         // 流式进行中（default false）
    pub diff: Option<DiffBlock>,  // Write/Edit 的结构化 diff（TUI 不再消费，仅 IDE 使用）
    pub content_hash: u64,        // #[serde(skip)] 增量渲染缓存键
}
```

### 7.2 DiffBlock

```rust
pub struct DiffBlock {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub is_binary: bool,
    pub is_too_large: bool,
    pub is_new_file: bool,
}

pub struct Hunk {
    pub old_range: String,    // e.g. "1,3"
    pub new_range: String,    // e.g. "1,4"
    pub lines: Vec<HunkLine>,
}

pub struct HunkLine {
    pub kind: HunkLineKind,   // Add / Del / Context
    pub text: String,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
}
```

### 7.3 SubAgentGroupData

```rust
pub struct SubAgentGroupData {
    pub agent_id: String,
    pub agent_name: String,
    pub view_models: Vec<ViewModel>,  // 嵌套子 ViewModel
    pub collapsed: bool,
    pub is_running: bool,
    pub content_hash: u64,
}
```

---

## 8. 渲染管道

```
ExecutorEvent (ToolStart / ToolEnd)
        │
        ▼
event/mapper.rs   ─── map_event() → SessionUpdate (ACP 标准协议)
        │
        ▼
event/router.rs   ─── route() → peri/agent_event (TUI 专属通道)
        │
        ▼
acp_bridge.rs     ─── dispatch_and_notify() → VIEW_MODELS atom
        │
        ▼
view_mapper.rs    ─── BaseMessage → ViewModel 转换（增量缓存）
    ├─ Human        → UserBubble / SystemNote
    ├─ Ai           → AssistantBubble（+ tool_card_ids）
    ├─ Tool(Agent)  → SubAgentGroup
    ├─ Tool(Ask)    → AskUserBlock
    ├─ Tool(其他)   → ToolCard
    └─ System       → SystemNote
        │
        ▼
render_bridge.rs  ─── 独立 tokio task
    ├─ 监听 ACP 事件 + 宽度变更
    ├─ content_hash 增量检测 → 跳过未变更的 ViewModel
    ├─ render_v2_vm() 逐条转 Line<'static>
    ├─ 构建 cumulative_heights + wrap_map
    └─ 写入 RENDER_CACHE atom
        │
        ▼
message_area.rs   ─── ratatui-kit ScrollView
    ├─ 从 RENDER_CACHE 读取 Vec<Line<'static>>
    ├─ viewport_clip() 二分查找可见范围
    └─ Paragraph + Wrap 渲染
```

### 8.1 关键常量

| 常量 | 值 | 位置 |
|------|----|------|
| 输出最大行数 | 4 | `compact_output_lines` |
| 每行最大字符 | 400 | `compact_output_lines` |
| 参数摘要最大字符 | 工具相关 | `format_tool_args` |
| 闪烁周期 | ~800ms | `RENDER_CALL_COUNT / 16` |
| 子 Agent 工具上限 | 5 | `render_subagent_group` |

---

## 9. 设计原则

1. **信息密度优先** — 默认折叠低价值结果（Read/Glob/Grep），展开高价值改动（Write/Edit 显示 output_summary）
2. **错误必须可见** — 任何 `is_error=true` 强制展开，红色 `●` 醒目标识
3. **增量渲染** — `content_hash` 跳过未变更项，避免全量重建
4. **统一状态语言** — 三种状态（运行中/完成/失败），统一 `●` 图标，三套颜色（白/绿/红）
5. **参数一行摘要** — 每工具定制关键字段提取 + 独立截断长度
6. **流式友好** — `is_running` 支持逐行追加，指示器 800ms 闪烁
7. **折叠独立** — 工具卡片折叠仅由 tool_name + is_running/is_error 决定，不再区分 diff 可见性
8. **SubAgent 隔离** — 嵌套消息缩进处理，最多 5 个 ToolCard，跳过 AssistantBubble
