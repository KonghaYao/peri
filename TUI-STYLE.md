# TUI Style Guide

## 设计哲学

中性灰层级 + Claude 暖橙品牌色。背景透明（弹窗光标行和用户消息区除外）。信息层级用亮度区分（TEXT/MUTED/DIM），颜色表达状态语义。

## 色板

源码：`peri-tui/src/ui/theme.rs`（业务常量）、`peri-widgets/src/theme/presets.rs`（DarkTheme trait 实现）。

### 强调色

| 名称 | 色值 | 用途 |
|------|------|------|
| ACCENT | `#D77757` | Claude 暖橙品牌色：用户消息前缀、Welcome Logo、激活边框、光标、关键操作、输入框提示符 |

### 功能色

| 名称 | 色值 | 用途 |
|------|------|------|
| SAGE | `#4EBA65` | 明亮绿：工具调用指示器（⏺）、SubAgent、对勾标记 |
| WARNING | `#FFC107` | 明亮琥珀：次要强调、Markdown 标题、重试状态 |
| ERROR | `#FF6B80` | 明亮红：错误/拒绝、失败工具结果边框、系统错误消息 |
| THINKING | `#A2A9E4` | 标准紫：面板选中行、面板标题、/model 面板光标、Config 编辑高亮 |
| LOADING | `#93A5FF` | 浅蓝紫：Loading spinner、Auto Mode 权限标签 |
| BASH_BORDER | `#FD5DB1` | 粉红：Bash 工具结果边框 |
| MODEL_INFO | `#A0825F` | 棕金：状态栏模型名（不抢眼） |
| TOOL_NAME | `= SAGE` | 语义别名：工具名展示色 |
| SUB_AGENT | `= SAGE` | 语义别名：SubAgent 展示色 |

### 文字层级（三级亮度）

| 层级 | 色值 | 用途 |
|------|------|------|
| TEXT | `#FFFFFF` | 纯白：主文字、AI 回复前缀、Running 指示器、展开工具组标题 |
| MUTED | `#999999` | 浅灰：次要文字、标签、路径、工具参数、聚合摘要、Spinner 辅助信息 |
| DIM | `#505050` | 深灰：极弱文字、占位、已完成项、分隔符、折叠工具组参数 |

### 底色

| 名称 | 色值 | 用途 |
|------|------|------|
| USER_BG | `#373737` | 用户消息底色（所有行带底色，与 sticky header 一致） |
| POPUP_BG | `#000000` | 纯黑弹窗底色 |
| CURSOR_BG | `#262626` | 中性暗灰（光标行背景，列表选中行） |
| SELECTION_BG | `#264F78` | 文本选区背景色（深色主题下网页默认选中蓝的暗色版本） |
| SUB_AGENT_BG | `#1E1E26` | SubAgent 嵌套消息背景色（比终端背景略亮，形成视觉容器） |

### 边框

| 名称 | 色值 | 用途 |
|------|------|------|
| BORDER | `#505050` | 中性灰空闲边框、标准面板边框 |
| BORDER_ACTIVE | `= ACCENT` | 激活边框：输入框/panel focus 状态、多 session 活跃列 |
| BORDER_DIM | `#2A2A30` | 非活跃 session 分隔线 |

### Diff 高亮色（diff 内容自动检测时）

| 名称 | 色值 | 用途 |
|------|------|------|
| DIFF_ADD | `#6EB56A` | 绿色：`+` 添加行 |
| DIFF_REMOVE | `#CC463E` | 红色：`-` 删除行 |
| DIFF_HUNK | `Cyan` | 青色：`@@` 上下文标记行 |

## Markdown 渲染

源码：`peri-widgets/src/markdown/`。通过 `DefaultMarkdownTheme` 参数化：

| 元素 | 颜色 | 说明 |
|------|------|------|
| 标题 (H1-H3) | WARNING + BOLD | `#FFC107` |
| 正文 | TEXT | `#FFFFFF` |
| 行内代码 | ACCENT | `#D77757` |
| 代码块 | TEXT 前景色 + SAGE `│` 前缀 | 多行代码块有行前缀，单行代码块无前缀 |
| 链接 | SAGE | `#4EBA65` |
| 引用块 | MUTED `▍` 前缀 | `#999999` |
| 列表符号 | TEXT `•` | `#FFFFFF` |
| 水平线 | MUTED `─` | `#999999` |
| 表格 | BOX 绘制边框 (┌├└─│) | CJK 字符自动对齐 |
| 加粗 | BOLD 修饰 | |
| 斜体 | ITALIC 修饰 | |

