# TUI 页面能力设计图 v2.1

> 最后更新：2026-07-10。状态：v2 设计基线 + 部分能力已落地。本文档从当前可落地布局出发，继续承载未来 TUI 架构演进；后续所有 TUI 页面能力设计都应以本文件为入口，先更新设计，再落地代码。
>
> **近期主要落地能力**：BgTaskArea 组件、ratatui-kit-markdown + bubbles 组件族全量迁移、render_bridge/RENDER_CACHE 退役、ViewModel → TuiRenderUnit 内部化、Compact 通过 session/load replay 重放、用户输入统一事件管道、i18n 全量接入、textarea 软换行/视口跟随/placeholder。

## 目录

- [v2 → 未来最优架构目标](#v2--未来最优架构目标)
- [TUI 事件分发与 MessagePipeline](#tui-事件分发与-messagepipeline)
- [ratatui-kit 范式迁移标注](#ratatui-kit-范式迁移标注)
- [Theme System v2](#theme-system-v2)
- [ACP-only Data Flow](#acp-only-data-flow)
- [设计原则](#设计原则)
- [1. AppShell 根页面](#1-appshell-根页面)
- [2. MessageArea 区域组件](#2-messagearea-区域组件)
  - [2.4 消息渲染样式详细规范](#24-消息渲染样式详细规范)
- [3. InputArea 输入区域组件](#3-inputarea-输入区域组件)
- [4. StatusBar 区域组件](#4-statusbar-区域组件)
- [5. PanelOverlay 面板容器](#5-paneloverlay-面板容器)
- [5b. BgTaskArea 后台任务区域](#5b-bgtaskarea-后台任务区域)
- [6. 15 个 Panel 页面设计](#6-15-个-panel-页面设计)
- [7. PopupOverlay 弹窗页面设计](#7-popupoverlay-弹窗页面设计)
- [8. 面板导航与互斥关系](#8-面板导航与互斥关系)
- [9. 快捷键设计规范](#9-快捷键设计规范)
- [10. 设计落地注意事项](#10-设计落地注意事项)

本文档描述 Peri TUI 的页面、面板、弹窗与局部组件能力。设计以当前 `peri-tui/src/kit/` 架构为基准：`AppShell → SessionColumn → MessageArea + PanelOverlay + BgTaskArea + InputArea`，`StatusBar` 与 `SessionColumn` 同级位于根布局底部。**InputArea、PanelOverlay 和 BgTaskArea 必须在 SessionColumn 内部**：PanelOverlay 位于消息流与输入区之间，BgTaskArea 位于 PanelOverlay 和 InputArea 之间；面板打开时隐藏 BgTaskArea 和 InputArea。PopupOverlay 是 AppShell 根级覆盖层。

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

Phase B：ratatui-kit 组件范式统一 ✅ ~~已基本完成~~
  - 所有旧组件标注 Legacy，并规划迁移到 #[component] / element! / hooks 范式
  - ~~旧组件短期只能作为 adapter 被 ratatui-kit 页面包裹，不允许继续扩展旧 API~~ → 已用 ratatui-kit-markdown + bubbles 组件族全量替代
  - ~~新增页面、Panel、Popup、Inline widget 必须直接使用 ratatui-kit 范式~~ → 已完成 render_bridge/RENDER_CACHE 退役，全量走 bubbles 组件族

Phase C：建立 Focus Graph（~~进行中~~）
  - Popup > InlineCompletion > Panel > Input > Message
  - Esc / Enter / Tab / arrows 走统一 focus router
  - 每个组件声明自己消费哪些事件
  - 已完成：用户输入统一事件管道、slash command SubmitRequest 强类型统一

Phase D：Theme System 1.0（进行中）
  - ~~建立可替换 theme palette：base / semantic / component tokens~~ → ratatui-kit-markdown PaletteProvider 已接入
  - 所有组件只消费 theme token，不直接写 Color / hex / ANSI 色
  - 支持内置主题、用户主题、运行时切换和 snapshot 校验
  - 已完成：i18n 全量接入（LcRegistry + LANG_VERSION atom，50+ FTL key，en/zh-CN）

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

当前 TUI 已从双路径分发模型迁移为统一事件管道。用户输入（LocalUserBubble）走 LOCAL_EVENT_TX → acp_bridge → dispatch_and_notify 统一路径，消除旧旁路。未来 Focus Graph 必须映射到现有 Effect 体系。

```text
ACP Events → acp_notifier → acp_bridge → dispatch_and_notify → VIEW_MODELS atom
                                                            → BG_DISPLAY atom
                                                            → other atoms
Local Events → LOCAL_EVENT_TX → acp_bridge → dispatch_and_notify (same path)
```

关键约束：

- `OpenPanel` / `ClosePanel` / `SubmitMessage` / `PollAgent` / `Render` 等能力都必须落到 Effect，不允许组件直接操作 runtime。
- MessagePipeline 单一数据源为核心数据结构。
- `commit_iteration` 是替换语义；禁止把 finalized messages extend 到 transcript。
- ~~子 Agent 提交不得污染父 Agent transcript~~ → bg SubAgent 使用独立 bg_event_sender。
- 用户输入不再通过旁路写入 RENDER_CACHE，统一走事件管道。

## ratatui-kit 范式迁移标注

v2 之后所有 TUI 组件以 ratatui-kit 为唯一 UI 范式。旧组件可以短期存在，但必须被标注为 Legacy，并通过 adapter 接入 ratatui-kit 树；禁止继续在旧范式上增加新能力。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ratatui-kit Component Tree (current)                                         │
│                                                                              │
│  AppShell #[component]                                                       │
│    ├─ SessionColumn #[component]                                             │
│    │   ├─ MessageArea #[component]                                           │
│    │   │   ├─ bubbles 组件族 (UserBubble/AssistantBubble/ToolCard/           │
│    │   │   │   SystemNote/SubAgentGroup/CollapsedGroup/ReasoningBlock)       │
│    │   │   └─ Vec<Line> 缓存 + 单 Paragraph 渲染                            │
│    │   ├─ PanelOverlay #[component]                                          │
│    │   ├─ BgTaskArea #[component]  ← 新增：后台 Agent 任务状态区             │
│    │   └─ InputArea #[component]                                             │
│    ├─ StatusBar #[component]                                                 │
│    └─ PopupOverlay #[component]                                              │
│                                                                              │
│  ~~Legacy widgets → 已全部消除~~                                             │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 迁移规则（当前状态）

- ✅ 新组件已全部使用 ratatui-kit：`#[component]`、`element!`、props、hooks、context。
- ✅ Legacy `Widget` / 手写 `Frame` 渲染已全部消除——render_bridge/RENDER_CACHE 退役，全量走 bubbles 组件族。
- ✅ ratatui-kit-markdown 替代 peri-widgets markdown（删除 ~1531 行，增量渲染 3.13µs/帧）。
- 事件处理统一走 ratatui-kit component 边界与 Focus Graph，不允许组件私自捕获全局键盘事件。
- Theme、ACP-only data、Panel Registry、Popup Registry 都必须以 ratatui-kit component 为消费端。

### 当前组件迁移标注

| 组件 | 目标状态 | 要求 |
|------|----------|------|
| AppShell | ✅ ratatui-kit native | 根组件，负责组合 SessionColumn / StatusBar / PopupOverlay / BgTaskArea |
| SessionColumn | ✅ ratatui-kit native | InputArea / PanelOverlay / BgTaskArea 必须在内部 |
| MessageArea | ✅ ratatui-kit native | bubbles 组件族渲染 UserBubble/AssistantBubble/ToolCard/SystemNote/SubAgentGroup/ReasoningBlock/CollapsedGroup |
| InputArea | ✅ ratatui-kit native | 手写 textarea 行为通过 props/state 进入组件，支持软换行/视口跟随/placeholder/光标焦点 |
| PanelOverlay | ✅ ratatui-kit native | 所有 Panel 由 registry 注入 component renderer |
| PopupOverlay | ✅ ratatui-kit native | HITL / Rewind / OAuth 由 registry 注入 component renderer |
| BgTaskArea | ✅ ratatui-kit native | 展示后台 Agent 任务状态，数据来自 BG_DISPLAY / BG_AGENT_IDS atom |
| StatusBar | ✅ ratatui-kit native | 只消费 theme token 与 ACP snapshot |
| Spinner | ✅ ratatui-kit native | 在 bubbles 组件族内实现 |
| Todo / Plan | ✅ ratatui-kit native | 显示在 Spinner 下方 |
| Welcome | ✅ ratatui-kit native | 空会话 MessageArea 状态，窄屏降级 |
| SetupWizard | ✅ ratatui-kit native | 首启引导 |
| 15 Panels | ✅ ratatui-kit native | 由 Panel Registry 注入 |
| 4 Popups | ✅ ratatui-kit native | HITL / Rewind / OAuth / AskUser |
| MentionPopup / SlashCompletion | ✅ ratatui-kit native | 与 InputArea 同宽，只使用上下边框 |

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
│  Standard ACP Events                                                         │
│    - session/update (message delta / tool call / done / error)               │
│    - session/prompt (TurnDone with StopReason)                               │
│    - permission / interaction / session events                               │
│                                                                              │
│  Peri Custom Events via `peri/unstable-event`                                │
│    - bg-callback-user-message  → 双通道 flush-then-push                      │
│    - turn-suspended            → bg agent loading 停止                        │
│    - budget-warning            → 上下文预算告警                              │
│    - rewind-preview            → Rewind 预览数据                              │
│    - plugin-snapshot                                                          │
│    - mcp-snapshot                                                             │
│    - cron-snapshot                                                            │
│    - memory-snapshot                                                          │
│    - workflow-snapshot                                                        │
│                                                                              │
│  TUI 内部类型（非 ACP）：TuiRenderUnit（8 变体）替代公有 ViewModel           │
└───────────────────────────────┬──────────────────────────────────────────────┘
                                │ single ingress
                                ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ TUI Event Router                                                             │
│  map ACP event → update atoms (VIEW_MODELS, BG_DISPLAY, ...)                 │
└───────────────────────────────┬──────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ Components                                                                   │
│  MessageArea / PanelOverlay / InputArea / BgTaskArea / PopupOverlay /        │
│  StatusBar                                                                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 数据来源规则

- TUI 组件禁止直接读取 Agent、Middleware、Plugin、MCP、Cron、Workflow 的内部状态。
- TUI 组件禁止直接读配置文件来获得业务真相；配置变化必须通过 ACP snapshot/event 进入。
- ACP service snapshot、panel lists、popup payload、task state 等都必须可追溯到 ACP event。
- 非 ACP 标准字段不得偷塞到 UI 私有状态；必须定义 `peri/unstable-event` 自定义事件 schema。
- TUI store 只做 UI 派生，不拥有业务生命周期。
- 如果某项能力无法从 ACP 标准事件/方法进入，先补 `peri/unstable-event` 自定义事件，再做 UI。
- **bg callback 合成消息**：bg agent 完成后走双通道 flush-then-push（`bg-callback-user-message` unstable event → flush current_turn，`session/update` → push 气泡），emit 点在 agent MQ drain 处保证时序。**禁止从 registry event pump（独立 tokio task）发送 TUI 气泡**。
- Compact 完成后通过 `session/load` 重放压缩历史，不再直接操作 TUI ViewModel 集合。

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

### 自定义事件命名（已落地 + 计划中）

```text
bg-callback-user-message     ← bg agent 回调合成用户消息（双通道 flush-then-push）
turn-suspended               ← bg agent 启动后 Turn suspense 信号
budget-warning               ← 上下文预算告警
rewind-preview               ← Rewind 预览数据
service-snapshot             ← ACP service snapshot
plugin-snapshot              ← Plugin 状态
mcp-snapshot                 ← MCP Server 状态
cron-snapshot                ← Cron 定时任务
memory-snapshot              ← Memory 文件
task-snapshot                ← 后台任务状态（计划）
workflow-snapshot            ← Workflow 运行状态
theme-changed                ← 主题切换通知（计划）
panel-payload                ← Panel 数据推送（计划）
```

### ACP-only 不妥协约束

- 新页面的数据需求必须先写出 ACP event/view model 来源。
- 没有 ACP 来源的页面能力不能落地为正式功能。
- 临时调试可以本地 mock，但必须与 ACP schema 同形。
- 所有 TUI 页面设计图必须标明数据来源：ACP standard / `peri/unstable-event` custom event / `session/query` snapshot。

## 设计原则

- **聊天优先**：默认界面始终以消息流为主体，所有能力围绕输入、回放、工具结果和状态反馈展开。
- **抽屉式面板**：15 个 Panel 不是全屏 modal，而是插入消息区底部，打开时隐藏输入区，减少焦点冲突。
- **弹窗高优先级**：HITL、Rewind、OAuth 是根级覆盖，优先于面板和输入补全。
- **键盘可达**：面板统一使用方向键导航，`Enter` 确认，`Esc` 关闭；禁止 `j/k/q` 这类裸单字母承担导航或关闭能力。
- **透明信息层级**：主文字、次级说明、弱提示、状态色遵循 Theme System 的 TEXT/MUTED/DIM 与功能色规范。
- **边框克制**：InputArea、PanelOverlay、所有 Panel、@mention、Slash completion 只使用上下边框；禁止左右边框，保持聊天主界面横向通透。

## 页面总览

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ AppShell                                                                     │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ SessionColumn                                                            │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ MessageArea                                                              │ │ │
│ │ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │ │
│ │ │ │ MessageArea / Welcome                                                │ │ │ │
│ │ │ │ - bubbles 组件族（UserBubble/AssistantBubble/ToolCard/SystemNote/   │ │ │ │
│ │ │ │   SubAgentGroup/ReasoningBlock/CollapsedGroup）                       │ │ │ │
│ │ │ │ - Vec<Line> 缓存 + 单 Paragraph 渲染                                │ │ │ │
│ │ │ │ - mouse wheel scroll                                                 │ │ │ │
│ │ │ └──────────────────────────────────────────────────────────────────────┘ │ │ │
│ │ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │ │
│ │ │ │ LoadingFooter（固定在 ScrollView 之外）                              │ │ │ │
│ │ │ │ - Spinner 动画（accent 橙色）                                       │ │ │ │
│ │ │ │ - Todo 列表                                                         │ │ │ │
│ │ │ │ - 空态时保留「✻ Brewed for Xm Xs」（灰色 MUTED）                    │ │ │ │
│ │ │ └──────────────────────────────────────────────────────────────────────┘ │ │ │
│ │ └──────────────────────────────────────────────────────────────────────────┘ │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ PanelOverlay：Model/Login/Agent/Hooks/Config/Threads/...             │ │ │
│ │ │ - inside SessionColumn                                               │ │ │
│ │ │ - between MessageArea and InputArea                                  │ │ │
│ │ └──────────────────────────────────────────────────────────────────────┘ │ │
│ │ ┌──────────────────────────────────────────────────────────────────────┐ │ │
│ │ │ BgTaskArea：后台 Agent 任务状态区                                     │ │ │
│ │ │ - inside SessionColumn, 位于 PanelOverlay 和 InputArea 之间          │ │ │
│ │ │ - 展示后台 Agent 名称/状态/耗时                                      │ │ │
│ │ │ - 空态时高度收缩为 0                                                 │ │ │
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
│ PopupOverlay：Hitl / Rewind / OAuth，居中覆盖，优先级最高           │
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
│  ◜ 思考中… (12s · ↓ 1.2k tokens)                                             │
│    ◼ 进行中  设计 Workflow Panel                                              │
│                                                                              │
│  ● agent (coder)  修改文档                                                    │
│    N tool calls, running 2min 15s                                            │
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

- Panel 宽度全铺满终端：外层 View 和内层 View 均使用 `Constraint::Fill(1)`，不再使用固定 60 列约束（`panel_registry::panel_constraint(layout.width)` 已废弃宽度维度的固定尺寸）。
- Panel 与 InputArea 同样只有上下边框；禁止左右边框。
- 面板打开时隐藏 InputArea。
- 互斥组：Settings、Agent、Tools、Info、Thread；同组只保留一个。

## 5b. BgTaskArea 后台任务区域

`BgTaskArea` 是 `SessionColumn` 内部组件，位于 PanelOverlay 和 InputArea 之间。当面板打开时，BgTaskArea 和 InputArea 一起隐藏。数据来自 `BG_DISPLAY` 和 `BG_AGENT_IDS` atom，由 `dispatch_and_notify` 在 SubagentStarted/SubagentDone 事件时写入。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ SessionColumn                                                                │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ MessageArea                                                                  │
│ ...                                                                          │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ BgTaskArea                                                                   │
│                                                                              │
│  ● coder (bg)  修改 TUI-PAGE.md                                              │
│    running 2min 15s                                                          │
│                                                                              │
│  ✓ reviewer (bg)  审查 agent 模块                                            │
│    completed 45s                                                             │
│                                                                              │
│ ═══════════════════════════════════════════════════════════════════════════  │
│ InputArea（panel open 或 bg task 区无内容时 hidden）                         │
│ ═══════════════════════════════════════════════════════════════════════════  │
└──────────────────────────────────────────────────────────────────────────────┘
```

### BgTaskArea 视觉规格

| 属性 | 规格 |
|------|------|
| 前缀 | `●`（running），`✓`（completed），`✗`（failed） |
| 状态色 | running → `success`（绿色动画 800ms 闪烁），completed → `success`，failed → `error` |
| Agent 名称 | `agent_type (bg)`（`text` 色） |
| 任务预览 | `muted` 色，截断 |
| 耗时 | `"running Xmin Xs"` / `"completed Xs"`（`muted` 色） |
| 空态 | 无后台 Agent 时高度收缩为 0 |
| 边框 | 与 Panel/InputArea 统一，只用上下边界分隔线（`border_dim`） |

能力：

- 展示所有活跃的后台 SubAgent 状态（名称、描述、耗时）。
- bg agent 启动时通过 `SubagentStarted(is_background: true)` 事件添加条目。
- bg agent 完成/失败时通过 `SubagentDone` 事件更新条目状态。
- `Ctrl+B` 跳转焦点到 BgTaskArea（从 InputArea）。
- 空态不占用布局空间。

## 6. 15 个 Panel 页面设计

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

技术实现：ThreadBrowser 采用手动渲染模式（仿 Login 面板），不再使用 VirtualList。VirtualList 在 `panel_shell!` 的 `border` 内 `Fill(1)` 会被 ratatui 解析为 0，导致不可见。当前实现：`Vec<Line>` → `Paragraph` → `ScrollView(Text)`，手动处理 ↑/↓/Enter 键盘事件，条目间有空行分隔，选中行使用 `>` 标记 + bold 高亮。

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

### 7.2 AskUser Panel

用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联在 MessageArea 和 InputArea 之间渲染（与 Thread Browser 等面板一致）。Tab 键在问题间切换，当前问题显示选项列表，Space 选中/取消，Enter 跳到下一个未确认问题或全部答完后提交，Esc 取消并标记失败。

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

  Tab::next-question · ↑/↓::navigate · Space::select · Enter::next · Esc::cancel
──────────────────────────────────────────────────────────────────
```

多问题已全部确认时：

```text
──────────────────────────── Ask User ────────────────────────────
  布局方案 ✓  启用能力 ✓  备注 ✓
──────────────────────────────────────────────────────────────────
  备注

  ○ 选项 A
  ● 选项 B

  Tab::next-question · ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
──────────────────────────────────────────────────────────────────
```

能力：展示 Agent 发起的结构化问题，支持 1-4 个问题批量接收；顶部用 tabs 展示所有问题，已回答项旁显示 ✓；当前 tab 展示一个问题内容，通过 `Tab` / `Shift+Tab` 切换。每个问题可为单选（○/●）或多选（☐/☑）；面板打开时隐藏 InputArea，与其他 Panel 行为一致。

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
