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
│ │ @ mention files    / commands                                           │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Auto · perihelion · anthropic/claude-code-sonnet · CPU 12% · MEM 430MB        │
│                 /::commands · Shift+Enter::newline · Ctrl+T::mode · Ctrl+O::diff│
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 聚合展示对话、工具调用、工具结果、SubAgent、后台 Agent 状态、系统通知和当前 streaming turn。
- 输入区支持多行编辑、历史、文件 mention、slash command、软换行、视口跟随、placeholder。
- 状态栏持续暴露运行环境、权限模式、模型、资源占用和上下文快捷键。
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

- 统一渲染 TUI 内部类型（`TuiRenderUnit` 8 变体），包括文本、工具、SubAgent、系统事件等。
- 使用 bubbles 组件族（UserBubble/AssistantBubble/ToolCard/SystemNote/SubAgentGroup/CollapsedGroup/ReasoningBlock）进行渲染。
- ratatui-kit-markdown 做 Markdown 解析 + 代码高亮（通过 PaletteProvider 接入 Theme System）。
- Vec<Line> 缓存（`LinesCache` generation 增量检测）+ 单 Paragraph 渲染，消除每帧 N 个 widget 树开销。
- 支持 diff 可见性切换，diff 内容自动使用增删行语义色。
- 鼠标滚轮滚动消息区；键盘 Up/Down 保留给输入区。

### 2.3 Loading Spinner + Todo

TUI loading 统一使用 `peri-widgets/src/spinner`，包括 `SpinnerState` / `SpinnerMode` / `SpinnerWidget`。禁止在 MessageArea 中手写独立 loading 文案或自造 spinner。

**架构（v2.1）**：LoadingFooter 作为 MessageArea 的固定子区域，位于 ScrollView 之外、消息流底部。不随消息区滚动，空态时显示灰色 Brewed 总结行（不复位为 0 高度）。数据流：`ACP_STATE.is_loading` + `TODO_ITEMS` atom → 每轮渲染按壁钟时间补偿步进（once 门控防 tight loop）。

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ MessageArea（ScrollView 可滚动）                                              │
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
│   ✔ 已完成  阅读 peri-widgets spinner                                        │
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
- Todo 文本优先显示每项的 `activeForm`；若缺失再显示 `content` 的短文本。
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
| system_reminder | 仅渲染 `📋 Context compacted`（`dim` 色，*ITALIC*，无前缀/无底色） |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

##### AI 回复 `AssistantBubble`

```
AI 回复的 Markdown 内容段落，由 Markdown 渲染器处理。————————
段落之间由空行分隔。——————————————————————————————————————

代码块自动语法高亮：
  code example here

▍ 这是引用块内容，前缀 ▍ 可嵌套多级
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
| **引用块** | `▍ ` 前缀（`muted` 色），嵌套 `quote_depth` 次，前后各 1 空行 |
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
- `LinesCache`（generation 增量检测）+ 单 `Paragraph` 渲染，消除每帧 N 个 widget 树开销
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
| 预览行 | `" ⎿ "` 前缀（`dim`）+ 尾部内容（`dim`），最多 3 行 |
| 折叠逻辑 | 默认折叠，仅显示首行和预览行 |
| message_id 透传 | reasoning chunk 携带 `message_id`，按段分配切片 |
| 空行 | 首尾各加一个空行，保证与相邻消息块的间距 |

##### 工具调用 `ToolBlock`

```
● tool_name (参数摘要)———————————————
  ⎿ 工具执行结果内容———————————————————