当 AI 回复内容被检测为 diff 格式时（前 5 行含 `@@` 或 `+++`），自动切换 diff 着色覆盖 markdown 渲染。

## 消息流

源码：`peri-tui/src/ui/message_render.rs`。

### 消息类型与视觉

| 类型 | 前缀 | 前缀色 | 说明 |
|------|------|--------|------|
| 用户消息 | `❯` | ACCENT + BOLD | USER_BG 底色，所有行带底色 |
| AI 回复 | `●` | TEXT (纯白) | 首行文本合并到 `●` 后，支持 markdown 渲染 |
| 思考 (Reasoning) | — | — | 不在消息流中渲染，完全隐藏 |
| 工具调用 | `⏺` | SAGE (绿) | 工具名 TEXT + BOLD，参数 DIM `(...)` |
| 工具聚合组 | `⏺` | SAGE (绿) 前缀 + MUTED 汇总文字 | 仅一行汇总文本，不可展开 |
| AskUserQuestion | `⏺` | SAGE (绿) | 标题 `User answered Peri's questions:` + `⎿ · header → answer` |
| SubAgent | `●` | SAGE / ERROR | 折叠：名称 + 任务预览；展开：嵌套消息 + 执行结果 |
| 系统消息 | `·` | DIM | 自动检测错误/警告/信息颜色 |

### 间距规则

- 每条有内容的消息后加 **1 个空行**，由 `render_one` 统一管理
- 空内容消息（如纯思考被隐藏的 AssistantBubble）不渲染、不占位
- 消息内部不插入多余空行
- SubAgent 展开时用空行分隔嵌套消息与结果区

### 工具状态指示器

指示器 `⏺` 按状态变化，工具名称统一 TEXT + BOLD，参数用 DIM 色 `(...)` 显示：

| 状态 | 指示器 | 指示器颜色 | 工具名颜色 |
|------|--------|-----------|-----------|
| Running | `⏺` 闪烁（200ms 周期，⏺/空格交替） | SAGE | TEXT + BOLD |
| Completed | `⏺` | SAGE | TEXT + BOLD |
| Failed | `✗` | ERROR | TEXT + BOLD |

工具结果行格式：` ⎿ ` 前缀（DIM 色）+ 内容（MUTED 色或 ERROR 色）。错误折叠时显示 error_summary（最多 400 Unicode 字符，DIM `⎿` 前缀）。

### AskUserQuestion 渲染

专用渲染路径，独立于普通 ToolBlock：

```
⏺ User answered Peri's questions:
  ⎿ · header → answer
  ⎿ · header2 → answer2
```

- 标题行：`⏺` (SAGE) + 标题文字 (TEXT)
- 结果行：`⎿` (DIM) + `·` (DIM) + `header → answer` (MUTED)
- 解析工具输出 `[问: H]\n回答: V` 格式，重新格式化为 `H → V`
- 错误态：指示器和标题使用 ERROR 色

## 只读工具聚合折叠

read_file、search_files_rg、glob_files 等只读工具自动聚合：

- **相邻的同类型工具**合并为一组（无其他消息穿插时）
- 仅显示一行汇总：`⏺ Read 3 files`（⏺ SAGE + 文字 MUTED），不可展开
- 出错工具即使在折叠态也显示 error_summary（ERROR 色）

摘要格式：

| 工具 | 单数 | 复数 |
|------|------|------|
| read_file | Read 1 file | Read N files |
| search_files_rg | Searched for 1 pattern | Searched for N patterns |
| glob_files | Matched 1 pattern | Matched N patterns |

## SubAgent 渲染

- 折叠：`● agent_id`（SAGE + BOLD）+ `task_preview…`（MUTED，截断 50 字符）
- 展开：名称 + 任务描述 + 缩进 2 空格的嵌套消息 + 空行 + 结果行 ` │ `（MUTED，最多 20 行，超长截断 80 字符 + `…`）
- 错误态：前缀和名称使用 ERROR 色，显示 error_summary

## Welcome Card

源码：`peri-tui/src/ui/welcome.rs`。空消息时垂直+水平居中显示：

