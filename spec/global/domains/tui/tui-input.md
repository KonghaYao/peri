# TUI 输入系统

> 本文档描述 InputArea 输入区域的完整设计规范，包括多行编辑、历史、@mention、slash 命令、软换行、视口跟随、占位符。

- 聚合展示对话、工具调用、工具结果、SubAgent、后台 Agent 状态、系统通知和当前 streaming turn。
- 输入区支持多行编辑、历史、文件 mention、slash command、软换行、视口跟随、placeholder。
- 状态栏持续暴露运行环境、权限模式、模型、资源占用和上下文快捷键。

---

## 3. InputArea 输入区域组件

InputArea 是 TUI 的核心交互组件，承载文本编辑、4 种叠加模式、键盘事件分发和跨平台兼容层。底层提供完整的编辑器基元（光标控制、选择、剪切/粘贴、软换行、视口跟随、placeholder）。

### 3.1 键盘事件分发架构

InputArea 内所有按键事件通过优先级链分发，同一事件只被最高优先级的激活层消费：

```text
1. 后台 Agent Bar 焦点模式（bg_bar_cursor 有值时独占所有按键）
2. Focused_only（focused_instance_id 有值，仅 Esc 退出）
3. 全局快捷键（Shift+Tab/Ctrl+B/Ctrl+T/Ctrl+Shift+T/Ctrl+O）
4. SetupWizard 按键拦截
5. Session Panel 键盘分发（Model/Agent/Hooks/Login/Config/ThreadBrowser）
6. Global Panel 键盘分发（Status/Memory/Mcp/Cron/Plugin/Betas）
7. OAuth popup
8. Rewind popup
9. HITL popup
10. 主匹配块（Ctrl+C、Esc、↑/↓、Ctrl+V、Tab、Enter、Ctrl+U/D、Delete）
12. tui_textarea 通用输入（字符插入、光标移动、退格、Delete、词级操作）
```

### 3.2 默认输入框

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ──────────────────────────────────────────────────────────────────────────── │
│   ❯ 帮我实现一个功能，并写测试                                               │
│ ──────────────────────────────────────────────────────────────────────────── │
└──────────────────────────────────────────────────────────────────────────────┘
```

**颜色**：边框与 `❯` 前缀均使用 `muted` 灰色（`#999999`），idle 与 loading 态统一。

能力：

- InputArea 边框、`❯` 前缀统一使用 `muted` 灰色，与消息区形成弱对比，不抢注意力。
- 多行 buffer，`Shift+Enter` / `Alt+Enter` 插入换行。
- `Enter`（无修饰键）提交消息并写入输入历史。
- **软换行**：通过 `wrap_text()` 做 display-width 感知的视觉行折叠（CJK 兼容）。
- **视口跟随**：以光标行为中心构建渲染窗口，超出视口时自动跟随。
- **Placeholder**：文本为空时渲染提示文本（与 tui-textarea 行为对齐）。
- **光标焦点态**：loading 中始终显示光标；终端窗口聚焦时显示；面板/弹窗打开时隐藏。
- `Ctrl+C`（有文本时）清空输入区全部内容（select-all + cut）。
- `Ctrl+C`（纯文本为空时，2 秒内两次）退出应用。
- `Ctrl+C`（loading 中）打断 Agent。
- `Ctrl+V` 粘贴剪贴板内容（图片优先解析为附件，否则插入纯文本）。
- `Ctrl+U`（textarea 有内容时）从光标位置删除到行首（`delete_line_by_head`）。
- `Ctrl+U`（textarea 为空时）消息区域向上翻页（20 行）。
- `Ctrl+D` 消息区域向下翻页（20 行）。
- `Delete`（有待上传附件时）移除最近一个待上传附件。
- 字符级光标移动：←/→ 逐字符，`Home`/`End` 行首/行尾。
- 词级操作：`Ctrl+←/→` 跳词，`Ctrl+W` 删词，`Ctrl+Backspace`/`Alt+Backspace` 删前一词。
- `Ctrl+X` / `Ctrl+C` / `Ctrl+V` 剪切/复制/粘贴选区。
- 鼠标拖拽选择文本。
- 任何可打印字符输入即退出历史浏览态。

