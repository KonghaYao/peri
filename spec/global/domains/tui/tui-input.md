# TUI 输入系统

> 本文档描述 InputArea 输入区域的完整设计规范，包括多行编辑、历史、@mention、slash 命令、软换行、视口跟随、占位符。

- 聚合展示对话、工具调用、工具结果、SubAgent、后台 Agent 状态、系统通知和当前 streaming turn。
- 输入区支持多行编辑、历史、文件 mention、slash command、软换行、视口跟随、placeholder。
- 状态栏持续暴露运行环境、权限模式、模型与后台任务；CPU%/MEM/上下文使用率在 composer footer 右侧资源线。

---

## 3. InputArea 输入区域组件

InputArea 是 TUI 的核心交互组件，承载文本编辑、4 种叠加模式（@mention / slash / 历史浏览 / 预测输入）、键盘事件分发和跨平台兼容层。底层提供完整的编辑器基元（光标控制、选择、粘贴、软换行、视口跟随、placeholder）。

### 3.1 键盘事件分发架构

InputArea 的键盘事件通过焦点分层（focus_router）与事件优先级（use_event_handler）两级机制分发，同一事件只被最高优先级的激活层消费：

```text
FocusLayer 优先级（focus_router.rs，active_layer() 判定）：
  1. Popup（POPUP_KIND 有值）        — HITL / Rewind / OAuth / AskUser 等根级覆盖
  2. InlineCompletion                — @mention / slash 激活时，方向键/Enter/Esc 归弹窗
  3. Panel（ACTIVE_PANEL 有值）      — 面板内导航
  4. Input / Message                 — 输入区与消息区

注册层级（event_handlers.rs + 组件内 use_event_handler）：
  - register_global_handlers（Global + High）：Ctrl+C 三级链（取消→双击退出）、Ctrl+T / Ctrl+Shift+T / Ctrl+O 全局快捷键
  - register_root_handlers（Current + Normal）：Shift+Tab 权限模式循环、Esc 关闭优先级链（双击 Esc 500ms 内触发 Rewind）
  - InputArea（Global + Normal）：终端焦点 Gain/Lost；Current + Normal：全部编辑键；Global + High：鼠标点击光标定位
  - SlashCompletion / MentionPopup（Current + Normal + hit_test）：inline_nav 统一导航（Up/BackTab=上，Down/Tab=下，Enter=确认，Esc=取消）
```

- `input_accepts_key()`：Popup/Panel 激活时输入区不消费键盘；Ctrl+Up/Down/Home/End 显式让给消息区滚动。
- 同一事件只被最高优先级激活层消费；消费后不再下传。
- 文本编辑基元位于自研 `peri-tui/src/components/textarea/`（TextAreaState + render + widget + word + history），不再依赖 tui-textarea crate。

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
- **软换行**：通过 `wrap_text()`（`peri-tui/src/components/textarea/render.rs`）做 display-width 感知的视觉行折叠（CJK 兼容）。
- **视口跟随**：以光标行为中心构建渲染窗口（`cursor_visual_up/down` 基于视觉行移动），超出视口时自动跟随。
- **Placeholder**：文本为空时渲染提示文本（预测文本优先，其次 editor 占位符）。
- **光标焦点态**：loading 中始终显示光标；终端窗口聚焦时显示；面板/弹窗打开时隐藏。
- `Ctrl+C`：三级链——loading 中打断 Agent；空闲首次按提示「再次按 Ctrl+C 退出」；1 秒内两次退出应用。
- `Ctrl+V` 粘贴剪贴板内容：图片优先编码为 PNG 存入 `~/.peri/images/` 并插入 `@image <path>` 文本，否则插入纯文本（`\r\n`/`\r` 归一化为 `\n`，10K 字符上限）。
- `Ctrl+U` 清空输入区全部内容。
- `Ctrl+D` 从光标位置删除一个字符（delete_forward）。
- Emacs 风格编辑键：`Ctrl+A` 行首、`Ctrl+E` 行尾、`Ctrl+B` 左移、`Ctrl+F` 右移、`Ctrl+H` 退格、`Ctrl+W` 删前一词、`Ctrl+Z` undo、`Ctrl+R` redo、`Ctrl+Y` paste_yank（粘贴到 yank 寄存器）。
- 字符级光标移动：←/→ 逐字符，`Home`/`End` 行首/行尾，`Ctrl+←/→`、`Alt+B/F` 跳词。
- 词级操作：`Alt+Backspace` 删前一词，`Alt+Delete` 删后一词。
- 鼠标点击输入区文本定位光标（Global+High 事件 + unicode-width 显示列换算）。
- 任何可打印字符输入即退出历史浏览态。