```

| 状态 | 指示器 | 颜色 | 动画 |
|------|--------|------|------|
| Running | `●` | `success` | 800ms 切换（`●` ↔ 空格），1600ms 完整周期 |
| Completed | `●` | `success` | 固定 |
| Failed | `✗` | `error` | 固定 |

| 属性 | 规格 |
|------|------|
| 工具名 | `text` 色，**BOLD**，经过 `format_tool_name()` 映射显示名 |
| 参数摘要 | `" (summary)"`，`dim` 色，截断 400 Unicode 字符 |
| 结果前缀 | `"  ⎿ "`，正常态 `dim` 色，错误展开态 `error` 色 |
| 结果内容 | 正常 `muted`，错误 `error` |
| 错误摘要（折叠时） | `"  ⎿ "`（`dim`）+ 错误内容（`error`），截断 400 字符 |
| 折叠/展开 | 默认折叠只读工具（Read/Glob/Grep/AskUserQuestion） |
| Write/Edit | 完成后**强制展开** |
| Diff 视图 | 内嵌 diff 行，默认关闭，Ctrl+O 切换 |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

**工具显示名映射表** (`format_tool_name`)：

| 工具 | 显示名 |
|------|--------|
| Bash | Shell |
| Read | Read |
| Write | Write |
| Edit | Edit |
| Glob | Glob |
| Grep | Grep |
| folder_operations | Folder |
| TodoWrite | Todo |
| AskUserQuestion | Ask |
| Agent | Agent | Agent ToolCard 同时显示 tool calls count + running duration |
| LSP | LSP |
| artifact | ArtUp |
| WebSearch | Research |
| WebFetch | Browse |
| AgentResult | SubAgent | 后台 agent 结果，自动展开 |
| 其他 | PascalCase 转换 |

**工具参数摘要规则** (`format_tool_args`)：

| 工具 | 提取字段 | 截断 |
|------|---------|------|
| Bash | `command` | 400 字符 |
| Read/Write/Edit | `file_path`（相对化） | 不截断 |
| Glob/Grep | `pattern`（相对化） | 200 字符 |
| folder_operations | `operation path` | 不截断 |
| WebSearch/WebFetch | `query` / `url` | 60 字符 |
| ExecuteExtraTool/SearchExtraTools | `tool_name` / `query` | 40 字符 |
| AgentResult | `task_id` | 12 字符 |
| artifact | `file_path`（相对化） | 不截断 |
| LSP | `operation` | 40 字符 |

**自动展开规则** (`should_auto_expand_tool`)：
- `AgentResult`（后台 agent 结果）：自动展开
- `ExecuteExtraTool`（deferred 工具包装）：自动展开
- 错误结果不自动展开

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

**主 Agent 工具卡片（Agent ToolCard）**：
```
● Agent(agent_id) 任务预览内容…————————————————
  N tool calls, running Xmin Xs
```

**折叠态**：
```
  嵌套消息首行内容——————————————————————————————
```

**展开态**：
```
  嵌套消息首行内容——————————————————————————————
  嵌套消息续行内容——————————————————————————————
    ⎿ 最终结果内容—————————————————————
```

| 属性 | 规格 |
|------|------|
| 工具调用指示器 | `●`，`success` 色，动画同 ToolBlock 规则（Running 态 800ms 闪烁） |
| 主行 | `Agent(agent_id)`（正常 `success`，错误 `error`，后台运行 `warning`）+ 任务预览（`muted`，截断 50 字符） |
| 工具计数+耗时 | 第二行 `"  N tool calls, running Xmin Xs"`（`muted`），与 SubAgent 组的 child 数量配对 |
| 后台 Agent 短 hash | `#hash`（后台 agent），`muted` 色 |
| ~~❯ Agent header~~ | **已移除**。Agent 工具使用统一的 `●` 前缀（与 ToolBlock/聚合组一致） |
| 嵌套消息缩进 | 每行前 `"  "`（2 空格缩进） |
| 最终结果行 | `"  ⎿ "`（`dim`）+ 第一行内容（`muted`），截断 80 字符 |
| 前空行 | 1 行 |
| 后空行 | 1 行 |

**批次汇总** (`batch_agents` 非空)：

| 汇总行 | `● N agents finished`（`success`）/ `failed`（`error`）/ mixed |
|--------|------|
| 折叠态子行 | `├─`/`└─` 树形连接符（`dim`）+ task_preview（`text`）+ `· N tool uses`（`dim`）+ `· Done/Failed` |
| 展开态追加 | `"     ⎿ "` + final_result（`muted`） |

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
| 其余行 | `· ` 前缀（`dim`）+ 内容：自动检测 `❌`/失败/错误 → `error`，`⚠`/已中断 → `warning`，其他 → `muted` |

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
- 渲染缓存：LRU 容量 64，key = (old_hash, new_hash, flags, width)