### 3.3 @mention 文件选择

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│   ────────────────────────────────────────────────────────────────────────   │
│   @src/                                                                      │
│   > peri-tui/src/kit/...                                                     │
│     peri-agent/src/...                                                       │
│     peri-acp/src/...                                                         │
│     docs/architecture.md                                                     │
│   ────────────────────────────────────────────────────────────────────────   │
│   > 请阅读 @src/                                                             │
│   ────────────────────────────────────────────────────────────────────────   │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- @mention 文件选择弹窗与输入框同宽，只使用上下边框，不使用左右边框。
- 在空白或行首后输入 `@` 激活文件候选列表，后面紧跟路径前缀（如 `@src/`）。
- 使用 `SkimMatcherV2` 进行模糊匹配，按 prefix 过滤文件候选。
- 目录导航：用 `/` 表示层级路径；不存在的路径部分会回退到最近存在的父目录。
- ↑/↓ 导航候选，Enter 插入选中路径，Esc 取消弹窗，Tab 触发补全。
- 关闭条件：Esc、提交（Enter+有候选项）、文本变化导致无法匹配。

### 3.4 Slash completion

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│   ────────────────────────────────────────────────────────────────────────   │
│   / commands                                                                 │
│   > /model      Model alias panel                                            │
│     /login      Provider config                                              │
│     /agents     Subagent info                                                │
│     /threads    Thread browser                                               │
│     /compact    Compact context                                              │
│   ────────────────────────────────────────────────────────────────────────   │
│   > /mod                                                                      │
│   ────────────────────────────────────────────────────────────────────────   │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- Slash completion 弹窗与输入框同宽，只使用上下边框，不使用左右边框。
- 在空白或行首后输入 `/` + 前缀激活命令补全。
- 候选项合并三类来源，排序规则：**前缀精确匹配 > 命令 > Skill > Agent 命令 > 字母序**：
  - 命令注册表（`command_registry.match_prefix`）
  - 插件 skill 名称（模糊匹配，来自 `SKILL_NAMES` atom，启动时通过 `AvailableCommandsUpdate` 注入）
  - ACP agent 命令
- 补全行为：`hint_complete()` 仅替换 `/token` 段，插入 `/名称 `，其余文本不丢失。
- ↑/↓ 导航候选，Enter 确认选中项（默认选中第一个），Tab 循环候选，Esc 取消。
- 面板命令（如 `/model`）直接映射到 `PanelKind`。
- 远端命令如 `/bg`、`/clear`、`/compact`、`/rewind` 交给 ACP server。
- **提交流程**：使用 `SubmitRequest` / `SessionControlRequest` / `ViewActionRequest` 强类型统一 parse，消除 input_area 和 submit_consumer 双重字符串解析。local panel slash 优先于远端 ACP command/skill。

### 3.5 输入历史浏览模式

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│   ────────────────────────────────────────────────────────────────────────   │
│   > 上一条提交的消息                                                         │
│   ────────────────────────────────────────────────────────────────────────   │
│   [history: 3/100]
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 光标在首行时按 ↑ 进入历史模式，在末行时按 ↓ 也可以进入。
- 每一条通过 Enter 提交的消息都会入栈（`push_input_history`），上限 1000 条。
- 进入历史模式时，当前编辑内容自动保存为草稿（`draft_input`）；浏览到最旧方向时自动恢复草稿。
- 任意可打印字符输入后退出历史态。
- Esc 退出历史态并关闭局部浮层（不影响 prompt 内容）。
- 持久化到 `~/.peri/input-history.json`（原子写入：先写 `.tmp` 再 rename）。