### 3.3 @mention 文件选择

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│   ────────────────────────────────────────────────────────────────────────   │
│   @src/                                                                      │
│   > peri-tui/src/kit/...                                                     │
│     peri-agent/src/...                                                       │
│     peri-acp/src/...                                                         │
│     docs/design/architecture.md                                              │
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
- ↑/↓ 导航候选，Enter 插入选中路径，Esc 取消弹窗，Tab 移到下一个候选（与 ↓ 相同，BackTab/↑ 上一个）。
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
- 候选项由 `build_slash_items()`（input_area.rs）合并三类来源，预按字母序排序：
  - Panel 注册表（`PANELS`，`slash_command` 非空的 16 面板，`SlashActionKind::Panel`）
  - `/setup` 固定命令（打开 Setup Wizard）
  - 远端 ACP command/skill（`AVAILABLE_SLASH_COMMANDS` atom，经 `AvailableCommandsUpdate` 注入；`SKILL_NAMES` atom 区分 Skill 与 Command 类别）
- 有前缀时组件端用 `SkimMatcherV2` 模糊匹配并按分数降序（`SlashCompletionItem` 预存 label_lowercase 避免每帧 to_lowercase）。
- 补全行为：`apply_slash_selection()`（input_area.rs）仅替换 `/token` 段，插入 `/名称 `，其余文本不丢失；`replace_last_mention()` 负责 @mention 段替换。
- ↑/↓（或 BackTab/Tab）导航候选，Enter 确认选中项（默认选中第一个），Esc 取消。
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

- 光标在首行时按 ↑ 进入历史模式，在末行时按 ↓ 也可以进入（`cursor_visual_up/down` 视觉行移动失败时触发 `history_up`/`history_down`）。
- 每一条通过 Enter 提交的消息都会入栈（`push_history`），上限 1000 条。
- 进入历史模式时，当前编辑内容自动保存为草稿（`HISTORY_DRAFT` atom，input_history.rs）；浏览到最旧方向时自动恢复草稿。
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
| `Ctrl+C` | Loading 中 | 打断 Agent |
| `Ctrl+C` | 空闲，1 秒内两次 | 退出应用 |
| `Esc` | 激活层为 Popup / Panel | 关闭当前弹窗/面板 |
| `Esc` | @mention 激活 | 关闭 @mention 弹窗 |
| `Esc` | Slash hint 激活 | 关闭 slash hint |
| `Esc` | 500ms 内两次 | 打开 Rewind 选择器 |
| `↑` | @mention 激活 | @mention 候选 ↑ |
| `↑` | Slash hint 激活 | Slash 候选 ↑ |
| `↑` | 光标在首行（视觉行移动失败） | 进入历史模式 ↑ |
| `↑` | 其他 | textarea 光标上移一行 |
| `↓` | @mention 激活 | @mention 候选 ↓ |
| `↓` | Slash hint 激活 | Slash 候选 ↓ |
| `↓` | 光标在末行（视觉行移动失败） | 进入历史模式 ↓ |
| `↓` | 其他 | textarea 光标下移一行 |
| `Ctrl+V` | 剪贴板有图片 | 编码 PNG 存入 `~/.peri/images/`，插入 `@image <path>` |
| `Ctrl+V` | 剪贴板纯文本 | 插入剪贴板文本（10K 字符上限） |
| `Tab` | 有预测文本 | 接受预测（优先于弹窗 Tab 行为） |
| `Tab` | @mention / Slash 激活 | 移到下一个候选（与 ↓ 相同；BackTab/↑ 上一个） |
| `Enter` | @mention 有候选项 | 插入选中文件路径 |
| `Enter` | Slash hint 激活 | 确认补全选择 |
| `Shift+Enter` / `Alt+Enter` | 任意 | 在 textarea 中插入换行 |
| `Enter` | 无修饰键 | 提交消息 |
| `Enter` | Loading 中 | 缓冲消息（追加到 INPUT_BUFFER 队列，上限 32 条） |
| `Ctrl+U` | 任意 | 清空输入区全部内容 |
| `Ctrl+D` | 任意 | 从光标位置删除一个字符 |
| `Ctrl+A` / `Ctrl+E` | 任意 | 光标到行首 / 行尾 |
| `Ctrl+B` / `Ctrl+F` | 任意 | 光标左移 / 右移一字符 |
| `Ctrl+H` | 任意 | 退格 |
| `Ctrl+W` | 任意 | 删除光标前一词 |
| `Ctrl+Z` / `Ctrl+R` | 任意 | undo / redo |
| `Ctrl+Y` | 任意 | paste_yank（粘贴到 yank 寄存器） |
| `Alt+B` / `Alt+F` / `Alt+←/→` | 任意 | 光标左/右跳一词 |
| `Alt+Backspace` / `Alt+Delete` | 任意 | 删除光标前/后一词 |
| `Ctrl+Up` / `Ctrl+Down` / `Ctrl+Home` / `Ctrl+End` | 输入区有焦点 | 让给消息区滚动（显式排除，避免与输入键冲突） |
| 任意可打印字符 | 任意 | 退出历史态，插入字符，更新 @mention/slash 检测 |