| 区域 | 样式 |
|------|------|
| ASCII Art Logo (>=50 cols) | ACCENT + BOLD，6 行 `███╗` 风格 |
| 窄屏标题 (<50 cols) | ACCENT + BOLD `Peri` |
| 副标题 | MUTED `Peri Agent Framework` |
| 分隔线 | DIM `────── What can I do? ──────` |
| 功能亮点 | ACCENT ` • ` + TEXT 内容 |
| 命令提示 | WARNING `/model` 等 + MUTED 间距 |
| 首次引导（无 Provider 时） | WARNING ` ▶ ` + BOLD + TEXT `/login` |
| 快捷键提示 | DIM 统一显示 |
| Provider/模型信息 | ACCENT ` ⚡ ` + TEXT |
| Skills 计数 | WARNING `#` + TEXT |

## Spinner

源码：`peri-widgets/src/spinner/`。位于消息区域底部：

| 模式 | 显示格式 | 颜色 |
|------|---------|------|
| Loading | `✻ verb (Xm Xs · ↓ X.Xk tokens)` | ACCENT（compact 时 THINKING） |
| 完成 | `✻ Brewed for Xm Xs` | MUTED |

动画帧：16 个 Unicode 符号循环（`✳✴✵✶✷✸✹✺✻✼❃❊…`），每 200ms 推进一帧。
Verb 列表：128 个中文烹饪/动作动词随机选择。
Spinner 下方附加 Tip 行：`⎿  Tip: ...`（MUTED 色）。

## Todo 列表

在 loading 状态时跟在 Spinner + Tip 之后显示：

| 状态 | 图标 | 图标样式 | 文字样式 |
|------|------|---------|---------|
| InProgress | `◼` | ACCENT + BOLD | TEXT |
| Completed | `✔` | SAGE | MUTED + CROSSED_OUT |
| Pending | `◻` | MUTED | MUTED + `(可开始)` 提示 |

## 布局

### 垂直布局（单 Session）

```
┌─────────────────────────────────────┐
│ Sticky Header (动态高度)            │  ← 最后一条用户消息摘要
│ Messages Area (Min(1))              │  ← 消息列表 + 滚动条 + Spinner
│ Attachment Bar (0/3 行)             │  ← 有附件时 3 行
│ Panel / Popup (0~60% 屏幕高度)      │  ← 面板/弹窗区
│ Queued Messages (0~3 行)            │  ← loading 时待发送消息预览
│ Input Area (3~40% 屏幕高度)         │  ← 输入框 + `❯` 提示符
│ Status Bar (3 行固定)               │  ← 状态信息
└─────────────────────────────────────┘
```

### 面板高度

| 面板类型 | 最大高度 |
|---------|---------|
| 标准面板 | 屏幕高度 60% |
| 插件面板 | 屏幕高度 70% |
| HITL 审批 | `items.len() * 2 + 5` 行 |
| AskUser | 自适应（考虑文字换行） |
| OAuth | 9 行固定 |

## 状态栏

源码：`peri-tui/src/ui/main_ui/status_bar.rs`。3 行高度。

### 第一行（左→右）

| 元素 | 样式 | 条件 |
|------|------|------|
| 权限模式标签 | 按模式变色，切换后 3 秒 BOLD + SLOW_BLINK | 非 Default 时显示 |
| ` │ ` 分隔符 | MUTED | 始终 |
| `📁 cwd` | MUTED | 始终 |
| 模型名 | MODEL_INFO，切换后 3 秒 BOLD + SLOW_BLINK | 始终 |
| `│ ctx: X% (XK/XK)` | SAGE(<70%) / WARNING(70-85%) / ERROR(>=85%) | 有上下文数据时 |
| `│ ⟳ 重试 N/M (Xs)` | WARNING | 重试中 |
| `│ MCP (N/M)...` | MUTED(初始化中) / SAGE(ready, 3秒) / ERROR(failed) | MCP 有配置时 |
| `│ ⏱ Xm Xs` | MUTED | 仅 loading 时 |

### 第二行

- **左侧**：复制成功提示(MUTED) / 后台任务数(WARNING `[BG: N]`) / Agent 名称(MUTED)
- **右侧**：快捷键提示，上下文感知切换

右侧快捷键上下文：

