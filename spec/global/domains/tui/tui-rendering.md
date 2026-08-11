# TUI 渲染系统

> 本文档描述 TUI 的渲染相关组件：AppShell 根页面、MessageArea 消息区、StatusBar 状态栏、BgTaskArea 后台任务区。包含视口裁剪、滚动节流等渲染策略。

---

## 1. AppShell 根页面

### 1.1 正常主界面

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  ● Assistant response markdown...                                             │
│                                                                              │
│  ⏺ Read 3 files                                                               │
│                                                                              │
│  ❯ 用户输入                                                                   │
│                                                                              │
│  ◜ 思考中… (12s · ↓ 1.2k tokens)                                             │
│    ◼ 进行中  设计 Workflow Panel                                              │
│                                                                              │
│  ● agent (coder)  修改文档                                   2min 15s       │
│                                                                              │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ ❯ 输入你的任务...                                                        │ │
│ │ @ mention files    / commands          CPU 12% · MEM 430MB · 42% ctx     │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Auto · perihelion · anthropic/claude-code-sonnet                             │
│                 /::commands · Shift+Enter::newline · Ctrl+T::mode · Ctrl+O::diff│
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 聚合展示对话、工具调用、工具结果、SubAgent、后台 Agent 状态、系统通知和当前 streaming turn。
- 输入区支持多行编辑、历史、文件 mention、slash command、软换行、视口跟随、placeholder。
- 状态栏持续暴露运行环境、权限模式、模型与后台任务；CPU%/MEM/上下文使用率在 composer footer 右侧资源线。
- BgTaskArea 展示后台 Agent（background subagent）的运行状态和耗时。

### 1.2 Setup Wizard 首次启动页

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│                                                                              │
│          ┌────────────────────── Setup Wizard ──────────────────────┐         │
│          │                                                          │         │
│          │                    欢迎使用 Peri TUI                     │         │
│          │                                                          │         │
│          │  ● 未配置 Provider — Agent 功能不可用                    │         │
│          │                                                          │         │
│          │  要配置 Provider，请选择以下任一方式：                   │         │
│          │                                                          │         │
│          │    1. 进入主界面后打开 Login 页面配置 API Key            │         │
│          │    2. 或打开 Settings 页面调整 Provider 配置             │         │
│          │    3. 或手动编辑 ~/.peri/settings.json                   │         │
│          │                                                          │         │
│          │  按 Enter / Esc 跳过向导，进入主界面                 │         │
│          └──────────────────────────────────────────────────────────┘         │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- Provider 未配置时引导用户进入 Login / Config 或编辑配置文件。
- 可跳过，不阻断进入主界面。
- 已配置时显示当前 Provider 和模型 alias。


---

## 2. MessageArea 区域组件

### 2.1 空消息 Welcome

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│                       ██████╗ ███████╗██████╗ ██╗                            │
│                       ██╔══██╗██╔════╝██╔══██╗██║                            │
│                       ██████╔╝█████╗  ██████╔╝██║                            │
│                       ██╔═══╝ ██╔══╝  ██╔══██╗██║                            │
│                       ██║     ███████╗██║  ██║██║                            │
│                       ╚═╝     ╚══════╝╚═╝  ╚═╝╚═╝                            │
│                                                                              │
│              Your AI operating system for code, tools, and workflows          │
│                                                                              │
│              ────────────────────────────────────────                        │
│                                                                              │
│               • Code across the repo with shared context                      │
│               • Open files, run tools, and inspect results                    │
│               • Delegate work to agents and workflows                         │
│                                                                              │
│                 /model   /agents   /tasks   /help                            │
│                                                                              │
│                 Enter::send · Shift+Enter::newline · @::mention-files     │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 空会话时展示产品定位、核心能力、常用命令和输入提示。
- 窄屏下 Logo 降级为 `Peri` 文本标题。

### 2.2 消息流渲染

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ❯ 设计一下 TUI 页面                                                           │
│                                                                              │
│ ● 我会先梳理现有区域组件，然后写入 TUI-PAGE.md。                              │
│                                                                              │
│ ⏺ Read 4 files                                                               │
│                                                                              │
│ ⏺ Bash (cargo test -p peri-tui --lib)                                        │
│   ⎿ test result: ok. 42 passed                                               │
│                                                                              │
│ ● coder                                                                       │
│   设计文档已生成...                                                           │
│                                                                              │
│ ✗ Bash (cargo clippy)                                                         │
│   ⎿ error: ...                                                                │
│                                                                              │
│ ◜ 思考中… (12s · ↓ 1.2k tokens)                                             │
│                                                                              │
│   ● 进行中  整理 TUI 页面设计                                                │
│   ○ 待处理  写入 TUI-PAGE.md                                                │
│   ✓ 已完成  梳理现有 spinner 组件                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 统一渲染 TUI 内部类型 `TuiRenderUnit` 8 变体（`tui_render_unit.rs`）：TuiUserBubble / TuiAssistantBubble / TuiToolCard / TuiSystemNote / TuiSubAgentGroup / TuiCollapsedGroup / TuiDivider / TuiAskUserBlock。
- 各变体渲染函数位于 `message_area/render.rs`（`vm_to_lines_cached` + 各变体渲染辅助函数）。
- ratatui-kit-markdown 做 Markdown 解析 + 代码高亮（通过 PaletteProvider 接入 Theme System）；`kit/markdown/` 自行实现 `ParsedBlock → Line` 转换与流式增量缓存（稳定前缀持久化 + 尾部不稳定块回滚）。
- 渲染缓存按 VM `content_hash` 分片（`VmCacheSlot`，`message_area/mod.rs`）：仅正在流式的 VM 重新解析 markdown + 重建 wrap_map，其余 VM 直接 `Arc::clone` 复用，流式单次成本从 O(N×W) 降至 O(W)；单 Paragraph 渲染，消除每帧 N 个 widget 树开销。
- 支持 diff 可见性切换（Ctrl+O），diff 内容自动使用增删行语义色。
- 鼠标滚轮滚动消息区；键盘 Up/Down 保留给输入区。

### 2.3 Loading Spinner + Todo

TUI loading 统一使用 `peri-tui/src/components/spinner`（`SpinnerState` / `SpinnerMode` / 动画帧 `animation.rs`），spinner 行渲染在 `message_area/footer.rs`。禁止在 MessageArea 中手写独立 loading 文案或自造 spinner。