### 3.8 全局快捷键（与输入区无关的上下文）

以下全局快捷键在 InputArea 无弹窗/面板覆盖时生效：

| 快捷键 | 动作 |
|--------|------|
| `Shift+Tab`（BackTab） | 循环切换权限模式（default → accept-edit → auto-mode → bypass） |
| `Ctrl+O` | 切换内联 diff 可见性 |
| `Ctrl+T` / `Alt+M`（macOS: `µ`） | 循环切换模型别名（opus → sonnet → haiku） |
| `Ctrl+Shift+T` / `Alt+Shift+M`（macOS: `Â`） | 循环切换 Provider |

以上快捷键由 `focus_router.rs` 的 `classify_global_shortcut()` 统一识别（`GlobalShortcut` 枚举），模型/Provider 循环触发 StatusBar 高亮提示。`Ctrl+B` 为输入区 Emacs 光标左移（非全局快捷键）；后台任务区为只读状态渲染，无焦点跳转。

### 3.9 跨平台兼容层

**macOS Option 键处理**：

终端在按下 Option 键时发送合成 Unicode 字符（不带修饰符标志位）。`KeyBinding` 结构体（focus_router.rs）同时匹配无修饰符的 macOS 字符路径和标准 Ctrl+字母路径，确保 macOS 终端与标准终端行为一致。

| 功能 | macOS 路径 | 标准路径 |
|------|-----------|----------|
| 循环模型 | `Alt+M`（`µ`） | `Ctrl+T` |
| 循环 Provider | `Alt+Shift+M`（`Â`） | `Ctrl+Shift+T` |

**终端模式与特殊处理**（entry.rs 统一启用）：

- **终端能力**：全屏模式显式启用 `EnableMouseCapture`（未启用时很多终端会把滚轮转成 Up/Down，与键盘方向键语义混淆）、`EnableBracketedPaste`（粘贴内容合并为单个 `Event::Paste`）、`EnableFocusChange`（`FocusGained`/`FocusLost` 驱动输入区光标显示态）。
- **SIGINT 拦截**：raw mode 下 macOS 部分终端仍可能对 Ctrl+C 发送 SIGINT，进程级 handler 吞掉 SIGINT，Ctrl+C 事件级处理由 Global handler（event_handlers.rs）独立完成。
- **粘贴归一化**：部分终端（VSCode、iTerm2）在 Bracketed Paste 中使用 `\r` 作为换行分隔符，`Event::Paste` 分支统一 `\r\n`/`\r` → `\n` 归一化，并设 10K 字符上限（I22-A，防误粘 10MB 日志冻结终端）。