| 上下文 | 快捷键 |
|--------|--------|
| 面板打开 | 面板自提供 `status_bar_hints()` |
| 多 Session | `/` 命令 + `Ctrl+N/P` 切换 + `Ctrl+W` 关闭 |
| OAuth 弹窗 | `Ctrl+O` 打开浏览器 + `Enter` 提交 + `Esc` 取消 |
| Approval 弹窗 | `↑↓` 移动 + `Space` 切换 + `Enter` 确认 |
| Questions 弹窗 | `Tab` 切换 + `↑↓` 移动 + `Space` 选择 + `Enter` 确认 |
| 退出确认 | `Ctrl+C` 关闭 + 其他键取消 |
| 默认 | `/` 命令 + `Alt+Enter` 换行 |

按键 MUTED + BOLD，说明 MUTED。右侧右对齐，超宽时截断右侧。

### 第三行

空行，视觉缓冲。

## 面板系统

源码：`peri-tui/src/ui/main_ui/panels/`。基于 `PanelManager` + `PanelComponent` trait 组件化架构。

### 面板列表

| 命令 | 面板 | 作用域 | 边框 |
|------|------|--------|------|
| `/login` | LoginPanel | Session | Browse=BORDER, Edit/New/Delete=动态色 |
| `/model` | ModelPanel | Session | BORDER |
| `/agents` | AgentPanel | Session | BORDER |
| `/history` | ThreadBrowserPanel | Session | BORDER |
| `/cron` | CronPanel | Session | BORDER |
| `/config` | ConfigPanel | Session | Browse=BORDER, Edit=WARNING |
| `/memory` | MemoryPanel | Session | BORDER |
| `/hooks` | HooksPanel | Session | BORDER |
| `/mcp` | McpPanel | Session | BORDER |
| `/status` | StatusPanel | Session | BORDER |
| `/plugin` | PluginPanel | Global | BORDER |
| `/cost` | StatusPanel (Cost tab) | Session | BORDER |
| `/context` | StatusPanel (Context tab) | Session | BORDER |

### 选中行样式

所有面板/弹窗的选中行统一使用 `fg(THINKING) #A2A9E4` 标准紫文字，**不使用底色**。光标指示器使用 `❯` 符号（THINKING 色）。/model 面板例外：选中项（✔）标签使用 SAGE 绿色，优先级高于紫色。

### 边框颜色规则

| 类型 | 边框颜色 | 面板 |
|------|----------|------|
| 标准面板 | BORDER (`#505050`) | model, agent, cron, thread_browser, config, memory, hooks, mcp, status |
| 编辑态 | WARNING | login(Edit/New/Delete), config(Edit) |
| 引导流程 | ACCENT | setup_wizard 步骤 1-2（Done=SAGE） |
| 警告弹窗 | WARNING | HITL 审批 |
| 提问弹窗 | THINKING | AskUser |
| OAuth | BORDER | oauth |

### 面板标题

统一 `BorderedPanel` 组件，标题 THINKING + BOLD，格式 `" 标题 "` 左右留空。

### 快捷键提示位置

所有面板的快捷键提示统一放在列表/表单**底部**，状态栏第二行右侧通过 `status_bar_hints()` 自描述。

### /model 面板样式

左右分栏 Profile 编辑器：左侧 4 个固定档位卡片（fable / opus / sonnet / haiku），右侧当前 Profile 的 K/V 编辑行。

```
┌─ Model ─────────────────────────────────────────────────────────────────────┐
│                                                                              │
│   Profiles                         Provider                         openai    │
│  ─────────────────────             Model                    gpt-5.6-luna    │
│  ● fable · openai                  Effort                              high    │
│    gpt-5.6-luna high               Max tokens                        32000    │
│    high · 1m                       1m enable                            off    │
│                                                                              │
│  ○ opus · anthropic                                                        │
│    claude-opus-4-6                                                          │
│    max · 200k                                                               │
│                                                                              │
│  ○ sonnet · openai                                                          │
│    gpt-5.6-luna xhigh                                                       │
│    xhigh · 1m                                                               │
│                                                                              │
│  ○ haiku · anthropic                                                        │
│    claude-haiku-4-5                                                         │
│    medium · 200k                                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

| 元素 | 颜色 |
|------|------|
| 边框 | BORDER (`#505050`) |
| 标题 "Model" | THINKING + BOLD |
| 左侧 active profile（`●` + 标题行） | SAGE + BOLD |
| 左侧普通 profile（`○`） | TEXT |
| 模型名主色 | MODEL_INFO |
| 模型名内嵌 effort 后缀 | model_accent + BOLD |
| 摘要 effort 值 | effort + BOLD |
| `200k`/`1m` 窗口标识 | token_context |
| 摘要间隔 `·` | MUTED |
| 右侧 K/V key（焦点行） | THINKING + BOLD |
| 右侧 K/V key（普通行） | MUTED |
| 右侧 K/V value | TEXT |
| 底部导航提示 | DIM |