### 3.6 预测输入模式

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│   ────────────────────────────────────────────────────────────────────────   │
│   > 帮                                                           [Tab 接受]  │
│     帮我实现一个功能，并写测试                                     ← 预测文本 │
│   ────────────────────────────────────────────────────────────────────────   │
```

能力：

- LLM 生成的下一步输入建议（`PredictionState`），显示为灰色/弱色预测文本。
- `Tab` 接受预测文本（优先于 slash hint 和 @mention 的 Tab 行为）。
- 任何可打印字符输入后预测被清除。
- 提交消息后预测被清除。

### 3.7 全局输入快捷键汇总

以下快捷键在 InputArea 有焦点时生效（排序按优先级；消费后不下传）：

| 快捷键 | 条件 | 动作 |
|--------|------|------|
| `Ctrl+C` | 有文本选中 | 剪切选区并清空 |
| `Ctrl+C` | 纯文本缓冲区 | 清空输入（select-all + cut） |
| `Ctrl+C` | Loading 中 | 打断 Agent |
| `Ctrl+C` | 空闲，2 秒内两次 | 退出应用 |
| `Esc` | Loading 中 | 清除缓冲消息 |
| `Esc` | @mention 激活 | 关闭 @mention 弹窗 |
| `Esc` | Slash hint 激活 | 关闭 slash hint |
| `Esc` | 2 秒内两次 | 打开 Rewind 选择器 |
| `↑` | @mention 激活 | @mention 候选 ↑ |
| `↑` | Slash hint 激活 | Slash 候选 ↑ |
| `↑` | 光标在首行 | 进入历史模式 ↑ |
| `↑` | 其他 | textarea 光标上移一行 |
| `↓` | @mention 激活 | @mention 候选 ↓ |
| `↓` | Slash hint 激活 | Slash 候选 ↓ |
| `↓` | 光标在末行 | 进入历史模式 ↓ |
| `↓` | 其他 | textarea 光标下移一行 |
| `Ctrl+V` | 剪贴板有图片 | 粘贴为附件 |
| `Ctrl+V` | 剪贴板纯文本 | 插入剪贴板文本 |
| `Tab` | 有预测文本 | 接受预测 |
| `Tab` | @mention 激活 | @mention 补全 |
| `Tab` | Slash hint 激活 | Slash 候选导航 |
| `Enter` | @mention 有候选项 | 插入选中文件路径 |
| `Enter` | Slash hint 激活 | 确认补全选择 |
| `Shift+Enter` / `Alt+Enter` | 任意 | 在 textarea 中插入换行 |
| `Enter` | 无修饰键 | 提交消息 |
| `Enter` | Loading 中 | 缓冲消息（追加到队列） |
| `Ctrl+U` | textarea 有内容 | 从光标位置删除到行首 |
| `Ctrl+U` | textarea 为空 | 消息区向上翻页（20 行） |
| `Ctrl+D` | 任意 | 消息区向下翻页（20 行） |
| `Delete` | 有待上传附件 | 移除最近一个待上传附件 |
| 任意可打印字符 | 任意 | 退出历史态，插入字符，更新 @mention/slash 检测 |

### 3.8 全局快捷键（与输入区无关的上下文）

以下全局快捷键在 InputArea 无弹窗/面板覆盖时生效：

| 快捷键 | 动作 |
|--------|------|
| `Shift+Tab`（BackTab） | 循环切换权限模式（default → accept-edit → auto-mode → bypass） |
| `Ctrl+O` | 切换内联 diff 可见性 |
| `Ctrl+B` | 跳转到后台 Agent Bar |
| `Ctrl+T` / `Alt+M`（macOS: `µ`） | 循环切换模型别名（opus → sonnet → haiku） |
| `Ctrl+Shift+T` / `Alt+Shift+M`（macOS: `Â`） | 循环切换 Provider |

### 3.9 跨平台兼容层

**macOS Option 键处理**：

终端在按下 Option 键时发送合成 Unicode 字符（不带修饰符标志位）。`KeyBinding` 结构体同时匹配无修饰符的 macOS 字符路径和标准 Ctrl+字母路径，确保 macOS 终端与标准终端行为一致。

| 功能 | macOS 路径 | 标准路径 |
|------|-----------|----------|
| 循环模型 | `Alt+M`（`µ`） | `Ctrl+T` |
| 循环 Provider | `Alt+Shift+M`（`Â`） | `Ctrl+Shift+T` |

**Windows 特殊处理**：

- **IME 候选窗口定位**：渲染循环调用 `Frame::set_cursor()` 使 Windows IME 候选窗口跟随 textarea 光标位置，而非固定在 `(0,0)`。
- **鼠标滚轮过滤**：Windows Terminal（ConPTY）生成与 MouseScroll 交织的虚假 `Key(Up/Down)` 事件。两阶段过滤（向前窥视 + 向后检查时间戳）防止 textarea 误拦截滚动事件。
- **模拟粘贴检测**：在不支持 bracketed paste 的终端上，将按键快速突发模式转换为 `Event::Paste`。


---

> [返回总索引](tui-index.md)