## Issue 经验附录

### issue_2026-07-05-input-unicode-cursor-misalignment
**摘要:** 输入框 Unicode 字符删除时光标估算错误，出现多个白色光标残影
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** CJK 光标, unicode-width, CjkGhostFix, 续接 cell, AlwaysUpdate
**问题本质:** 光标以字符索引而非显示列定位；ratatui `set_stringn` 对双宽字符的续接 cell 始终 reset，两帧相同 → diff 跳过 → 终端保留主 cell bg 的视觉扩展（白色残影）。
**通用模式:** 终端坐标必须按显示宽度计算（`display_width_before` 用 `UnicodeWidthChar`）；双宽字符续接 cell 需 `CellDiffOption::AlwaysUpdate` 强制 diff 发送 SGR——唯一同时满足「透明无底色」与「清除残影」的方案。
**涉及文件:** peri-tui/src/kit/input_area.rs（CjkGhostFix hook + display_width_before + 空态光标）, peri-tui/src/components/textarea/render.rs（render_multiline_with_cursor，原 peri-widgets/src/textarea/render.rs，已迁入 peri-tui）, peri-tui/src/components/textarea/state.rs（TextAreaState，原 peri-widgets/src/textarea/state.rs）, peri-tui/src/kit/text_selection.rs（visual_col_to_byte_offset 参考实现）；peri-tui/src/kit/theme.rs 已不存在（主题 token 迁至 peri-theme crate）
**CLAUDE.md 链接:** false

### issue_2026-07-07-inputarea-mouse-click-cursor-positioning
**摘要:** InputArea 鼠标点击光标快速定位——功能缺失
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** 鼠标点击, 光标定位, EventScope, AreaTracker, 事件优先级
**问题本质:** ratatui-kit 迁移后（S13 删除 src/event/mouse.rs）鼠标事件分发变化：`EventScope::Current` 不向 InputArea 分发鼠标事件；message_area 的 Global+High handler 对所有鼠标事件返回 Consumed 导致 Normal 优先级永远排不到。
**通用模式:** 鼠标事件需 `EventScope::Global + EventPriority::High` 注册，命中检测用 `AreaTracker`（pre_component_draw 捕获区域）+ 显示列换算（unicode-width）；遮挡场景经 mouse_router 放行给前景 handler。
**涉及文件:** peri-tui/src/kit/input_area.rs（Global+High 鼠标 handler + composer 区域命中换算）, peri-tui/src/kit/message_area/mod.rs（消息区外 Down(Left) 放行，原 message_area.rs 已拆为目录）, peri-tui/src/components/textarea/state.rs（line_col_to_cursor，原 peri-widgets/src/textarea/state.rs）；旧 peri-tui/src/event/mouse.rs 已删除（commit 59d0574a，S13 清理）
**CLAUDE.md 链接:** false

### issue_2026-07-05-paste-newline-triggers-submit
**摘要:** 输入框粘贴含换行文本时直接触发 Enter 提交
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** Bracketed Paste, Event::Paste, 换行粘贴, entry.rs
**问题本质:** 终端未启用 Bracketed Paste 时，crossterm 将粘贴内容拆为按键事件流，含换行的文本被当作 Enter 提交。
**通用模式:** 文本输入类终端必须启用 `EnableBracketedPaste`（退出时 Disable）；`Event::Paste` 与按键事件分别处理。
**涉及文件:** peri-tui/src/kit/entry.rs（Bracketed Paste 启用/禁用）, peri-tui/src/kit/input_area.rs（Event::Paste 分支 + `\r\n`/`\r` 归一化 + 10K 字符上限）
**CLAUDE.md 链接:** false

