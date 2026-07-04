# TUI 页面能力设计图 v2

> 状态：v2 设计基线。本文档从当前可落地布局出发，继续承载未来 TUI 架构演进；后续所有 TUI 页面能力设计都应以本文件为入口，先更新设计，再落地代码。

## 目录

- [v2 → 未来最优架构目标](#v2--未来最优架构目标)
- [TUI 事件分发与 MessagePipeline](#tui-事件分发与-messagepipeline)
- [ratatui-kit 范式迁移标注](#ratatui-kit-范式迁移标注)
- [Theme System v2](#theme-system-v2)
- [ACP-only Data Flow](#acp-only-data-flow)
- [设计原则](#设计原则)
- [1. AppShell 根页面](#1-appshell-根页面)
- [2. MessageArea 区域组件](#2-messagearea-区域组件)
- [3. InputArea 输入区域组件](#3-inputarea-输入区域组件)
- [4. StatusBar 区域组件](#4-statusbar-区域组件)
- [5. PanelOverlay 面板容器](#5-paneloverlay-面板容器)
- [6. 14 个 Panel 页面设计](#6-14-个-panel-页面设计)
- [7. PopupOverlay 弹窗页面设计](#7-popupoverlay-弹窗页面设计)
- [8. 面板导航与互斥关系](#8-面板导航与互斥关系)
- [9. 快捷键设计规范](#9-快捷键设计规范)
- [10. 设计落地注意事项](#10-设计落地注意事项)

本文档描述 Peri TUI 的页面、面板、弹窗与局部组件能力。设计以当前 `peri-tui/src/kit/` 架构为基准：`AppShell → SessionColumn → MessageArea + PanelOverlay + InputArea`，`StatusBar` 与 `SessionColumn` 同级位于根布局底部。**InputArea 和 PanelOverlay 必须在 SessionColumn 内部**：PanelOverlay 位于消息流与输入区之间；面板打开时隐藏 InputArea。PopupOverlay 是 AppShell 根级覆盖层。

## v2 → 未来最优架构目标

我们要把 TUI 架构到“最优秀”：不是堆更多页面，而是形成**可组合、可验证、可演进、低心智负担**的界面系统。未来设计遵循以下目标：

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Future TUI Architecture                                                      │
│                                                                              │
│  Domain Events                                                               │
│      │                                                                       │
│      ▼                                                                       │
│  ViewModel Store ───────────────┐                                            │
│      │                          │                                            │
│      ▼                          ▼                                            │
│  Layout Composer          Interaction Router                                 │
│      │                          │                                            │
│      ▼                          ▼                                            │
│  Page Components          Focus / Command / Shortcut                         │
│      │                          │                                            │
│      └──────────────┬───────────┘                                            │
│                     ▼                                                        │
│              Render Surface                                                  │
│                                                                              │
│  原则：数据单源 → 布局可组合 → 交互可路由 → 渲染可测试 → 能力可扩展            │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 未来架构原则

- **单一事实源**：TUI 所有数据只允许从 ACP event/view model 流进入；非 ACP 标准能力必须先封装为 Peri 自定义 ACP 事件，再进入 TUI store，禁止 UI 直接读业务后端。
- **布局与业务分离**：区域组件只声明布局和视觉；业务动作通过 command/effect 层执行。
- **焦点显式化**：输入区、消息区、面板、弹窗、补全浮层必须有统一 focus model，禁止靠事件处理顺序碰运气。
- **能力插件化**：新增 Panel / Popup / Inline Widget 只注册元数据、渲染器、事件路由，不改散落 switch。
- **可观测优先**：每个长任务、后台任务、工具调用、工作流都在 UI 有稳定位置和状态生命周期。
- **响应式终端**：窄屏、宽屏、高度不足、滚动场景都必须有明确降级策略。
- **统一 Theme System**：所有颜色、边框、状态色、文本层级都只能从可替换 theme palette 派生；组件禁止硬编码颜色。
- **测试友好**：核心布局和 ViewModel 渲染应可 headless snapshot，不依赖真实终端交互。
- **v2 不妥协升级**：版本更新以最终架构正确性为准，不为兼容临时坏设计牺牲结构；迁移可以分阶段，但目标设计不降级。
- **ratatui-kit 范式统一**：所有旧 TUI 组件都必须迁移或包裹为 ratatui-kit component，不允许新旧渲染范式长期并存。

### v2 之后的演进方向

```text
Phase A：固化 v2 基线
  - 页面设计图成为所有 TUI 变更的入口
  - 明确 SessionColumn 内部结构不可破坏
  - 为 Panel / Popup / Input / Message 建立统一命名与能力边界

Phase B：ratatui-kit 组件范式统一
  - 所有旧组件标注 Legacy，并规划迁移到 #[component] / element! / hooks 范式
  - 旧组件短期只能作为 adapter 被 ratatui-kit 页面包裹，不允许继续扩展旧 API
  - 新增页面、Panel、Popup、Inline widget 必须直接使用 ratatui-kit 范式

Phase C：建立 Focus Graph
  - Popup > InlineCompletion > Panel > Input > Message
  - Esc / Enter / Tab / arrows 走统一 focus router
  - 每个组件声明自己消费哪些事件

Phase D：Theme System 1.0
  - 建立可替换 theme palette：base / semantic / component tokens
  - 所有组件只消费 theme token，不直接写 Color / hex / ANSI 色
  - 支持内置主题、用户主题、运行时切换和 snapshot 校验

Phase E：ACP-only Data Flow
  - TUI 数据入口统一为 ACP events / ACP view models
  - 非 ACP 标准能力封装为 `peri/unstable-event` 上的 Peri 自定义事件
  - 禁止 UI 组件直接读取 agent/runtime/plugin/backend 内部状态

Phase F：Panel Registry 2.0
  - Panel metadata + render + event + command 一处注册
  - 消除新增面板时多文件同步遗漏
  - 支持 panel category、search、pin、recent

Phase G：可组合工作台
  - 默认单列聊天
  - 宽屏可启用 side inspector / task rail / trace rail
  - 长任务与 workflow 有独立观察区域，不挤压主聊天

Phase H：UI Snapshot Verification
  - 关键页面 ASCII/snapshot 固化
  - 每次 TUI 变更自动比较布局退化
  - 对窄屏、宽屏、popup、panel-open 状态做矩阵测试
```

## TUI 事件分发与 MessagePipeline

当前 TUI 仍处于双路径分发模型，未来 Focus Graph 必须映射到现有 Effect 体系，而不是另起一套事件系统。

```text
TuiEvent
  → state_machine::handle(State, Event) -> (State, Vec<Effect>)
  → keyboard::handle_key_event legacy fallback
  → merge/dedupe Effects
  → execute Effects
  → render
```

关键约束：

- `OpenPanel` / `ClosePanel` / `SubmitMessage` / `PollAgent` / `Render` 等能力都必须落到 Effect，不允许组件直接操作 runtime。
- MessagePipeline 单一数据源为 `transcript + Option<PartialAiMessage>`。
- `commit_iteration` 是替换语义；禁止把 finalized messages extend 到 transcript。
- 子 Agent 提交不得污染父 Agent transcript。

## ratatui-kit 范式迁移标注

v2 之后所有 TUI 组件以 ratatui-kit 为唯一 UI 范式。旧组件可以短期存在，但必须被标注为 Legacy，并通过 adapter 接入 ratatui-kit 树；禁止继续在旧范式上增加新能力。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ratatui-kit Component Tree                                                   │
│                                                                              │
│  AppShell #[component]                                                       │
│    ├─ SessionColumn #[component]                                             │
│    │   ├─ MessageArea #[component]                                           │
│    │   ├─ PanelOverlay #[component]                                          │
│    │   └─ InputArea #[component]                                             │
│    ├─ StatusBar #[component]                                                 │
│    └─ PopupOverlay #[component]                                              │
│                                                                              │
│  Legacy widgets                                                               │
│    └─ only via RatatuiWidgetAdapter / ViewModelRendererAdapter               │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 迁移规则

- 新组件必须使用 ratatui-kit：`#[component]`、`element!`、props、hooks、context。
- 旧 `Widget` / 手写 `Frame` 渲染只能作为 adapter 进入 ratatui-kit component tree。
- 旧组件禁止直接读取全局状态；必须通过 props 或 context 接收派生 view state。
- 旧组件禁止新增业务能力；新增能力先迁移到 ratatui-kit，再实现。
- 事件处理统一走 ratatui-kit component 边界与 Focus Graph，不允许组件私自捕获全局键盘事件。
- Theme、ACP-only data、Panel Registry、Popup Registry 都必须以 ratatui-kit component 为消费端。

### 当前组件迁移标注

| 组件 | 目标状态 | 要求 |
|------|----------|------|
| AppShell | ratatui-kit native | 根组件，负责组合 SessionColumn / StatusBar / PopupOverlay |
| SessionColumn | ratatui-kit native | InputArea 与 PanelOverlay 必须在内部 |
| MessageArea | ratatui-kit native + adapter | 可短期包裹 legacy ViewModel renderer，但新 loading/todo 直接用 kit 组件 |
| InputArea | ratatui-kit native | 手写 textarea 行为通过 props/state 进入组件，不暴露旧渲染入口 |
| PanelOverlay | ratatui-kit native | 所有 Panel 由 registry 注入 component renderer |
| PopupOverlay | ratatui-kit native | HITL / AskUser / Rewind / OAuth 由 registry 注入 component renderer |
| StatusBar | ratatui-kit native | 只消费 theme token 与 ACP snapshot |
| Spinner | MessageArea 子组件，ratatui-kit wrapped widget | 使用 `peri-widgets/src/spinner`，通过 kit wrapper 接入 |
| Todo / Plan | MessageArea 子组件，ratatui-kit native | 消费 `SessionUpdate::Plan` view state，显示在 Spinner 下方 |
| Welcome | ratatui-kit native | 空会话 MessageArea 状态，窄屏需降级 |
| SetupWizard | ratatui-kit native | 首启引导，禁止裸 `q` 关闭 |
| 14 Panels | ratatui-kit native | 由 Panel Registry 注入，所有 Panel 只使用上下边框 |
| 4 Popups | ratatui-kit native | 由 Popup Registry 注入；AskUser 只使用上下边框，其余 popup 可按语义保留 modal 边框 |
| MentionPopup / SlashCompletion | ratatui-kit native | 与 InputArea 同宽，只使用上下边框 |

## Theme System v2

v2 之后，TUI 的所有视觉表达必须基于统一 theme system。Theme 是可替换能力，不是散落在组件里的颜色常量。

### Theme 架构

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Theme Package                                                                 │
│                                                                              │
│  Palette（原始色盘）                                                          │
│    ├─ base.bg / base.fg / base.gray.* / brand.*                              │
│    ├─ accent.* / success.* / warning.* / danger.* / info.*                   │
│    └─ diff.add / diff.remove / diff.hunk                                     │
│          │                                                                   │
│          ▼                                                                   │
│  Semantic Tokens（语义 token）                                                │
│    ├─ text.primary / text.muted / text.dim                                   │
│    ├─ border.default / border.active / border.dim                            │
│    ├─ status.running / status.success / status.warning / status.error        │
│    └─ surface.default / surface.user / surface.popup / surface.selection     │
│          │                                                                   │
│          ▼                                                                   │
│  Component Tokens（组件 token）                                               │
│    ├─ message.user.bg / message.ai.prefix / message.tool.indicator           │
│    ├─ input.border / input.cursor / input.placeholder                        │
│    ├─ panel.border / panel.title / panel.row.selected                        │
│    ├─ popup.bg / popup.border / popup.action.primary                         │
│    └─ statusbar.text / statusbar.resource.good/warn/bad                     │
│          │                                                                   │
│          ▼                                                                   │
│  Components                                                                   │
│    MessageArea / InputArea / PanelOverlay / PopupOverlay / StatusBar         │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Theme 规则

- 组件禁止直接写 `Color::Rgb`、hex、ANSI 色名或临时颜色常量。
- 组件只能使用 component token；component token 缺失时补齐 token，不在组件内绕过。
- semantic token 负责表达意义，palette 负责表达审美；替换主题时组件结构不变。
- theme 必须覆盖：文本层级、背景、边框、选中态、光标、滚动条、diff、工具状态、SubAgent、错误态。
- 所有内置主题必须通过同一 token schema；用户主题也必须通过 schema 校验。
- Theme 切换应是数据更新，不应要求重启 TUI。

### Theme 数据形态

```text
ThemeDefinition
  name: string
  mode: dark | light | high-contrast
  palette:
    base / brand / gray / accent / success / warning / danger / info / diff
  semantic:
    text / border / surface / status / diff
  component:
    message / input / panel / popup / statusbar / markdown / scrollbar
```

### Theme 不妥协约束

- 新组件上线前必须声明 component token 使用清单。
- 新状态色上线前必须先定义 semantic token。

### Status Token 规范

- `status.running`：状态显示只用 emoji `●`，颜色使用 running token；不在列表中重复显示 `running`。
- `status.success`：状态显示只用 emoji `✓`，颜色使用 success token；不在列表中重复显示 `completed` / `done` / `connected` / `enabled`。
- `status.warning`：状态显示只用 emoji `○`，颜色使用 warning token；不在列表中重复显示 `pending` / `waiting` / `paused` / `disabled`。
- `status.error`：状态显示只用 emoji `✗`，颜色使用 error token；不在列表中重复显示 `failed` / `error` / `disconnected`。
- 状态在 UI 列表中使用“emoji + theme color”表达，英文状态只保留在 ACP payload / debug / accessibility label，不作为主要视觉文本。
- 示例：`✓` 使用 `status.success`；`●` 使用 `status.running`；`○` 使用 `status.warning`；`✗` 使用 `status.error`。
- 禁止“临时先写死颜色以后再抽”；颜色系统是 v2 基线能力。
- `TUI-STYLE.md` 应逐步降级为主题示例，不再作为组件直接依赖的颜色源。
- Theme System / MCP / Plugin / Workflow 等属于 v2 目标能力；若当前代码尚未完整实现，文档仍以目标架构为准，但必须在对应实现 issue 中标注 DTO、ACP event 和 ratatui-kit 迁移任务。
- `peri/unstable-event` 中列出的事件名属于设计期事件目录，未实现项必须在 `docs/design/peri-acp-protocol.md` 或对应 issue 中标注状态，不得在 UI 侧绕过。

## ACP-only Data Flow

TUI 的所有业务数据来源只有一个途径：**从 ACP 来**。ACP 标准没有覆盖的 Peri 专有能力，也必须包装成 Peri 自定义 ACP 事件，经同一事件入口进入 TUI。

### 数据流架构

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Agent / Runtime / Middleware / Plugin / Workflow / Cron / MCP                │
└───────────────────────────────┬──────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ ACP Layer                                                                    │
│                                                                              │
│  Standard ACP Events / ViewModels                                            │
│    - message delta / view commit / done / error                              │
│    - permission / interaction / session events                               │
│                                                                              │
│  Peri Custom Events via `peri/unstable-event`                                │
│    - event: "plugin-snapshot"     data: PluginSnapshot                       │
│    - event: "mcp-snapshot"        data: McpSnapshot                          │
│    - event: "cron-snapshot"       data: CronSnapshot                         │
│    - event: "memory-snapshot"     data: MemorySnapshot                       │
│    - event: "workflow-snapshot"   data: WorkflowSnapshot                     │
└───────────────────────────────┬──────────────────────────────────────────────┘
                                │ single ingress
                                ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ TUI Event Router                                                             │
│  map ACP event → update Store → derive View State                            │
└───────────────────────────────┬──────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ Components                                                                   │
│  MessageArea / PanelOverlay / InputArea / PopupOverlay / StatusBar           │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 数据来源规则

- TUI 组件禁止直接读取 Agent、Middleware、Plugin、MCP、Cron、Workflow 的内部状态。
- TUI 组件禁止直接读配置文件来获得业务真相；配置变化必须通过 ACP snapshot/event 进入。
- ACP service snapshot、panel lists、popup payload、task state 等都必须可追溯到 ACP event。
- 非 ACP 标准字段不得偷塞到 UI 私有状态；必须定义 `peri/unstable-event` 自定义事件 schema。
- TUI store 只做 UI 派生，不拥有业务生命周期。
- 如果某项能力无法从 ACP 标准事件/方法进入，先补 `peri/unstable-event` 自定义事件，再做 UI。

### 自定义事件通道

所有 Peri 专有事件统一走 JSON-RPC notification method：`peri/unstable-event`。

```json
{
  "jsonrpc": "2.0",
  "method": "peri/unstable-event",
  "params": {
    "session_id": "<session-id>",
    "event": "workflow-snapshot",
    "data": {}
  }
}
```

规则：

- 通道 method 固定为 `peri/unstable-event`，不要再发明 `peri/workflow_snapshot`、`peri/plugin_snapshot` 这类 method。
- `event` 使用 kebab-case 字符串，全局唯一。
- `data` 结构定义在 `peri-acp-types/src/event_data.rs` 或同等协议类型位置。
- 完整事件目录以 `docs/design/peri-acp-protocol.md` 为准；TUI-PAGE 只记录页面数据需求。

### 自定义事件命名建议

```text
service-snapshot
plugin-snapshot
mcp-snapshot
cron-snapshot
memory-snapshot
task-snapshot
workflow-snapshot
theme-changed
panel-payload
```

### ACP-only 不妥协约束

- 新页面的数据需求必须先写出 ACP event/view model 来源。
- 没有 ACP 来源的页面能力不能落地为正式功能。
- 临时调试可以本地 mock，但必须与 ACP schema 同形。
- 所有 TUI 页面设计图必须标明数据来源：ACP standard / `peri/unstable-event` custom event / `session/query` snapshot。

## 设计原则

- **聊天优先**：默认界面始终以消息流为主体，所有能力围绕输入、回放、工具结果和状态反馈展开。
- **抽屉式面板**：14 个 Panel 不是全屏 modal，而是插入消息区底部，打开时隐藏输入区，减少焦点冲突。
- **弹窗高优先级**：HITL、AskUser、Rewind、OAuth 是根级覆盖，优先于面板和输入补全。
- **键盘可达**：面板统一使用方向键导航，`Enter` 确认，`Esc` 关闭；禁止 `j/k/q` 这类裸单字母承担导航或关闭能力。
- **透明信息层级**：主文字、次级说明、弱提示、状态色遵循 Theme System 的 TEXT/MUTED/DIM 与功能色规范。
- **边框克制**：InputArea、PanelOverlay、所有 Panel、@mention、Slash completion、AskUser Popup 只使用上下边框；禁止左右边框，保持聊天主界面横向通透。

## 页面总览

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ AppShell                                                                     │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ SessionColumn                                                            │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ MessageArea / Welcome                                                │ │ │
│ │ │ - committed view models                                              │ │ │
│ │ │ - current turn stream                                                │ │ │
│ │ │ - tool / subagent / diff render                                      │ │ │
│ │ │ - mouse wheel scroll                                                 │ │ │
│ │ └──────────────────────────────────────────────────────────────────────┘ │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ PanelOverlay：Model/Login/Agent/Hooks/Config/Threads/...             │ │ │
│ │ │ - inside SessionColumn                                               │ │ │
│ │ │ - between MessageArea and InputArea                                  │ │ │
│ │ └──────────────────────────────────────────────────────────────────────┘ │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ InputArea：multiline prompt + @mention + slash completion            │ │ │
│ │ │ - inside SessionColumn                                               │ │ │
│ │ │ - hidden while panel is open                                         │ │ │
│ │ └──────────────────────────────────────────────────────────────────────┘ │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ StatusBar：permission · cwd · provider/model · CPU · MEM + hints         │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│ PopupOverlay：Hitl / AskUser / Rewind / OAuth，居中覆盖，优先级最高           │
└──────────────────────────────────────────────────────────────────────────────┘
```

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
│  ◜ 思考中…  Todo: 设计 Workflow Panel (12s · ↓ 1.2k tokens)                  │
│                                                                              │
│                                                                              │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ > 输入你的任务...                                                        │ │
│ │ @ mention files    / commands                                           │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Auto · perihelion · anthropic/claude-code-sonnet · CPU 12% · MEM 430MB        │
│                 /::commands · Shift+Enter::newline · Ctrl+T::mode · Ctrl+O::diff│
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 聚合展示对话、工具调用、工具结果、SubAgent、系统通知和当前 streaming turn。
- 输入区支持多行编辑、历史、文件 mention、slash command。
- 状态栏持续暴露运行环境、权限模式、模型、资源占用和上下文快捷键。

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

- 统一渲染 ACP view models，包括文本、工具、SubAgent、系统事件等。
- 支持 diff 可见性切换，diff 内容自动使用增删行语义色。
- 鼠标滚轮滚动消息区；键盘 Up/Down 保留给输入区。
- loading 时底部使用 `peri-widgets/src/spinner` 的 `SpinnerWidget`，而不是手写 `● Thinking...`。
- 若 TodoWrite 工具写入了 todo list，MessageArea 在 Spinner 下方显示 Todo 列表；Spinner 行本身只显示 spinner verb、elapsed time、token count。

### 2.3 Loading Spinner + Todo

TUI loading 统一使用 `peri-widgets/src/spinner`，包括 `SpinnerState` / `SpinnerMode` / `SpinnerWidget`。禁止在 MessageArea 中手写独立 loading 文案或自造 spinner。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ MessageArea                                                                  │
│                                                                              │
│ ● 我会先梳理当前 TUI 页面结构，然后更新设计文档。                              │
│                                                                              │
│ ⏺ Read 3 files                                                               │
│                                                                              │
│ ◜ 思考中… (12s · ↓ 1.2k tokens)                                              │
│                                                                              │
│   ● 进行中  更新 Workflow Panel 设计                                          │
│   ○ 待处理  补充 spinner + todo 设计图                                        │
│   ○ 待处理  复核快捷键与边框规则                                              │
│   ✓ 已完成  阅读 peri-widgets spinner                                        │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- `SpinnerMode::Thinking`：模型推理中，verb 默认 `思考中…`。
- `SpinnerMode::ToolUse`：工具执行中，verb 默认 `执行工具…`。
- `SpinnerMode::Responding`：回复生成中，verb 默认 `正在生成回复…`。
- `SpinnerWidget::with_theme(theme)` 从 Theme System 获取 accent / muted 颜色。
- Spinner 后缀展示 elapsed time 与 token count，例如 `(12s · ↓ 1.2k tokens)`。
- Todo 列表显示在 Spinner 下方，不嵌入 Spinner 主行。
- Todo 样式沿用 ACP `SessionUpdate::Plan` / IDE plan 组件语义，但不显示额外标题或分隔线；Spinner 下方直接渲染 todo list。
- Todo 列表与 TodoWrite 工具状态挂钩：`in_progress` 显示 `● 进行中`，`pending` 显示 `○ 待处理`，`completed` 显示 `✓ 已完成`。
- Todo 文本优先显示每项的 `activeForm`；若缺失再显示 `content` 的短文本。
- Todo 列表最多展示当前 in-progress、接下来的 2-3 个 pending、最近 1 个 completed；超出数量用 `+N more` 折叠。
- Todo 数据来自 ACP-only data flow：TodoWrite 工具结果映射为标准 `SessionUpdate::Plan`；若标准通道不足，再通过 `peri/unstable-event` 推入 TUI store，MessageArea 只消费派生后的 todo view state。

## 3. InputArea 输入区域组件

InputArea 是 TUI 的核心交互组件，承载文本编辑、4 种叠加模式、键盘事件分发和跨平台兼容层。底层依赖 `tui_textarea::TextArea` 提供完整的编辑器基元（光标控制、选择、剪切/粘贴等）。

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
8. AskUser popup
9. Rewind popup
10. HITL popup
11. 主匹配块（Ctrl+C、Esc、↑/↓、Ctrl+V、Tab、Enter、Ctrl+U/D、Delete）
12. tui_textarea 通用输入（字符插入、光标移动、退格、Delete、词级操作）
```

### 3.2 默认输入框

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ──────────────────────────────────────────────────────────────────────────── │
│   > 帮我实现一个功能，并写测试                                               │
│ ──────────────────────────────────────────────────────────────────────────── │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 多行 buffer，`Shift+Enter` / `Alt+Enter` 插入换行。
- `Enter`（无修饰键）提交消息并写入输入历史。
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
  - 插件 skill 名称（模糊匹配）
  - ACP agent 命令
- 补全行为：`hint_complete()` 仅替换 `/token` 段，插入 `/名称 `，其余文本不丢失。
- ↑/↓ 导航候选，Enter 确认选中项（默认选中第一个），Tab 循环候选，Esc 取消。
- 面板命令（如 `/model`）直接映射到 `PanelKind`。
- 远端命令如 `/bg`、`/clear`、`/compact`、`/rewind` 交给 ACP server。

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

## 5. PanelOverlay 面板容器

`PanelOverlay` 是 `SessionColumn` 内部组件，不是 AppShell 根级浮层。它固定插在 `MessageArea` 和 `InputArea` 之间；当任意 panel 打开时，`InputArea(hidden: true)`，避免输入区抢焦点。Panel 与 InputArea 统一为上下边框样式：只有 top/bottom border，没有 left/right border。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ SessionColumn                                                                │
│ ──────────────────────────────────────────────────────────────────────────── │
│ MessageArea                                                                  │
│ ...                                                                          │
│ ──────────────────────────────────────────────────────────────────────────── │
│                          Active Panel                                        │
│                                                                              │
│   panel content                                                              │
│                                                                              │
│ ──────────────────────────────────────────────────────────────────────────── │
│ InputArea（panel open 时 hidden，不参与交互）                                │
│ ──────────────────────────────────────────────────────────────────────────── │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- Panel 水平占满输入区域宽度，参与 `SessionColumn` 垂直布局。
- Panel 与 InputArea 同样只有上下边框；禁止左右边框。
- 面板打开时隐藏 InputArea。
- 互斥组：Settings、Agent、Tools、Info、Thread；同组只保留一个。

## 6. 14 个 Panel 页面设计

统一约束：所有 Panel 只使用上下边框，不使用左右边框；所有 Panel 的数据来源必须可追溯到 ACP standard、`session/query` snapshot 或 `peri/unstable-event` custom event。

### 6.1 Model Panel


```text
────────────────────────────────────────────────────────────────────────
  Model
  Model Alias Selection
  Provider: anthropic

  ❯ Opus       claude-opus-4-20250514
    Sonnet   ✔ claude-sonnet-4-20250514
    Haiku      claude-3-5-haiku-20241022

  Active: Sonnet
  Model ID: claude-sonnet-4-20250514
  Effort: high
  Max Tokens: 64000
  1M Context: OFF

  ↑/↓::navigate Enter::select · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：选择 active model alias，并同步 ACP service snapshot 与状态栏。数据来源：ACP service snapshot / config snapshot；变更通过统一 config action 返回 snapshot。

### 6.2 Login Panel


```text
──────────────────────────────── Login ────────────────────────────────
  2 providers configured
  Enter::activate · Esc::close

  > ✔ default  (anthropic)
      api key: configured
      base url: https://...

    openai  (openai)
      api key: missing
      base url: https://...

  ↑/↓::navigate Enter::activate · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示 provider 列表、API key 配置状态，Enter 激活 provider。数据来源：ACP service snapshot / config snapshot；敏感值只显示 configured/missing，不展示 secret。

### 6.3 Agent Panel


```text
──────────────────────────────── Agent ────────────────────────────────
  Current Agent Session
  ----------------------

  > Provider:          anthropic (default)
    Model:             sonnet (alias: sonnet)
    Permission Mode:   auto-mode
    CWD:               /Users/.../perihelion
    Messages:          36 committed / 2 current
    Total Messages:    38

  SubAgents
    coder        ✓     修改文档
    reviewer     ●     审查变更

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：只读展示当前会话元信息和从 ACP view models 派生的 SubAgent 状态。数据来源：ACP session state、message pipeline 和 subagent event/view models。

### 6.4 Hooks Panel


```text
──────────────────────────────── Hooks ────────────────────────────────
  5 hooks registered
  (read-only — configured via plugins)

  > 1. pretooluse        Before tool execution
       plugin: security-guard
       matcher: Bash

    2. posttooluse       After tool execution
       plugin: telemetry
       matcher: *

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：只读展示插件声明的 hooks、事件说明、来源和匹配器。数据来源：`session/query` hooks summary 或 `peri/unstable-event` 的 `hooks-snapshot`。

### 6.5 Config Panel


```text
──────────────────────────────── Config ───────────────────────────────
  Configuration (persisted to ~/.peri/settings.json)

  > Show Diff          [ON]
    Cache Warning      [ON]
    Streaming Mode     streaming      < block | none >
    1M Context         [OFF]
    Language           zh             < en | zh >
    Active Alias       sonnet         < opus | sonnet | haiku >
    Permission Mode    auto-mode      < default | accept-edit | ... >

  ↑/↓::navigate Space/Enter::toggle · ←/→::cycle · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：编辑核心 PeriConfig 字段；permission mode 写运行时共享状态，其余配置持久化到 `~/.peri/settings.json`。数据来源：ACP config snapshot；变更通过 config action 后返回新 snapshot，UI 不直接读写配置文件。

### 6.6 Thread Browser Panel


```text
─────────────────────────────── Threads ───────────────────────────────
  Recent Threads
  Enter::open-thread · Esc::close

  > 2026-07-03  TUI 页面能力设计
      id: 01J...   42 messages   perihelion

    2026-07-02  修复 panel overlay 白屏
      id: 01J...   18 messages   perihelion

    2026-07-01  v2 stages cutover
      id: 01J...   96 messages   perihelion

  ↑/↓::navigate Enter::open · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：浏览历史 thread，选择后切换当前会话上下文。数据来源：ACP session list / thread summary query；切换通过 `SwitchSession` effect 与 ACP session open。

### 6.7 MCP Panel


```text
───────────────────────────── MCP Servers ─────────────────────────────
  Project: /Users/.../perihelion

  > ✓ filesystem        stdio   tools 8    resources 2
    △ langfuse          http    tools 12   oauth needed
    ✗ slack             sse     tools 0    reconnect failed
    ◯ browser           http    disabled

  Detail: langfuse
    transport: http
    source:    project
    tools:     trace-list, trace-get, score-create, ...
    resources: prompts, datasets

  Actions
    > View tools/resources
      Re-authenticate OAuth
      Clear OAuth credentials
      Reconnect
      Disable server

  ↑/↓::navigate · Enter::detail/execute · Esc::back/close
────────────────────────────────────────────────────────────────────────
```

能力：MCP Panel 是深度操作面板，不是只读摘要。旧版本包含 ServerList / ServerDetail 两层：列表按 project/user/plugin 来源分组，详情展示 status、transport、source、tools、resources，并提供 ViewTools、ReAuthenticate、ClearAuth、Reconnect、Disable/Enable 等操作。v2 需要保留这些深度操作，但迁移到 ratatui-kit component，并通过 ACP-only data flow 获取 `mcp-snapshot` 与操作结果事件。

### 6.8 Plugin Panel


```text
─────────────────────────────── Plugins ───────────────────────────────
  [Installed]  Discover  Marketplaces  Errors

  > ✓ frontend-design      user     v1.0.0   skills 1  commands 0
    ✓ supergoal            user     v0.6.1   skills 1  agents 0
    ✗ broken-plugin        project  v0.2.0   load error
    ◯ mcp-github           user     v0.4.1   disabled

  Detail: frontend-design
    marketplace: claude-plugins-official
    author:      Anthropic
    path:        ~/.claude/plugins/...
    skills:      frontend-design
    commands:    -
    agents:      -
    mcp:         -

  Actions
    > Disable plugin
      Uninstall
      Back to plugin list

  Tab::next-view · Shift+Tab::prev-view · ↑/↓::navigate · Enter::detail/execute · Space::toggle · Esc::back/close
────────────────────────────────────────────────────────────────────────
```

Discover 视图：

```text
─────────────────────────────── Plugins ───────────────────────────────
  Installed  [Discover]  Marketplaces  Errors

  Search: frontend_

  > frontend-design      official   v1.0.0   installs 662k
    ui-polish            community  v0.3.2   installs 8k

  Detail actions: Install user scope / Install project scope / Back
────────────────────────────────────────────────────────────────────────
```

能力：Plugin Panel 是深度操作面板，不是只读插件列表。旧版本包含 Installed / Discover / Marketplaces / Errors 四个 view，支持安装、卸载、启用/禁用、查看详情、搜索 discover、添加 marketplace、删除 marketplace、安装到 user/project scope、展示 load error。v2 顶部使用 tabs 切换 view；详情和操作菜单在面板内部完成，不跳出根布局。数据来源走 ACP-only：`session/query` 获取 plugin summary，动态变化通过 `peri/unstable-event` 的 `plugin-snapshot` / `plugin-action-result` 推入。

### 6.9 Cron Panel


```text
──────────────────────────────── Cron ─────────────────────────────────
  Scheduled Tasks

  > ✓ */15 * * * *     next 12:45:00    检查后台任务状态
    ✓ 0 9 * * 1        next Mon 09:00   生成周报
    ◯ */5 * * * *      paused           检查 workflow 状态

  Detail: */15 * * * *
    id:       cron_abc123
    prompt:   检查后台任务状态
    next:     2026-07-03 12:45:00
    storage:  in-memory

  Actions
    > Toggle enabled
      Delete task
      Confirm delete: Enter::confirm · Esc::cancel

  ↑/↓::navigate · Enter/Space::toggle · Ctrl+D::delete · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：Cron Panel 是深度操作面板，不是只读任务列表。旧版本支持任务列表、启用/禁用切换、删除确认、删除后刷新、空列表引导和鼠标选择。v2 保留这些操作：`Enter/Space` toggle enabled，`Ctrl+D` 进入删除确认，确认后通过 ACP/cron action 更新列表；任务状态仅用 emoji + theme token 显示，避免英文状态噪音。数据来源走 ACP-only：`session/query` 获取 `CronSummary`，变更通过 `peri/unstable-event` 的 `cron-snapshot` / `cron-action-result` 推入。

### 6.10 Status Panel


```text
─────────────────────────────── Status ────────────────────────────────
  Runtime Snapshot

  > Provider          anthropic
    Model Alias       sonnet
    Permission Mode   auto-mode
    CWD               /Users/.../perihelion
    Git Repo          yes
    MCP               ready 3/4
    Plugins           8 loaded
    CPU               12%
    Memory            430MB

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：集中展示 ACP service snapshot 的环境、运行时和资源状态。数据来源：ACP service snapshot；不直接读取 runtime/global state。

### 6.11 Memory Panel


```text
─────────────────────────────── Memory ────────────────────────────────
  4 memory files in ~/.claude/memory
  Enter::edit-in-$EDITOR · Esc::close

  > perihelion-architecture.md   12 KB  2h ago
    tui-traps.md                 4.8 KB  1d ago
    workflow-notes.md            8.1 KB  5d ago
    style-guide.md               2.0 KB  2026-06-01

  ↑/↓::navigate Enter::edit · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示 memory 文件、大小和更新时间；Enter 使用 `$EDITOR` 打开。数据来源：`session/query` memory summary 或 `peri/unstable-event` 的 `memory-snapshot`；编辑动作通过明确 effect 触发。

### 6.12 Tasks Panel


```text
──────────────────────────────── Tasks ────────────────────────────────
  Background Tasks

  > task_001  ●     code-reviewer
      审查刚才生成的 TUI-PAGE.md
      started: 2m ago

    task_000  ✓     coder
      写入 TUI 页面设计文档
      result: available

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：查看后台 Agent 任务状态、任务描述、执行者和结果可用性。数据来源：ACP task/background agent events 或 `peri/unstable-event` 的 `task-snapshot`。

### 6.13 Betas Panel


```text
──────────────────────────────── Betas ────────────────────────────────
  Feature Flags

  > ratatui-kit-ui       ✓
      New kit-based TUI rendering path

    workflow-panel       ✓
      Workflow visibility panel

    theme-system         ○
      Experimental theme loader

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示实验功能开关与说明，用于理解当前 TUI 能力边界。数据来源：ACP service/config snapshot 或 `peri/unstable-event` 的 `feature-flags-snapshot`。

### 6.14 Workflow Panel


```text
────────────────────────────── Workflow ───────────────────────────────
  [● run_01JZ]  ✓ run_01JY  ✗ run_01JX
────────────────────────────────────────────────────────────────────────
  Phase           Agents
  ─────────────   ─────────────────────────────────────────────────────
  > ✓ Design      ● coder-2     sonnet   128k tok   14 tools
    ● Build       ✓ coder-1     haiku     42k tok    8 tools
    ○ Verify      ○ reviewer    sonnet     0 tok     0 tools
    ○ Ship        ○ verifier    sonnet     0 tok     0 tools
                  ✗ smoke-test  haiku     10k tok    3 tools

  Tab::next-workflow · Shift+Tab::prev-workflow · ↑/↓::navigate · ←/→::pane · Enter::inspect · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示多个 workflow run 的可切换工作台。顶部 tabs 切换不同 workflow run，状态 emoji 放在文本之前。主体左右分栏，左侧为 Phase，右侧为 Agents，宽度比例固定为 2:8；Workflow Panel 不显示 `Selected Phase` / `Selected Agent` 详情区，避免重复信息。Agents 列必须展示 agent 名称、模型、token 用量、工具调用数。所有状态必须以 emoji + theme status token 同时区分，列表中只显示 `✓`、`●`、`○`、`✗`，不要重复显示英文状态。数据源必须来自 ACP-only flow：JSON-RPC notification method 为 `peri/unstable-event`，其中 `event` 为 `workflow-snapshot`；payload 在 `WorkflowRunListDto` 基础上扩展 phase/agent 运行态。
## 7. PopupOverlay 弹窗页面设计

### 7.1 HITL Permission Popup

```text
┌────────────────────────── Permission Request ──────────────────────────┐
│ Tool wants to run                                                       │
│                                                                        │
│  Bash                                                                   │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ cargo test -p peri-tui --lib                                      │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  [Allow once]   [Allow session]   [Deny]                               │
│                                                                        │
│  Enter::confirm · ←/→::choose · Esc::deny                         │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示工具名和输入参数，支持用户审批或拒绝工具执行。

### 7.2 AskUser Popup

```text
──────────────────────────── Ask User ────────────────────────────
  [布局方案]  启用能力  备注
──────────────────────────────────────────────────────────────────
  请选择布局方案

  ○ 单列聊天优先
    适合窄屏和默认工作流

  ● 抽屉面板
    面板插入消息流底部，输入区隐藏

  ○ 双栏监控
    适合长期运行任务

  Tab::next-question · Shift+Tab::prev-question · ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
──────────────────────────────────────────────────────────────────
```

多问题切换状态：

```text
──────────────────────────── Ask User ────────────────────────────
   布局方案  [启用能力]  备注
──────────────────────────────────────────────────────────────────
  是否启用实验能力

  ☑ Theme System v2
  ☐ Side Inspector
  ☑ ACP custom events

  Tab::next-question · Shift+Tab::prev-question · ↑/↓::navigate · Space::toggle · Enter::submit · Esc::cancel
──────────────────────────────────────────────────────────────────
```

能力：展示 Agent 发起的结构化问题，支持 1-4 个问题批量接收；顶部用 tabs 展示所有问题，当前 tab 展示一个问题内容，通过 `Tab` / `Shift+Tab` 切换问题。每个问题可为单选、多选或自定义输入；整体弹窗只使用上下边框，不使用左右边框。

### 7.3 Rewind Popup

```text
┌──────────────────────────── Rewind Preview ──────────────────────────┐
│  This will remove the latest user turn and derived messages.           │
│                                                                        │
│  Messages to remove                                                    │
│  - user: 修一下这个 bug                                                │
│  - assistant: 我会先复现...                                            │
│  - tool: Bash                                                          │
│                                                                        │
│  Files touched                                                         │
│  - peri-tui/src/kit/input_area.rs                                      │
│                                                                        │
│  [Confirm rewind]                         [Cancel]                    │
└────────────────────────────────────────────────────────────────────────┘
```

能力：在执行 `/rewind` 前展示将被回退的消息和文件影响范围。

### 7.4 OAuth Popup

```text
┌──────────────────────────── OAuth Required ──────────────────────────┐
│  MCP server requires browser authorization.                           │
│                                                                        │
│  Server: langfuse                                                      │
│  URL:    https://...                                                   │
│                                                                        │
│  1. Open URL in browser                                                │
│  2. Complete authorization                                             │
│  3. Return to Peri                                                     │
│                                                                        │
│  Enter::open-or-copy-url · Esc::close                          │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示 MCP OAuth 授权信息，辅助用户完成外部登录流程。

## 8. 面板导航与互斥关系

| 组 | 面板 | 语义 |
|----|------|------|
| Settings | Model / Login / Config | 模型、Provider、运行配置互斥 |
| Agent | Agent / Hooks | Agent 观测与 hook 观测互斥 |
| Tools | MCP / Plugin / Cron / Tasks / Workflow | 外部工具与自动化能力互斥 |
| Info | Status / Memory / Betas | 运行信息、记忆和实验能力互斥 |
| Thread | ThreadBrowser | 会话切换独占 |

## 9. 快捷键设计规范

快捷键是 TUI 的公共 API，v2 之后必须稳定、可发现、可组合。新增或修改快捷键必须先更新本章节，再落地实现。

### 9.1 禁止规则

- **禁止 `Shift+字母`**：终端对大小写和 Shift 修饰的兼容性不稳定，也会与普通输入混淆。
- **禁止裸单字母局部快捷键**：Panel / Popup 内也禁止用 `j/k/q` 做导航或关闭；统一使用方向键导航、`Esc` 关闭。
- **禁止在输入区抢占普通字符**：当 focus 在 InputArea 时，未带 `Ctrl/Alt` 的字符都应进入文本编辑。
- **禁止 PageUp/PageDown**：遵循项目约束，滚动使用鼠标滚轮或显式组合键。
- **禁止同一快捷键多处隐式分发**：所有全局快捷键必须能从注册表追踪到唯一能力。

### 9.2 推荐规则

- **面板打开不占用专用全局快捷键**：Model、Agent、Plugin 等 panel 只能通过 slash command、command palette 或统一入口打开；禁止为单个 panel 分配 `Ctrl+字母`。
- **局部导航使用方向键**：Panel / Popup 已获得 focus 时，使用 `↑/↓/←/→` 导航；不使用 `j/k/h/l`。
- **确认/取消语义固定**：`Enter` 确认，`Esc` 取消或关闭当前最高优先级 UI。
- **Tab 仅用于候选项/焦点遍历**：不得用于提交危险动作。
- **所有快捷键必须以 `detail` 形式展示**：StatusBar hints、面板底部提示和设计图说明统一使用这个格式。

### 9.3 焦点优先级

```text
PopupOverlay
  > InlineCompletion（@mention / slash）
  > PanelOverlay
  > InputArea
  > MessageArea
```

规则：同一个按键事件只允许被最高优先级的激活层消费；消费后不再下传。

## 10. 设计落地注意事项

- 根级 overlay 空态必须返回零尺寸 `Positioned`，避免白屏或挤压布局。
- Panel 空态必须显式使用 `Constraint::Length(0)` 或等价零尺寸约束；不要依赖普通 `View()` 默认 flex 行为。
- 面板打开时应隐藏输入区，避免输入焦点与面板事件处理冲突。
- 消息区只处理鼠标滚轮；键盘导航优先留给输入区或当前面板。
- 新增 PanelKind 的目标形态是 Panel Registry 2.0 单点注册；在迁移完成前，临时实现需同步现有 `panel_types.rs`、`panel_registry.rs`、`panel_overlay.rs`、slash command 和本文档。