**架构（v2.1）**：LoadingFooter 作为 MessageArea 的固定子区域，位于可滚动内容之外、消息流底部。不随消息区滚动，空态时显示灰色 Brewed 总结行（不复位为 0 高度）。数据流：`ACP_STATE.is_loading` + `TODO_ITEMS` atom → 每轮渲染按壁钟时间补偿步进（once 门控防 tight loop）。

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ MessageArea（可滚动内容区）                                                    │
│                                                                              │
│ ● 我会先梳理当前 TUI 页面结构，然后更新设计文档。                              │
│                                                                              │
│ ⏺ Read 3 files                                                               │
╞══════════════════════════════════════════════════════════════════════════════╡
│ LoadingFooter（固定，不滚动）                                                 │
│                                                                              │
│ ✳ 思考中… (12s · ↓ 1.2k tokens)                                              │
│                                                                              │
│   ◼ 进行中  更新 Workflow Panel 设计                                          │
│   ◻ 待处理  补充 spinner + todo 设计图                                        │
│   ◻ 待处理  复核快捷键与边框规则                                              │
│   ✔ 已完成  阅读 spinner 组件                                                │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- `SpinnerMode::Thinking`：模型推理中，verb 默认 `思考中…`。
- `SpinnerMode::ToolUse`：工具执行中，verb 默认 `执行工具…`。
- `SpinnerMode::Responding`：回复生成中，verb 默认 `正在生成回复…`。
- Spinner 帧使用 **`accent` 橙色**（`#D77757`），辅助文本使用 `muted` 灰色。
- Spinner 后缀展示 elapsed time 与 token count，例如 `(12s · ↓ 1.2k tokens)`。
- Todo 列表显示在 Spinner 下方，不嵌入 Spinner 主行。
- Todo 样式沿用 ACP `SessionUpdate::Plan` / IDE plan 组件语义，但不显示额外标题或分隔线；Spinner 下方直接渲染 todo list。
- Todo 列表与 TodoWrite 工具状态挂钩：`in_progress` 显示 `◼ 进行中`，`pending` 显示 `◻ 待处理`，`completed` 显示 `✔ 已完成`。
- Todo 文本渲染 `TodoItem.content` 字段（`footer.rs`：`TodoItem { status, content }`，无其他文本字段）。
- Todo 列表最多展示当前 in-progress、接下来的 2-3 个 pending、最近 1 个 completed；超出数量用 `+N more` 折叠。
- Todo 数据来自 ACP-only data flow：TodoWrite 工具结果映射为标准 `SessionUpdate::Plan`；若标准通道不足，再通过 `peri/unstable-event` 推入 TUI store。

### 2.4 消息渲染样式详细规范