### issue_2026-07-09-textarea-no-soft-wrap
**摘要:** textarea 缺少软换行（soft wrapping），长行被截断且视口跟随异常
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 软换行, wrap_text, 视觉行, desired_col, 视口跟随
**问题本质:** 渲染只按 `\n` 拆分逻辑行；视口跟随与光标上下移动基于逻辑行（`text.matches('\n').count()`），软换行后光标可能落在视口外、Down 键跳到错误行。
**通用模式:** 折行在渲染层做（纯视觉概念，不写入状态层）；`TextAreaState` 记忆 `desired_col` 保持视觉列；渲染与光标移动都必须传入 `max_width`。
**涉及文件:** peri-tui/src/components/textarea/render.rs（wrap_text + WrapResult/VisualLine，原 peri-widgets/src/textarea/render.rs 已迁入）, peri-tui/src/components/textarea/state.rs（TextAreaState.desired_col，原 peri-widgets/src/textarea/state.rs）, peri-tui/src/components/textarea/widget.rs（area.width 作为 max_width）, peri-tui/src/kit/input_area.rs（composer 可用宽度传入 + cursor_visual_up/down）
**CLAUDE.md 链接:** false

### issue_2026-07-15-setup-wizard-no-paste-login-no-edit
**摘要:** Setup 向导 Form 不支持粘贴，Login 面板不支持编辑 Provider
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Setup Wizard, paste, Login 面板, Provider 编辑
**问题本质:** `handle_wizard_event` 通过 `let Event::Key(key) = event` 过滤丢弃了所有非按键事件，`Event::Paste` 被直接丢弃；Login 面板为纯只读列表，无编辑入口。
**通用模式:** 事件分发禁止用 let-else 模式丢弃非目标事件类型；表单输入组件需统一支持 paste。
**涉及文件:** peri-tui/src/kit/setup_wizard.rs（handle_wizard_event / handle_text_input 增加 paste 支持）, peri-tui/src/kit/panels/login.rs（Provider 编辑入口）
**CLAUDE.md 链接:** false

### issue_2026-07-05-enter-clear-hook-mismatch-panic
**摘要:** Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃
**状态:** fixed+verify
**归档日期:** 2026-07-06
**关键词:** ratatui-kit hook 顺序, build_footer_lines, 条件早退
**问题本质:** `MessageArea::build_footer_lines` 内条件早退 + hook 调用点位于 `if empty` 分支后，违反 ratatui-kit 的 hook 稳定顺序约束，触发 `Hook type mismatch` panic。
**通用模式:** `#[component]` 的 hooks 必须在所有条件分支、`match` 与提前返回前按稳定顺序调用。
**涉及文件:** peri-tui/src/kit/message_area/footer.rs（build_footer_lines，原 message_area.rs 已拆为目录）, peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/submit_consumer.rs（/clear 触发新 session）
**CLAUDE.md 链接:** false

### issue_2026-07-09-agent-prefix-triggers-command-without-slash
**摘要:** 输入 "agent " 开头触发 OpenPanel 命令，无需 / 前缀
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** slash command, trim_start_matches, panel_for_slash_command, 前缀守卫
**问题本质:** `parse_submit_request` 调用 `panel_for_slash_command` 前缺少 `/` 前缀检查——`trim_start_matches('/')` 在无前缀时是 no-op，`"agent"` 也能匹配注册的 slash_command。
**通用模式:** 命令归一化必须显式检查命令前缀；注册表匹配函数与调用方守卫职责分离（`panel_for_slash_command` 本身保持无前缀兼容，slash popup 的 on_select 传入命令名属合理场景）。
**涉及文件:** peri-tui/src/kit/submit_request.rs（`command.starts_with('/')` 守卫）, peri-tui/src/kit/panel_registry.rs（panel_for_slash_command）
**CLAUDE.md 链接:** false

### issue_2026-08-02-prediction-metadata-only-clears-placeholder
**摘要:** 仅元数据（SetTitle/AddTag）的 prediction 以空文本覆盖输入区占位内容
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** prediction 覆盖, 空文本语义, 占位保护
**问题本质:** 服务端 prediction 只产出元数据动作（无 Placeholder）时 text 为空串，客户端 handle_prediction 用空文本覆盖已有占位（如 `!` 或上次输入）。
**通用模式:** "空 text"应视为无内容而非清空指令；覆盖型写入先判空；仅元数据响应不得改动占位。
**涉及文件:** peri-tui/src/kit/acp_events/（handle_prediction 路径）
**CLAUDE.md 链接:** false


---

> [返回总索引](tui-index.md)