### /memory 面板

每行格式：`❯ [✓] label    ...path`

- 存在：`✓` SAGE；不存在：`✗` MUTED
- 不存在 + 光标行：额外提示 `按 Enter 创建并编辑`（MUTED）

### /mcp 面板

- 按 ConfigSource 分组（Project MCPs / User MCPs / Plugin MCPs）
- 分组标题 MUTED，服务器计数 MUTED
- 连接状态：Connected=SAGE, Failed=ERROR, Connecting=WARNING, Disabled=MUTED

### /hooks 面板

- 只读面板，标题 `Hooks` 或 `Hooks (none configured)`
- 事件列表：序号 + 事件名(THINKING+BOLD) + hook 数量

### /config 面板

- Browse：字段列表，光标行 THINKING + BOLD，值 TEXT
- Edit：编辑字段高亮，开/关用 THINKING / MUTED 对比

### /plugin 面板

- Tab 栏：活动 tab THINKING 底色 + TEXT + BOLD，非活动 MUTED
- 最多 70% 屏幕高度（其他面板 60%）

## 弹窗

### HITL 审批弹窗

- 边框 WARNING 色
- 标题 `⚠ Approval Required`（WARNING + BOLD）
- 选项：`✓` Approved(SAGE) / `✗` Rejected(ERROR) / `○` Pending(MUTED)
- 光标行 `❯`（THINKING 色）

### AskUser 批量问答弹窗

- 边框 THINKING 色
- Tab 栏：活动 tab THINKING 底色 + TEXT，非活动 MUTED
- 分隔线 `──────────────`（MUTED）
- 选项：`▶ ○` 单选 / `▶ ☐` 多选
- 自定义输入区

### OAuth 弹窗

- 边框 BORDER 色
- URL 区域 MUTED
- Ctrl+O / Enter / Esc 快捷键提示

### Setup Wizard

- 步骤 1-2：ACCENT 边框
- 完成：SAGE 边框
- 全屏覆盖，优先于所有正常界面

## Widget 库

源码：`peri-widgets/src/`。独立 crate，零内部依赖。

| Widget | 样式要点 |
|--------|---------|
| BorderedPanel | TOP+BOTTOM 边框，标题居中 |
| ScrollableArea | 右侧滚动条 MUTED 色 |
| SelectableList | `▶` 光标标记，2 空格缩进 |
| InputField | `Label  Value█` 模式，2 空格缩进 |
| CheckboxGroup | `✓` 选中 / `✗` 未选中 |
| RadioGroup | `●` 选中 / `○` 未选中 |
| TabBar | `│` 分隔符，indicator char |
| Spinner | 16 帧 Unicode 符号动画 |

## 快捷键全览

### 快捷键设计规则

- **禁止 Shift + 字母**：`Shift + 字母` 在编辑状态下等同于输入大写字母，二者不可区分
- 全局操作优先使用 `Ctrl + 字母` 或功能键（如 `Esc`、`PageUp`）
- 面板内操作优先使用 `↑/↓`、`Space`、`Enter`、`Esc` 等无冲突按键

### 按键语义统一规则

| 按键 | 语义 | 说明 |
|------|------|------|
| `Enter` | 确认 / 进入深度 / 保存 | 不用于 toggle 等切换操作 |
| `Space` | 选中切换 | toggle checkbox/radio/枚举值、列表项启用/禁用 |
| `Backspace` | 删除 | 仅用于文本删除（搜索框、编辑字段） |
| `Ctrl+D` | 删除项 | 删除面板中的条目（Provider/Cron/Plugin/MCP 等） |

### 全局快捷键