---

#### 2.4.4 消息区布局规格

| 属性 | 规格 |
|------|------|
| 消息区宽度 | `inner.width - 1`（右侧 1 列留给滚动条） |
| 视口裁剪 | 二分查找 `wrap_map` 定位可见行，只克隆视口内数据 |
| 滚动跟随 | 默认跟随底部，用户手动滚离时取消（`scroll_follow = false`）。吸底自动跟随阈值 `max(5, vis_height/4)`。history load 时 entries_len 从 0→N 强制 scroll_to_bottom()。 |
| 缩放去抖 | 记录 `last_resize_width`，防止 N 次/秒 resize 重渲染 |

**滚动条**（右侧 1 列）：

| 元素 | 规格 |
|------|------|
| 滚动条体 | `muted` 色 |
| 滚动到底 ▼ | offset < max_scroll 时显示，`muted` + **BOLD** |
| 滚动到顶 ▲ | offset > 0 时显示，`muted` + **BOLD** |

**Sticky Header**（仅 `max_scroll > 0` 时渲染）：
- 显示最后一条用户消息的摘要
- 前缀 `❯`（`accent`，**BOLD**）+ 底色 `user_bg`
- 自动换行 + 截断

**选区高亮**：
- 字符级高亮，背景色 `selection_bg` `#264F78`
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
- 仅使用 `item.content` 字段渲染文本
- Pending 项可选附加 `(可开始)` 提示
- Spinner 下方可选显示 `"  ⎿  Tip: "` 提示行
- Todo 列表结束后插入 3 行空行

---

#### 2.4.6 前缀符号体系总览

| 符号 | 语义 | 位置 |
|------|------|------|
| `❯` | 用户消息头 | UserBubble 首行 |
| `●` | 工具调用头 / 聚合组头 / Agent 工具头 | ToolBlock / ToolCallGroup / Agent ToolCard 首行 |
| `◼` | Todo 进行中 | Todo InProgress |
| `✗` | 工具失败 | ToolBlock 首行 |
| `✔` | Todo 完成 | Todo Completed |
| `◻` | Todo 待处理 | Todo Pending |
| `·` | 系统消息 | SystemNote 普通行 |
| `⎿` | 结果/续行 | 工具结果行、错误摘要行、子 Agent 结果、SystemNote 续行 |
| `▍` | 引用块 | Markdown 引用前缀 |
| `├─` / `└─` | 树形连接 | SubAgent 批次汇总 |
| `✳` | Spinner | Loading 动画 16 帧之一 |
| `▲` / `▼` | 滚动 | 滚动条顶部/底部按钮 |

> 注：`▸`/`▾` 折叠/展开箭头在 `peri-widgets` 组件库中存在，但 TUI 消息渲染路径未使用。

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

## 4. StatusBar 区域组件

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Auto · perihelion · anthropic/claude-code-sonnet · CPU 12% · MEM 430MB        │
│                 /::commands · Shift+Enter::newline · Ctrl+T::mode · Ctrl+O::diff│
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 第 1 行显示 permission mode、cwd basename、provider/model、CPU、MEM。
- 第 2 行根据状态切换 hints：
  - 默认：slash commands hint + 输入区快捷键
  - popup 激活：弹窗快捷键（Esc: close、Enter: confirm）
  - @mention / slash 激活：补全导航快捷键（Esc: close、Tab: navigate、Enter: select）
- StatusBar 只保留 2 行；视觉缓冲由父布局 padding 提供，不作为 StatusBar 内部行。


---

## 5b. BgTaskArea 后台任务区域

`BgTaskArea` 是 `AppShell` 根层组件，位于 StatusBar 下方（屏幕最底部）。数据来自 `BG_DISPLAY` atom，由 `dispatch_and_notify` 在 bg 任务事件时写入。

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
| 状态符号 | `●`（running），`✓`（completed），`✗`（failed） |
| 状态色 | running → white，completed → green，failed → red |
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

> [返回总索引](tui-index.md)