> 本节定义 MessageArea 中每种消息类型的**精确视觉规格**——颜色、前缀符号、间距、字体和布局规则。
> 参数化颜色引自 [Theme System v2](#theme-system-v2) 的 SemanticTokens，此处引用语义名和设计参考值。
>
> **ASCII 图约定**：`————————` 表示该行内容延续到终端右边界（满宽），用于示意布局而非实际文本长度。空白行 `│  │` 省略中间内容区，仅保留左右边界示意。

#### 2.4.1 颜色 Token 参考

##### 强调色与功能色

| Token | Hex 参考 | 语义 |
|-------|----------|------|
| `accent` | `#D77757` | Claude 暖橙：用户消息前缀、激活边框、光标、Logo、关键操作 |
| `success` | `#4EBA65` | 工具成功、SubAgent 前缀、`✔` 对勾 |
| `warning` | `#FFC107` | 标题、次要强调、重试态、用户按钮、权限标签 |
| `error` | `#FF6B80` | 工具失败、错误摘要、缓存警告 |
| `thinking` | `#A2A9E4` | 推理/CoT 思考、面板选中行 |
| `loading` | `#93A5FF` | Loading 动画、SubAgent 箭头、Auto Mode 标签 |
| `model_info` | `#A0825F` | 状态栏模型名（棕金） |
| `bash_border` | `#FD5DB1` | Bash 工具结果边框（粉红） |
| `selected_fg` | `#B2B9F9` | 列表选中项前景色 |

##### 文字层级

| Token | Hex 参考 | 用途 |
|-------|----------|------|
| `text` | `#FFFFFF` | 主文字、AI 回复、工具名、用户消息、Todo InProgress |
| `muted` | `#999999` | 次要文字、标签、路径、Spinner 辅助、折叠预览 |
| `dim` | `#505050` | 占位符、分隔符、前缀 `⎿`/`·`、已完成项、滚动条 |

##### 底色

| Token | Hex 参考 | 用途 |
|-------|----------|------|
| `user_bg` | `#373737` | 用户消息整行底色 |
| `popup_bg` | `#000000` | 弹窗底色 |
| `cursor_bg` | `#262626` | 列表光标行背景 |
| `selection_bg` | `#264F78` | 文本选区背景色（暗蓝） |
| `subagent_bg` | `#1E1E26` | SubAgent 嵌套消息背景色 |

##### 边框色

| Token | Hex 参考 | 用途 |
|-------|----------|------|
| `border` | `#505050` | 空闲/标准面板边框 |
| `border_dim` | `#2A2A30` | 非活跃 Session 分隔线 |
| `border_active` | `#D77757` | 激活边框（= accent） |

##### Diff 高亮色

| Token | Hex 参考 | 用途 |
|-------|----------|------|
| `diff_add` | `#3FB950` | 新增行前景色 |
| `diff_add_bg` | `#12341A` | 新增行背景色 |
| `diff_add_word_bg` | `#1A4E24` | 新增单词级高亮 |
| `diff_remove` | `#F85149` | 删除行前景色 |
| `diff_remove_bg` | `#371412` | 删除行背景色 |
| `diff_remove_word_bg` | `#4E1C16` | 删除单词级高亮 |
| `diff_hunk` | `#578FA9` | Hunk 头部 (`@@`) 青色 |

---

#### 2.4.2 消息类型视觉规格

##### 用户消息 `UserBubble`

```
❯ 这是一条用户消息内容——————————————————————————
  续行自动缩进两个空格对齐——————————————————————
```

| 属性 | 规格 |
|------|------|
| 前缀 | `❯`，`accent` 色，**BOLD** |
| 底色 | 整行 `user_bg` |
| 首行 | `❯ ` + 内容 |
| 续行 | `  `（两个空格缩进）+ 内容 |
| system_reminder | 缩略渲染（`render_reminder_condensed`）：`ReminderType` 标签（10 类分类，`tui_render_unit.rs`，`dim` 色 *ITALIC*）+ 可选摘要行（`⎿` 前缀 + `muted` 内容）；无前缀/无底色 |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

##### AI 回复 `AssistantBubble`

```
AI 回复的 Markdown 内容段落，由 Markdown 渲染器处理。————————
段落之间由空行分隔。——————————————————————————————————————

代码块自动语法高亮：
  code example here

> 引用块文本并入普通段落渲染（无专用前缀符号）
```

| 元素 | 规格 |
|------|------|
| **正文段落** | `text` 色，Markdown 解析后逐行输出 |
| **标题 H1-H3** | `warning` 色，**BOLD**，前后各 1 空行（去重） |
| **标题 H4+** | `muted` 色，**BOLD**，前后各 1 空行 |
| **行内代码** | `thinking` 色，无反引号包围 |
| **多行代码块** | `text` 色，syntect 语法高亮，前后各 1 空行 |
| **单行代码块** | `thinking` 色，简洁态 |
| **链接** | `success` 色，*UNDERLINED*，OSC-8 包裹 |
| **引用块** | 引用块文本并入普通段落渲染（ratatui-kit-markdown `ParsedBlock` 无 BlockQuote 变体，无 `▍` 前缀） |
| **列表** | `•` / `1.` 前缀，`text` 色，嵌套 `"  "` 缩进 |
| **加粗** | 继承颜色，**BOLD** |
| **斜体** | 继承颜色，*ITALIC* |
| **删除线** | 继承颜色，~~CROSSED_OUT~~ |
| **水平线** | `─` × 60 字符，`muted` 色，前后各 1 空行 |
| **表格** | `┌├└─│` BOX 绘制，CJK 对齐，`muted` 色边框 |
| **空行去重** | `ensure_blank_line()`：仅上前一行非空时插入 |

**Markdown 渲染器**（ratatui-kit-markdown）：
- 使用 `ratatui_kit_markdown` 的 `parse_markdown` + `ParsedBlock` 公开 API
- 替代旧 `peri_widgets::markdown` 自研引擎（删除 13 文件 ~1531 行）
- 通过 `PaletteProvider` trait 接入 Theme System，支持代码语法高亮
- 渲染缓存按 VM `content_hash` 分片（`VmCacheSlot`）——仅流式 VM 重新解析 markdown + 重建 wrap_map，其余 `Arc::clone` 复用；`kit/markdown/` 另有字节级前缀增量缓存（稳定前缀持久化 + 尾部不稳定块回滚）
- 单 `Paragraph` 渲染，消除每帧 N 个 widget 树开销
- 增量渲染 3.13µs/帧（旧引擎 12.93ms/帧，4131x 加速）

##### 推理块（CoT Thinking）

```
Thought for 1234 chars
 ⎿ 最后一行预览内容————————————————————————
   更多预览行内容———————————————————————————
```

| 属性 | 规格 |
|------|------|
| 首行 | `"Thought for N chars"`，`dim` 色 |
| 预览行 | 展开态（`collapsed = false`）显示：`" ⎿ "` 前缀（`dim`）+ 尾部内容（`dim`），最多 3 行，不折行直接按视觉宽度截断 |
| 折叠逻辑 | 默认折叠，仅显示首行（折叠策略单点定义于 `tui_render_unit.rs`：「仅最后一个含 reasoning 的 assistant bubble 展开」） |
| message_id 透传 | `TuiReasoningBlock` 仅 `text + collapsed`（无 message_id 字段）；按段分配在 acp_events 层完成 |
| 空行 | 首尾各加一个空行，保证与相邻消息块的间距 |

##### 工具调用 `ToolBlock`

```
● tool_name (参数摘要)———————————————
  ⎿ 工具执行结果内容———————————————————
```

| 状态 | 指示器 | 颜色 | 动画 |
|------|--------|------|------|
| Running | `●` | `running` 色 | 固定（当前无闪烁动画） |
| Completed | `●` | `success` | 固定 |
| Failed | `●` | `error` | 固定 |

> 注：Skill 表现形态（`TuiToolPresentation::Skill`）完成/失败用 `✓`/`✗`；Generic 形态一律 `●` + 语义色。

| 属性 | 规格 |
|------|------|
| 工具名 | `text` 色，**BOLD**，经过 `format_tool_name()` 映射显示名 |
| 参数摘要 | `" (summary)"`，`dim` 色，截断 400 Unicode 字符 |
| 结果前缀 | `"  ⎿ "`（`dim` 色）+ 内容（正常 `muted`，错误 `error`） |
| 折叠态摘要 | `"  ⎿ "`（`dim`）+ 1 行摘要（`muted`），截断 400 字符 |
| 展开态输出 | 最多 4 行（TodoWrite 不限），单行 400 字符截断 |
| 折叠/展开 | 默认折叠 `COLLAPSED_BY_DEFAULT` 列表（Bash/Read/Edit/Write/Glob/Grep/AskUserQuestion，`render.rs`）；错误与运行中强制展开 |
| Diff 视图 | 内嵌 diff 行 + 变更统计行（`⎿ +N −M`），默认关闭，Ctrl+O 切换 |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

**工具显示名映射表** (`format_tool_name`，`kit/tool_display.rs`)：

| 工具 | 显示名 |
|------|--------|
| Bash | Shell（i18n `tool-name-shell`） |
| folder_operations | Folder（i18n `tool-name-folder`） |
| 其他 | 原样显示（不转换） |

> 注：ToolCard 显示名由 `format_tool_name` 单点映射；Agent ToolCard 额外显示 tool calls count + running duration（`SubAgentGroup` 主行）。

**工具参数摘要规则** (`format_tool_args`，`kit/tool_display.rs`)：

| 工具 | 提取字段 | 截断 |
|------|---------|------|
| Bash | `command` | 400 字符 |
| Read/Write/Edit | `file_path`（相对化） | 不截断 |
| Glob/Grep | `pattern`（相对化） | 200 字符 |
| folder_operations | `operation` + `folder_path` | 不截断 |
| WebSearch | `query` | 60 字符 |
| WebFetch | `url` | 不截断 |
| ExecuteExtraTool/SearchExtraTools | `tool_name` / `query` | 40 字符 |
| AgentResult | `task_id` | 12 字符 |
| artifact | `file_path`（相对化） | 不截断 |
| LSP | `operation` | 40 字符 |

> 注：ACP 路径的 `input_summary` 由 view_mapper 预摘要；v2 直连路径经 `truncate.rs::summarize_input`（与 view_mapper 同风格）。

**折叠/展开规则**（`message_area/render.rs` 常量 + `render_generic_tool_card_lines`）：
- `COLLAPSED_BY_DEFAULT`：Bash / Read / Edit / Write / Glob / Grep / AskUserQuestion —— 默认折叠（折叠态显示 1 行摘要，`⎿` + 400 字符截断）
- `AUTO_EXPAND`：AgentResult / ExecuteExtraTool / SearchExtraTools —— 自动展开
- `FORCE_EXPAND_ON_COMPLETE`：空列表 —— 无强制展开工具
- 错误结果**强制展开**（`is_error → collapsed = false`）；运行中同样展开
- 展开态输出最多 4 行（`TodoWrite` 不限行数），单行 400 字符截断

##### 只读工具聚合组 `ToolCallGroup`

```
● Read 4 files————————————————————————————
```

| 属性 | 规格 |
|------|------|
| 标题 | `● summary`（`success` + `muted`） |
| 行为 | **不可展开**，仅单行汇总 |
| 出错 | 错误工具在聚合态中仍显示 `error` 色 error_summary |
| AskUser | **专用路径**：`● User answered Peri's questions:`（`success`/`error`）+ 子行 `  ⎿ header → answer` |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

##### SubAgent 消息 `SubAgentGroup`

**Agent 工具卡片**（Generic ToolCard 形态，`render_generic_tool_card_lines`）：
```
● Agent (任务预览摘要…)————————————————————————
  ⎿ N tool calls, running Xmin Xs
```

**折叠态**（SubAgentGroup 内嵌套消息）：
```
  ▶ N collapsed tools——————————————————————————
  嵌套消息首行内容——————————————————————————————
```

| 属性 | 规格 |
|------|------|
| 工具调用指示器 | `●` + 语义色（running/success/error），与 ToolBlock 规则一致 |
| 头行 | `● ` + 工具名（`text`，**BOLD**）+ ` (输入摘要)`（`dim`，截断 400 字符），`format_tool_name` 映射 |
| 运行中第二行 | `"  ⎿ N tool calls, running Xmin Xs"`（`dim` 前缀 + `muted` 内容），`tool_calls_count` > 0 时显示（`render.rs`） |
| 折叠摘要 | 嵌套工具 > 5 个时显示 `▶ N collapsed tools`（`dim` + `muted`）；`collapsed` 时仅显示摘要行 |
| 嵌套消息缩进 | 每行前 `"  "`（2 空格缩进），去除首尾空行 |
| ~~批次汇总 batch_agents~~ | **已移除**：`batch_agents` 字段与「N agents finished」汇总行不再存在（`TuiSubAgentGroup` 现为 `agent_id/agent_name/view_models/collapsed/is_running`，`tui_render_unit.rs`） |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

##### 系统消息 `SystemNote`

```
· 系统通知内容——————————————————————————
✻ 星号开头的版本信息—————————————————
⎿ 缩进开头的上下文信息———————————————
  ⎿ 错误消息内容————————————————————————
```

| 前缀 | 规格 |
|------|------|
| `✻` 开头行 | `dim` 色，无额外前缀 |
| `⎿` 开头行 | `muted` 色，无额外前缀 |
| 其余行 | `· ` 前缀（`dim`）+ 内容，颜色按 `TuiNoteLevel` 枚举（`render_system_note_lines`，`message_area/render.rs`）：Info → `muted`，Warning → `warning`，Error → `error` |

##### 缓存警告 `CacheWarning`

| 属性 | 规格 |
|------|------|
| 内容 | 纯文本整行，`warning` 色，**无前缀符号** |

##### AskUser 问答块 `AskUserBlock`

```
● User answered Peri's questions:
  ⎿ header → answer—————————————————————
```

| 属性 | 规格 |
|------|------|
| 标题 | `● User answered Peri's questions:`（`success`/`error`） |
| 结果行 | `"  ⎿ "`（`dim`）+ `header → answer`（`muted`/`error`） |
| 解析格式 | `[问: H]\n回答: V` |

##### 错误摘要行 `error_summary_lines`

| 属性 | 规格 |
|------|------|
| 前缀 | `"  ⎿ "`，`dim` 色 |
| 内容 | `error` 色，截断 400 Unicode 字符 |
| 多行 | 原样保留换行 |

---

#### 2.4.3 Diff 渲染

| 行类型 | gutter | 前景色 | 背景色 |
|--------|--------|--------|--------|
| 新增文件 | `+ path` | `diff_add` | `diff_add_bg` |
| 删除文件 | `- path` | `diff_remove` | `diff_remove_bg` |
| 修改文件 | `  path` | `muted` | 无 |
| Hunk `@@` | 整行 | `diff_hunk` `#578FA9` | — |
| Context | `{old:>n}  {new:>n} │ 内容` | `dim` gutter + 默认内容 | — |
| Add `+` | `+{empty:>n}  {new:>n} │ 内容` | `diff_add` `#3FB950` | `diff_add_bg` `#12341A` |
| Remove `-` | `-{old:>n}  {empty:>n} │ 内容` | `diff_remove` `#F85149` | `diff_remove_bg` `#371412` |

**Word Diff**：变更单词用更深色背景（`#1A4E24` / `#4E1C16`），不变部分用行级背景色。

**特殊规则**：
- 新文件最多显示 6 行内容，超出显示 `"... N more lines not shown"`（`dim`）
- 二进制文件：`"  Binary file path - cannot display diff"`（`dim`）
- 超长 diff：`"  Diff too large for path - changes not displayed"`（`dim`）
- 公共缩进裁剪：自动检测并移除所有内容行的公共前导空格
- Diff 行数据来自 `TuiToolCard.hunks`（`TuiHunkLineKind::Add/Del`，`tui_render_unit.rs`），渲染时直接生成，无独立 LRU 缓存

---

#### 2.4.4 消息区布局规格

| 属性 | 规格 |
|------|------|
| 消息区宽度 | `vis_width = inner.width - 1`（右侧 1 列留给滚动条） |
| 视口裁剪 | 二分查找 `wrap_map`（visual_to_logical）定位可见行，只 clone + 渲染视口内行（~60 行），避免 O(N×W) per render |
| 滚动上限 | 自持 `ScrollPos`（usize 偏移，`message_area/scroll.rs`），替代 ratatui-kit `ScrollViewState`（u16 上限 65535 视觉行） |
| 滚动跟随 | `follow_bottom` 粘性语义（`run_auto_follow`）：默认跟随底部；用户上滚即进入浏览态（follow=false），内容增长不再吸回；结构性事件强制滚底并恢复跟随——提交（`LOADING_EPOCH` 递增）、history 切换 // /clear（`BRIDGE_RESET_COUNTER` 递增 → `prev==0` 哨兵批量强制滚底）、resize 缩视口（vis_height 变化） |
| Resize 处理 | total_visual_rows 变化时 clamp offset 到有效范围；vis_height 变化时跟随态滚底（`prev_vis_height` 哨兵），浏览态不打扰 |

**滚动条**（右侧 1 列）：

| 元素 | 规格 |
|------|------|
| 滚动条体 | `muted` 色 |
| 滚动到底 ▼ | offset < max_scroll 时显示，`muted` + **BOLD** |
| 滚动到顶 ▲ | offset > 0 时显示，`muted` + **BOLD** |

**Sticky Header**：~~最后一条用户消息摘要吸顶~~ **已移除**（crashes-and-rendering 修复时 `show_sticky = false` 后未恢复，当前无 sticky header 渲染代码）。

**选区高亮**：
- 字符级高亮，背景色取主题 `semantic.selection`（设计参考值 `#264F78` 暗蓝）
- Unicode-safe（`char_indices()` 切割）
- 跨多 span 时拆分片段

---

#### 2.4.5 Todo 列表样式

| 状态 | 图标 | 图标样式 | 文字样式 |
|------|------|---------|---------|
| InProgress | `◼` | `accent` + **BOLD** | `text` |
| Completed | `✔` | `success` | `muted` + ~~CROSSED_OUT~~ |
| Pending | `◻` | `muted` | `muted` |

- 缩进 2 空格（`"  ◼"` / `"  ✔"` / `"  ◻"`）
- Todo 列表在 Spinner 下方不显示额外标题或分隔线
- 仅使用 `TodoItem.content` 字段渲染文本（`footer.rs`），Pending 项附加 i18n `msg-todo-available`（`(可开始)`）
- Todo 列表结束后插入 1 行空行

---

#### 2.4.6 前缀符号体系总览

| 符号 | 语义 | 位置 |
|------|------|------|
| `❯` | 用户消息头 | UserBubble 首行 |
| `●` | 工具调用头 / 聚合组头 / Agent 工具头 | ToolBlock / ToolCallGroup / Agent ToolCard 首行 |
| `◼` | Todo 进行中 | Todo InProgress |
| `✗` | 失败（Skill 形态） | Skill ToolCard 完成/失败标记 |
| `✔` | Todo 完成 | Todo Completed |
| `◻` | Todo 待处理 | Todo Pending |
| `·` | 系统消息 | SystemNote 普通行 |
| `⎿` | 结果/续行 | 工具结果行、错误摘要行、子 Agent 运行行、SystemNote 续行 |
| `▶` | 折叠 | SubAgentGroup 折叠摘要（`▶ N collapsed tools`） |
| `✳` | Spinner | Loading 动画 16 帧之一 |
| `▲` / `▼` | 滚动 | 滚动条顶部/底部按钮（ratatui Scrollbar begin/end symbol） |

> 注：`▸`/`▾` 折叠/展开箭头在 ratatui-kit 组件库中存在，但 TUI 消息渲染路径未使用。

---

#### 2.4.7 Spinner 动画帧

16 帧来回扫动画（100ms/帧，50ms raw tick 每 2 次推进 1 帧）：

向前：`✳ ✴ ✵ ✶ ✷ ✸ ✹ ✺ ✻ ✼ ❃ ❊`
向后：`✼ ✻ ✺ ✸`（第 12–15 帧为第 8–11 帧倒序，形成来回扫效果）

tick 对 16 取模选帧：`BRAILLE_FRAMES[tick % 16]`。

Spinner 帧颜色：`accent`（`#D77757` 暖橙）；辅助文本（elapsed、token count）：`muted`。

紧凑态（Compact 中）：颜色切换为 `thinking`

---

#### 2.4.8 设计哲学

1. **前缀分层**：`❯`（用户消息）> `●`（工具/聚合/Agent）> `·`/`⎿`（辅助信息），形成三级视觉缩进
2. **颜色语义化**：`success`=成功绿色、`error`=失败红色、`warning`=警告琥珀、`thinking`=思考蓝紫
3. **背景约束**：除 `user_bg` / `subagent_bg` / `popup_bg` / `cursor_bg` / `selection_bg` 外，不使用任何背景色
4. **空行去重**：`ensure_blank_line()` 保证相邻空行不重复
5. **流式友好**：Markdown 增量渲染 + 表格 holdback 策略


---

## 3. StatusBar 区域组件

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Auto · perihelion · anthropic/claude-code-sonnet                             │
│                 /::commands · Shift+Enter::newline · Ctrl+T::mode · Ctrl+O::diff│
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 第 1 行显示 permission mode、cwd basename、provider/model、后台任务计数。
- CPU%/MEM/上下文使用率显示在输入区 composer footer 右侧资源线（`input_area.rs` `footer_right`）。
- 第 2 行根据状态切换 hints：
  - 默认：slash commands hint + 输入区快捷键
  - popup 激活：弹窗快捷键（Esc: close、Enter: confirm）
  - @mention / slash 激活：补全导航快捷键（Esc: close、Tab: navigate、Enter: select）
- StatusBar 只保留 2 行；视觉缓冲由父布局 padding 提供，不作为 StatusBar 内部行。


---

## 4. BgTaskArea 后台任务区域

`BgTaskArea` 是 `AppShell` 根层组件（`kit/bg_task_area.rs`），位于 StatusBar 下方（屏幕最底部）。数据来自 `BG_DISPLAY` atom，由 `dispatch_and_notify`（`acp_events/system.rs` 快照/BgTask 事件 + `acp_events/tool.rs` bg 工具事件）写入。每行展示一个活跃的 bg subagent / bg shell / workflow 任务。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ AppShell root                                                                 │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ SessionColumn                                                                 │
│   MessageArea                                                                 │
│   PanelOverlay                                                                │
│   InputArea                                                                   │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ StatusBar：Auto · perihelion · anthropic/…                                    │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ BgTaskArea                                                                    │
│                                                                              │
│  ● coder  修改 TUI-PAGE.md                                  2min 15s         │
│  ✓ reviewer  审查 agent 模块                                     45s         │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### BgTaskArea 视觉规格

| 属性 | 规格 |
|------|------|
| 每行格式 | `状态符号 + agent_type + desc + 右侧耗时`，一行一个 agent |
| 状态符号 | `●`（running，有当前工具）、`◎`（idle，运行中无当前工具）、`✔`（completed）、`✗`（failed） |
| 状态色 | running → white，idle → yellow，completed → green，failed → red |
| agent_type | dim 灰色 |
| desc | 终端宽减去固定开销后 CJK 安全截断，超长尾部加 `…` |
| 耗时 | 右对齐，dim 灰色。格式 `Xs` / `XmXs` / `XhXm`；已完成显示总运行时长 |
| 空态 | 无活跃任务时高度收缩为 0 |
| 排序 | 活跃任务在前，已完成/失败在后 |
| 完成保留 | 3 秒后移除 |

能力：

- 每行展示一个后台任务的状态（名称、描述、运行或总耗时）。
- 运行中任务通过 `RENDER_HEARTBEAT` 持续更新耗时显示。
- bg agent 启动时通过 `BgTaskStarted` 事件添加条目（含 `created_at` 时间戳）。
- bg agent 完成/失败时通过 `BgTaskCompleted` / `BgTaskCancelled` 更新条目状态。
- `duration_since()` 使用 `safe_elapsed()` 安全包装，避免时钟倒流 panic。
- 空态不占用布局空间。


---

## 5. 相关 Issue 经验

> 本条目录自 `spec/archive-issues/` 归档（渲染相关：scroll/render/viewport/SystemNote/Markdown/copy/spinner），
> 供 `spec/global/problems.md` 关键词索引锚点跳转。归档原文见各归档文件。

### issue_2026-07-03-tui-double-slash-cpu-spike
**摘要:** TUI 输入区输入 // 导致 CPU 持续高负载
**状态:** Verified
**归档日期:** 2026-07-06
**关键词:** slash popup, render body 写 atom, SLASH_SELECTED_INDEX, 级联重渲染
**问题本质:** `SlashCompletion` 组件 render body 中写入 `SLASH_SELECTED_INDEX` atom；`slash_active` 从 true→false 过渡（输入 `//` 触发）时与组件卸载生命周期交互，引发级联重渲染导致 CPU 100%。
**通用模式:** render body 不得写 atom——事件处理器已用 `saturating_sub`/`min` 保持边界安全，渲染期只用只读 `clamp_selection` 裁剪显示，无需回写。
**涉及文件:** peri-tui/src/kit/slash_completion.rs, peri-tui/src/kit/input_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-04-message-area-scrollview-steals-input
**摘要:** 主输入框无法输入——MessageArea ScrollView 事件处理器消费所有键盘事件
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** ScrollView, EventScope Global/High, 事件消费, active=false
**问题本质:** MessageArea 注册 `EventScope::Global, EventPriority::High` 处理器消费所有键盘/鼠标事件；`ratatui-kit` ScrollView 内置 handler（Current/Normal 优先级）继续消费 `↑/↓/j/k/h/l/PageUp/PageDown/Home/End`，InputArea 收不到任何按键。
**通用模式:** 引入第三方滚动组件时须关闭其内置键盘 handler（`active: false`），滚动由显式按键匹配（仅 `Ctrl+↑↓HomeEnd`）接管；修复事件拦截要覆盖全部优先级层（Global/High + Current/Normal）。
**涉及文件:** peri-tui/src/kit/message_area/（原 message_area.rs，已目录化）
**CLAUDE.md 链接:** false

### issue_2026-07-05-message-area-crashes-and-rendering
**摘要:** 消息区多场景崩溃/白屏/滚动异常
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** u16 overflow, saturating_add, 双重滚动冲突, arboard 独立线程
**问题本质:** 视口裁剪与鼠标坐标换算多处 u16 加法溢出 panic（`scroll_y + vis_height` 等）；`Paragraph.scroll` 与 ScrollView 双重滚动冲突导致内容白屏；剪贴板写入阻塞 UI 线程导致复制崩溃。
**通用模式:** 终端坐标运算统一 saturating 语义；滚动只保留单一来源；剪贴板等 IO 调用移出 UI 线程（`std::thread::spawn`）。
**涉及文件:** peri-tui/src/kit/message_area/（原 message_area.rs，已目录化）, peri-tui/src/kit/text_selection.rs, peri-tui/src/kit/layout.rs（render_bridge.rs 已删除）
**CLAUDE.md 链接:** false

### issue_2026-07-05-mouse-move-cpu-spike
**摘要:** 鼠标晃动导致 CPU 暴涨
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** MouseMove, 高频事件, 提前忽略
**问题本质:** 消息区 Global 事件处理器消费所有 `Event::Mouse`（含 `MouseMove`），鼠标高频移动触发 `scroll_state` 写锁获取与 `auto_scroll.set(false)` state 写入。
**通用模式:** 高频无状态事件（鼠标移动）应在 handler 入口提前忽略（返回 `Ignored`），不走 state 读写/锁获取。
**涉及文件:** peri-tui/src/kit/message_area/（原 message_area.rs，已目录化）
**CLAUDE.md 链接:** false

### issue_2026-07-05-scroll-performance-lag
**摘要:** 长数据高速滚动时刷新卡顿/掉帧（现象 2：tmux 环境任意数据量滚动卡顿）
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 滚动节流, write_no_update, ScrollThrottle, 渲染风暴, tmux PTY
**问题本质:** 每个滚轮事件 handler 内多次 `scroll_state.write()` 产生多次原子通知 → 每个滚轮事件 4 次 `terminal.draw()`；tmux 下每次 draw 经 PTY 序列化开销被 4 倍放大。
**通用模式:** 高频滚动合并为单次 `write_no_update()` + 帧间隔节流（`ScrollThrottle` 16ms），由 loop 强制 render 读最新 atom 值；涉及 PTY 的终端环境（tmux）对每帧 draw 开销极其敏感。
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/message_area/mod.rs（原 message_area.rs；render_bridge.rs 已删除）
**CLAUDE.md 链接:** false

### issue_2026-07-06-enter-hello-cpu-spike
**摘要:** TUI 输入 hello 并 Enter 后 CPU 100%
**状态:** Verified
**归档日期:** 2026-07-06
**关键词:** footer spinner 自激, render 写 state, AgentDone→TurnDone
**问题本质:** `build_footer_lines` 在 loading 稳态下每次 render 都写 hook state（`was_loading`/`load_start`/`spinner_state`），形成 render→state write→render 自激循环；`AgentDone` 通知未转 `TurnDone` 导致输出结束后 spinner 残留。
**通用模式:** loading 态 footer 的状态写入只在状态跃迁（loading 变化/delta>0）时进行；结束边界事件（AgentDone）必须映射为 TurnDone 才能清 loading。
**涉及文件:** peri-tui/src/kit/message_area/footer.rs（原 message_area.rs，已目录化）, peri-tui/src/kit/acp_notifier.rs
**CLAUDE.md 链接:** false

### issue_2026-07-06-message-area-copy-complex-content-crash
**摘要:** Message Area 复制操作导致 TUI 崩溃/卡死
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** render body 写 atom, 自激回路, status_bar, COPY_MESSAGE_UNTIL
**问题本质:** `status_bar.rs` 在 render body 中写 `COPY_MESSAGE_UNTIL` atom（复制提示 2s 过期时置 None），触发 wake→render→write 自激回路——复制后滚动 1348 次无异常，恰好卡死在 2s 过期点；arboard 调用同时阻塞 tokio worker。
**通用模式:** 渲染层对超时提示只能只读判断（`now < until`），过期清理放到事件/effect 边界；外部库调用（剪贴板）必须 spawn 独立线程。同 issue_2026-07-03-tui-double-slash-cpu-spike（render body 写 atom 铁律）。
**涉及文件:** peri-tui/src/kit/status_bar.rs, peri-tui/src/kit/message_area/selection.rs（原 message_area.rs，已目录化）, peri-tui/src/kit/text_selection.rs
**CLAUDE.md 链接:** false

### issue_2026-07-06-panels-selection-no-scroll-follow
**摘要:** 面板选中项超出可见行后看不到（缺 scroll 跟随）
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** 面板列表, 选中跟随, scroll_start_for_selected
**问题本质:** 各面板列表（Mcp/Plugin/Hooks/Tasks/Cron/Memory/Betas/ThreadBrowser）用 `use_state(0)` + `>` cursor 渲染，选中项离开视口时无滚动跟随。
**通用模式:** 面板列表的选中项跟随滚动（`scroll_start_for_selected`）是通用需求，应在共享层收敛，避免各面板各自实现。
**涉及文件:** peri-tui/src/kit/panels/{mcp,plugin,hooks,tasks,cron,memory,betas,thread_browser}.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-message-area-scroll-proximity-follow
**摘要:** 消息区自动吸底应基于滚动位置就近判断，而非二元开关
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** auto_scroll 二元开关, 就近判断, 吸底
**问题本质:** `auto_scroll: bool` 二元开关下，用户任何滚动/点击/快捷键都置 false，整轮不再自动跟随；滚回底部也不恢复（曾提出 threshold = max(vis_height/2, 5) 就近判断，后续演进为 `follow_bottom` 粘性语义）。
**通用模式:** 吸底语义应从「用户操作开关」演进为「滚动位置状态」——视口在底部附近即跟随，离开即停止，滚回即恢复；结构性事件（提交/切换/resize）强制恢复。
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/message_area/mod.rs（原 message_area.rs，已目录化）
**CLAUDE.md 链接:** false

### issue_2026-07-09-history-session-switch-loading-freeze
**摘要:** History 面板切换 session 后 loading 永久卡死，界面完全无响应
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** _meta serde rename, is_session_replay, replay loading 卡死
**问题本质:** ACP SDK v1.4.0 的 `meta` 字段序列化为 `_meta`（含下划线），`acp_notifier` 的 `is_session_replay` 只用 `"meta"`/`"content.meta"` 提取，永远找不到 `periReplay=true` 标记 → replay 的 ToolStarted/TextChunk 设 `phase=PromptRunning`，全程无 TurnDone → loading 永久卡 true。
**通用模式:** 协议字段名以序列化 key 为准（serde rename 后的形态），解析方与序列化方必须使用同一 key；session replay 事件的 loading 生命周期必须显式终止。
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）, peri-acp/src/dispatch/session_replay.rs, peri-tui/src/kit/thread_load_consumer.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-message-area-periodic-white-flash-streaming
**摘要:** 消息区在 agent 流式回复中周期性闪白（每 2-5 秒）
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** 吸底 use_effect, render↔effect 紧耦合, 增量门控
**问题本质:** 流式期间吸底 `use_effect` 每帧 `scroll_to_bottom()` 写入 ScrollViewState atom，形成 render↔effect 紧耦合环路导致闪烁；poll tick 全量重建为误判，已回退。
**通用模式:** effect 内的高频写入需要增量门控（`last_scrolled_at`）+ 底部 guard，避免每帧都写 atom。
**涉及文件:** peri-tui/src/kit/message_area/mod.rs（原 message_area.rs，已目录化）
**CLAUDE.md 链接:** false

### issue_2026-07-10-bg-subagent-tool-count-always-zero
**摘要:** 后台 subagent 完成通知中的"工具调用"计数始终为 0
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** BackgroundTaskResult, tool_calls_count 硬编码, bg/fork 路径
**问题本质:** 4 处构造 `BackgroundTaskResult` 的路径（`execute_bg.rs` 错误/成功、`spawner.rs` 错误/成功）硬编码 `tool_calls_count: 0`，未统计真实工具调用数。
**通用模式:** 结果统计字段必须在所有构造路径（bg/fork × 成功/失败）一致填充；硬编码 0 是最隐蔽的错误形态。
**涉及文件:** peri-agent/src/agent/events.rs, peri-middlewares/src/subagent/tool/execute_bg.rs, peri-middlewares/src/subagent/spawner.rs
**CLAUDE.md 链接:** false

### issue_2026-07-10-brewed-summary-missing-in-empty-state
**摘要:** MessageArea 空态时不显示「✻ Brewed for Xm Xs」总结行
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Brewed 总结, 空态早退分支, has_summary 时序
**问题本质:** `has_summary` 检查在 mutation block 之前读到旧值（单帧延迟）；空态 Welcome 早退分支跳过 footer 渲染路径；`brewed_lines` 未判空（首次启动 footer 为空仍进入 Brewed 分支）。
**通用模式:** 空态分支提前 return 前必须完成所有共享状态更新——footer 行预计算必须在 empty 分支之前（hook 顺序稳定）；footer 数据与欢迎态解耦。
**涉及文件:** peri-tui/src/kit/message_area/footer.rs, peri-tui/src/kit/message_area/mod.rs（原 message_area.rs，已目录化）
**CLAUDE.md 链接:** false

### issue_2026-07-11-history-replay-scroll-too-early
**摘要:** History 恢复会话时 scroll_to_bottom 过早，布局未就绪导致滚动位置停在中间
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** history replay, 批量滚底, prev==0 哨兵
**问题本质:** history 恢复批次到达时布局尚未就绪即执行 `scroll_to_bottom()`，滚动位置停在中间；旧 `prev==0 && len>0` 分支已删除，proximity 检测阻止大距离吸底。
**通用模式:** 批量事件（replay）的滚底需要跨批次保持触发（`prev==0` 哨兵不消费，直到 generation 停止增长）；`BRIDGE_RESET_COUNTER` 作为会话切换的语义哨兵。
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/thread_load_consumer.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-message-area-mouse-selection-regression
**摘要:** 消息区鼠标拖拽选中复制功能因重构回归 + 鼠标拖拽 CPU 暴涨
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 重构回归, text_selection 死代码, RENDER_CACHE 依赖
**问题本质:** commit `3bfb9fff` 删除 render_bridge（含 `RENDER_CACHE.entries: Vec<Line>` + `wrap_map`）后，text_selection 的文本提取/选区渲染失去数据源依赖被 `#[allow(dead_code)]` 禁用，选区复制功能静默丢失。
**通用模式:** 删除渲染管线前必须确认数据结构的全部消费者（含文本选区/复制）已迁移；标注"后续独立补回"的已知风险要显式跟踪，不能停留在迁移计划备注里。
**涉及文件:** peri-tui/src/kit/message_area/selection.rs, peri-tui/src/kit/message_area/mod.rs（原 message_area.rs，已目录化）, peri-tui/src/kit/atoms.rs, peri-tui/src/kit/status_bar.rs（render_bridge.rs 已删除）
**CLAUDE.md 链接:** false

### issue_2026-07-12-message-area-copy-unicode-misalignment
**摘要:** 消息区拖拽复制时 Unicode 字符后段错位（越往后偏移越大）
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Unicode 宽度, visual_col_to_byte_offset, 视觉坐标换算
**问题本质:** 视觉坐标转字节偏移按「宽度×k」乘法假设换算，CJK 字符上累积偏移；`wrap_byte_starts` 用 `unicode_width` 模拟 `Paragraph::wrap` 得到每视觉行起始字节偏移。
**通用模式:** 终端视觉坐标与逻辑偏移的换算必须用 `unicode_width` 按字符宽度逐步模拟，禁止「宽度×k」乘法假设。
**涉及文件:** peri-tui/src/kit/message_area/selection.rs（原 message_area.rs，已目录化）, peri-tui/src/kit/text_selection.rs
**CLAUDE.md 链接:** false

### issue_2026-07-12-message-area-scrollbar-not-reaching-bottom
**摘要:** 消息区滚动不到最末尾（内容+滚动条均未到底）+ 宽度变化后滚动失效
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** vis_width 对齐, 滚动条 thumb, resize clamp
**问题本质:** `line_count(vis_width)` 用 `area.width-1` 估算 `total_visual_rows`，但主渲染分支用 `Constraint::Fill(1)` 占满 `area.width`，估算偏大 → thumb 永远在底部之上；resize 后 offset 超范围导致滚动失效。
**通用模式:** 滚动条估算宽度必须与实际渲染 wrap 宽度一致（`Constraint::Max(vis_width)`）；resize 后必须 clamp offset 到新 `max_scroll`。
**涉及文件:** peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/message_area/scroll.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-clear-scrollbar-persists-at-welcome
**摘要:** /clear 后回到 Welcome 页面，滚动条仍然可见
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** ScrollbarFields 未重置, 空态早退, 僵尸滚动条
**问题本质:** empty 分支提前 return Welcome 前未重置 `scrollbar_fields`，`ScrollbarHook::post_component_draw` 用旧会话的 `content_length > viewport_length` 渲染僵尸滚动条。
**通用模式:** 提前 return 分支必须重置所有在 return 后仍被 hook 消费的状态（hook 注册在分支之前，draw 回调每帧执行）。
**涉及文件:** peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/message_area/props.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-main-agent-done-loading-persists-bg-still-running
**摘要:** 主 agent 完成回复后 loading 不退，因后台 agent 仍在运行
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** TurnSuspended, SubagentStopped 覆盖, idle_should_wait
**问题本质:** bg agent 运行时 `idle_should_wait` probe 为 true，End 阶段产出 `TurnSuspended`（清 loading）；随后 `SubagentStopped` 事件无条件设 `phase=PromptRunning` 重新置 loading。
**通用模式:** 终止语义的时序（TurnSuspended→SubagentStopped）必须被事件处理器整体理解，不能单事件各自为政；loading 状态以「当前 turn 是否有活跃产出」为准。
**涉及文件:** peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）, peri-agent/src/agent/stages/mod.rs, peri-acp/src/session/executor.rs, peri-middlewares/src/subagent/spawner.rs, peri-middlewares/src/subagent/background.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-statusbar-context-cache-display-regression
**摘要:** 状态栏上下文消耗显示 + 消息流缓存命中率警告，ratatui-kit 迁移后全部丢失
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** CONTEXT_USAGE, CACHE_HIT_INFO, 迁移回归
**问题本质:** 迁移后状态栏上下文消耗与缓存命中率警告链路未恢复：`CONTEXT_USAGE`/`CACHE_HIT_INFO` atom、`inject_cache_warning`、`StateSnapshotMeta` 写入缺失。
**通用模式:** 跨架构迁移的功能核对要以 atom/事件链路为单位（写入方→atom→消费组件），不能只看组件 UI 是否渲染。
**涉及文件:** peri-tui/src/kit/status_bar.rs, peri-tui/src/kit/atoms.rs, peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）, peri-tui/src/kit/acp_bridge.rs, peri-agent/src/agent/agent_context.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-submit-no-scroll-to-bottom
**摘要:** 用户发送 prompt 后消息区不自动跳转到最底部
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** LOADING_EPOCH, 提交强制滚底
**问题本质:** `run_auto_follow` 的 `is_loading` 分支依赖 proximity 检测，无「用户主动提交」的强制滚底信号；提交瞬间 `VIEW_MODELS` 尚未包含 UserBubble（prompt RPC 飞行中），proximity guard 生效。
**通用模式:** 结构性用户动作（提交）需要独立于内容增长的强制滚底信号——`LOADING_EPOCH` 递增在 user bubble 到达前先定位到底部，后续流式自然跟随。
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/submit_consumer.rs
**CLAUDE.md 链接:** false

### issue_2026-07-14-inline-code-no-color
**摘要:** Markdown 行内代码无颜色渲染
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 行内代码, Modifier::DIM 哨兵, 上游 parser 行为
**问题本质:** `span_style.rs` 用 `Modifier::DIM` 哨兵检测行内代码，但上游 `ratatui-kit-markdown 0.3.0` 不设置该修饰符。
**通用模式:** 基于第三方库内部修饰符的状态检测会随上游行为漂移；依赖公开 API 或显式数据（segment 类型），并用测试固化预期行为。
**涉及文件:** peri-tui/src/kit/markdown/span_style.rs, peri-tui/src/kit/markdown/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-15-markdown-table-raw-text-streaming
**摘要:** Markdown 表格流式输出时显示为原始 pipe 格式
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 流式表格, has_potential_table_header, 缓存失效
**问题本质:** 流式期间表格 header 行（`| 原文 | 改为 |`）先到达，pulldown-cmark 因缺分隔符解析为 `Paragraph`；分隔符到达后 block 结构翻转为 `Table`，增量缓存 `can_reuse` 只比 block 数量未比类型 → 旧 Paragraph 的原始 pipe 文本永久残留。
**通用模式:** 增量解析缓存的 `can_reuse` 判定必须包含 block 结构稳定性检查（如以 `|` 开头的段落标记为潜在表头 `has_potential_table_header`，缓存失效），不能只比较数量。
**涉及文件:** peri-tui/src/kit/markdown/convert.rs（ConvertState）, peri-tui/src/kit/markdown/mod.rs（parse_markdown_cached）
**CLAUDE.md 链接:** false

### issue_2026-07-15-terminal-rapid-shrink-width-crash
**摘要:** 快速缩小终端宽度到极小值时程序直接退出崩溃
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 表格 buffer 越界, 列宽 redistribution, 极窄终端
**问题本质:** `compute_table_col_widths` 在 `available == 0` 返回每列最小 1 宽，但 `table_data_to_lines` 的 buffer 宽度被 `max_width` 钳制、列宽 `wa` 未同步缩减 → 按原始列宽写入 buffer 越界 panic（`index outside of buffer`）。
**通用模式:** 渲染 buffer 分配与列宽计算必须共享同一套钳制语义；极窄宽度（resize 竞态）是 buffer 越界的高发场景，需加回归测试。
**涉及文件:** peri-tui/src/kit/markdown/table.rs, peri-tui/src/kit/message_area/render_test.rs
**CLAUDE.md 链接:** false

### issue_2026-07-16-system-note-cache-warning-position-wrong
**摘要:** Cache 命中率警告 SystemNote 在消息流中位置错位——被积压到上一个 user/AI message 后面
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** SystemNote 时序, committed vs current_turn, 事件排序
**问题本质:** `SystemNotification`/`BudgetWarning` 直接 push 到 `state.committed`（持久化队列），绕过 `current_turn` 的 `TurnSegment` 分段；`push_view_models` 按 `committed + current_turn.view_models()` 拼接 → SystemNote 永远排在所有 AI 内容之前，丢失时序位置。
**通用模式:** 渲染顺序由 committed + current_turn 的拼接语义决定，任何「插入到流中间」的消息必须进入 current_turn 分段而非持久化队列。
**涉及文件:** peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）, peri-tui/src/kit/acp_notifier.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-spinner-tick-decouple-from-acp-bridge
**摘要:** Spinner 帧推进绑定 acp_bridge 1s tick，应改为 TUI 独立 tick
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** spinner tick, RENDER_HEARTBEAT, 独立渲染循环
**问题本质:** spinner 帧推进绑定 acp_bridge 的 1s `tick_interval`（`acp_bridge.rs`），事件稀少时动画按 1s 粒度跳变；渲染心跳与业务事件流未解耦。
**通用模式:** 动画/计时类渲染需要独立于业务事件流的 tick 源（`RENDER_HEARTBEAT` atom），业务空闲时仍按帧率推进（50ms raw tick，2 次推进 1 帧）。
**涉及文件:** peri-tui/src/kit/atoms.rs（RENDER_HEARTBEAT）, peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/message_area/footer.rs, peri-tui/src/components/spinner/animation.rs（原 components/spinner/mod.rs）, peri-tui/src/kit/entry.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-system-note-level-color-not-rendered
**摘要:** SystemNote 的 Warning/Error 等级字体颜色未区分，全部显示为灰色
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** TuiNoteLevel, 启发式 vs 枚举, render_system_note_lines
**问题本质:** `render_system_note_lines` 颜色决策基于文本关键词（❌/⚠ 启发式）而非 `data.level`；`TuiNoteLevel` 已定义 Info/Warning/Error 三级但未被渲染使用（创建端 `acp_events` 多处已正确设置 level 字段）。
**通用模式:** 已有语义枚举（level）时渲染必须消费枚举，文本启发式是信息丢失源；创建端已正确设置说明数据流是通的，问题只在消费端。
**涉及文件:** peri-tui/src/kit/message_area/render.rs, peri-tui/src/kit/tui_render_unit.rs, peri-tui/src/kit/acp_events/（原 acp_events.rs，已目录化）
**CLAUDE.md 链接:** false


---

> [返回总索引](tui-index.md)