| 按键 | 行为 | 说明 |
|------|------|------|
| `Ctrl+C` | 中断 Agent（loading 时）/ 退出（idle 时） | |
| `Esc` | 退出程序（idle 时） | |
| `Enter` | 提交消息（idle）/ 缓冲消息（loading） | loading 时消息排队等待 |
| `Alt+Enter` | 插入换行 | |
| `Shift+Tab` | 循环切换权限模式 | Default → DontAsk → AcceptEdit → AutoMode → Bypass |
| `Alt+M` | 循环切换模型 | opus → sonnet → haiku |
| `Ctrl+N` / `Ctrl+P` | 切换 Session（多 session 时） | |
| `Ctrl+W` | 关闭当前 Session（多 session 时） | |
| `↑` | 浮层导航 / 历史恢复 | 浮层激活时导航候选，否则恢复上一条输入 |
| `↓` | 浮层导航 / 历史恢复 | 浮层激活时导航候选，否则恢复下一条输入 |
| `Tab` | 命令/Skills 提示浮层导航 | 选中后 Enter 补全 |
| `Ctrl+V` | 粘贴剪贴板（优先图片，回退文字） | |
| `PageUp/PageDown` | 消息区上下翻页（每次 10 行） | |
| `Del` | 删除最后一个待发送附件 | |
| `MouseScrollUp/Down` | 消息区滚动 | |

### 通用面板按键约定

| 按键 | 行为 |
|------|------|
| `↑/↓` | 竖向列表导航 |
| `←/→` | 横向切换（枚举字段） |
| `Enter` | 确认/进入/保存 |
| `Space` | 选中/切换 |
| `Esc` | 关闭/取消 |
| `Ctrl+V` | 粘贴剪贴板内容 |

### HITL 审批弹窗

| 按键 | 行为 |
|------|------|
| `↑/↓` | 移动光标 |
| `Space` `t` | 切换当前项（批准/拒绝） |
| `y` | 全部批准并确认 |
| `n` | 全部拒绝并确认 |
| `Enter` | 按当前选择确认提交 |
| `Ctrl+C` | 退出程序 |

### AskUser 批量问答弹窗

| 按键 | 行为 |
|------|------|
| `Tab` / `Shift+Tab` | 切换问题 Tab |
| `↑/↓` | 当前问题内选项光标移动 |
| `Space` | 切换当前选项选中状态 |
| `Enter` | 提交所有答案 |
| 普通字符 | 自定义文本输入 |
| `Backspace` | 删除字符 |
| `Ctrl+C` | 退出程序 |

## 命令系统

源码：`peri-tui/src/command/mod.rs`。

### 命令列表

| 命令 | 说明 |
|------|------|
| `/login` | Provider 配置管理（新建/编辑/删除） |
| `/model` | 打开模型选择面板 |
| `/model <alias>` | 直接切换活跃模型（`opus` / `sonnet` / `haiku`） |
| `/history` | 历史对话浏览 |
| `/agents` | SubAgent 定义管理 |
| `/compact` | 触发上下文压缩 |
| `/clear` | 清空当前消息列表 |
| `/cron` | 定时任务管理面板 |
| `/mcp` | MCP 服务器管理面板 |
| `/memory` | Memory 文件管理面板 |
| `/hooks` | Hooks 配置查看（只读） |
| `/config` | 查看/编辑运行时配置 |
| `/plugin` | 插件市场/管理面板 |
| `/cost` | Token 用量和成本面板（StatusPanel Cost tab） |
| `/context` | 上下文窗口使用情况面板（StatusPanel Context tab） |
| `/status` | 状态面板（含 Cost/Context 两个 tab） |
| `/loop` | 循环执行 |
| `/effort <level>` | 查看或设置推理力度（low/medium/high/xhigh/max） |
| `/rename [name]` | 查看或修改当前会话标题 |
| `/help` | 列出所有命令 |

### 命令匹配

匹配优先级：精确匹配(name) > 别名精确匹配 > 前缀唯一匹配（如 `/m` 匹配 `/model`）。

### Skills 浮层

输入 `#` 前缀触发 Skills 浮层，`Tab` / `↑↓` 导航，`Enter` 补全为 `#skill-name`。
输入 `/` 前缀触发命令提示浮层，`Tab` / `↑↓` 导航。

## 权限模式

源码：`peri-middlewares/src/hitl/shared_mode.rs`。

通过 `Shift+Tab` 循环切换，状态栏首列实时显示：

| 模式 | 标签 | 颜色 | 说明 |
|------|------|------|------|
| Default | (不显示) | TEXT | 默认：所有敏感工具需审批 |
| DontAsk | Don't Ask | WARNING | 不主动提问 |
| AcceptEdit | Accept Edit | THINKING | 允许文件系统的编辑 |
| AutoMode | Auto Mode | WARNING | 大模型自动判断 |
| Bypass | Bypass | ERROR | 所有都允许 |

模式切换后标签 3 秒 BOLD + SLOW_BLINK 高亮。
