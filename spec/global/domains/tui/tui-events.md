# TUI ACP 事件系统

> 本文档描述 TUI 的事件分派体系：ACP 事件管线、acp_bridge 桥接、acp_events 视图模型推送、VIEW_MODELS 原子状态管理。

---

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

## TUI 事件分发管线

TUI 已从双路径分发模型迁移为统一事件管道：ACP 通知与本地事件共用 `dispatch_and_notify` 单一入口，消除旧旁路。用户输入（LocalUserBubble）走 LOCAL_EVENT_TX → acp_bridge → dispatch_and_notify 统一路径；v2 内部事件经 v2_bridge 直连桥映射后复用同一 bridge 通道。

```text
ACP Events   → acp_notifier → bridge_tx ─┐
                                          ├→ acp_bridge → dispatch_and_notify → VIEW_MODELS atom
v2 Events    → v2_bridge    → bridge_tx ─┘                                    → BG_DISPLAY atom
Local Events → LOCAL_EVENT_TX → bridge_tx（同路径）                           → other atoms
```

### 事件分派实现（acp_events/ 子模块拆分）

`dispatch_and_notify`（`peri-tui/src/kit/acp_events/mod.rs`）已按事件类别拆分为 9 个文件的 match 骨架 + 子模块 handler（原 acp_events.rs 巨型单函数拆分，issue P1-1 落地）：

```text
peri-tui/src/kit/acp_events/
  mod.rs       — dispatch_and_notify 入口 + BridgeState + 辅助函数（inject_system_note 等）
  streaming.rs — TextChunk / ReasoningChunk 流式增量
  turn.rs      — PromptStarted/Submitted、TurnDone/Interrupted/Suspended、Replay 边界、LocalUserBubble
  tool.rs      — ToolStarted/Ended、ToolCount、Progress、ReplayToolStarted/Ended
  compact.rs   — CompactStarted/Completed/Error
  system.rs    — BudgetWarning/SystemNotification/Prediction/Hitl/AskUser/Rewind/Oauth/Plugin/BgTask
  subagent.rs  — SubagentStarted/Stopped
  agent.rs     — BackgroundTaskCompleted / AgentExecutionFailed
  render.rs    — push_view_models / push_acp_state 渲染辅助
```

子模块 handler 统一签名 `fn handle_xxx(state: &mut BridgeState, ...)`；新增事件变体只需在 `mod.rs` 加一个 match arm + 对应子模块 handler，可对单个 handler 做单元测试。

### 桥接与通道

- **acp_bridge.rs**：`spawn_acp_bridge` 后台 task 从 bridge_tx 消费 `AcpEventWithEpoch`，维护 `BridgeState`，每次事件后写入全局 atom；`apply_bridge_reset` 响应 `BRIDGE_RESET_COUNTER` 变更（/clear、thread 切换时清空 committed/current_turn/INPUT_BUFFER，防旧 session 残留）。
- **acp_notifier.rs**：解码 ACP session/update 通知 → 桥接事件 → bridge_tx；`is_session_replay` 检测链兼容 `_meta` / `meta` 两种序列化 key。
- **v2_bridge.rs**：v2 事件直连桥——消费 `RenderEvent` / `StateEvent` / `ObserveEvent`，映射为 AcpEventData 后推入 bridge_tx（与 ACP 路径双轨；SubAgent 生命周期事件有意排除，走 event_sink → acp_notifier 路径）。
- **atoms.rs**：全局 atom 定义替代部分 Effect 变体；`LOCAL_EVENT_TX`（input_area 本地提交通道）、`VIEW_MODELS`、`BG_DISPLAY`、`BG_AGENT_IDS`、`BRIDGE_RESET_COUNTER`、`INPUT_BUFFER` 等。
- **submit_consumer.rs**：单消费者从 `SUBMIT_TX` 顺序读取 `SubmitRequest`，转 ACP prompt 请求；承担首次会话懒初始化。

### 关键约束

- `SubmitRequest` 强类型（`OpenPanel` / `SessionControl` / `ViewAction` / `AgentText` / `KeepGoing`）经 `parse_submit_request` 统一解析后由 submit_consumer 消费；local panel 请求在 input_area 本地直接分发，组件不允许直接操作 ACP runtime。
- 提交与取消、会话加载及交互响应经 ACP client/transport 发送；组件只订阅和渲染状态，不能在 render 中驱动 Agent。
- ~~子 Agent 提交不得污染父 Agent transcript~~ → bg SubAgent 使用独立 bg 事件通道。
- 用户输入不再通过旁路写入 RENDER_CACHE，统一走 LOCAL_EVENT_TX → dispatch_and_notify 管道（render_bridge/RENDER_CACHE 已退役）。

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
| 16 Panels | ✅ ratatui-kit native | 由 Panel Registry 注入（PanelKind 16 变体，含 Theme） |
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
│    - rewind-preview            → Rewind 预览数据（已退役，Rewind v2 实时查询）│
│    - plugin-snapshot / plugin-action-result / plugin-search-result              │
│    - bg-task-snapshot / bg-task-started / bg-task-completed / bg-task-cancelled │
│    - service-snapshot / workflow-snapshot（TUI 侧轮询派生，非 unstable event） │
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
- 完整事件目录以 `docs/design/peri-acp-protocol.md` 为准；本文档只记录 TUI 侧页面数据需求。

### 自定义事件命名（已落地 + 计划中）

已落地（`peri-tui/src/kit/acp_types.rs` 的 `AcpEventData::decode` 按事件名分发）：

```text
turn-done / turn-interrupted / turn-suspended   ← turn 生命周期（turn-suspended：bg agent 启动后 Turn suspense 信号）
tool-count / progress                            ← 工具进度
budget-warning                                   ← 上下文预算告警
system-notification                              ← 系统通知
prediction / file-suggestions                    ← 输入辅助（预测文本、文件候选）
rewind-preview / rewind-completed                ← Rewind（rewind-preview 已退役：Rewind v2 候选改由打开面板时实时查询，变体保留以向后兼容旧服务端）
oauth-needed                                     ← OAuth 授权
subagent-started / subagent-stopped              ← SubAgent 生命周期
bg-task-started / bg-task-completed / bg-task-cancelled / bg-task-snapshot
bg-callback-user-message                         ← bg agent 回调合成用户消息（双通道 flush-then-push，emit 点在 agent MQ drain 处）
plugin-snapshot / plugin-action-result / plugin-search-result
```

TUI 侧轮询派生（非 unstable event——由后台轮询 task 拉取并写入 atom）：

```text
service-snapshot   ← spawn_service_snapshot 周期轮询（SERVICE_SNAPSHOT atom，2s；MCP/Cron/Memory 面板数据由它派生）
workflow-snapshot  ← workflow/list_runs 轮询（WORKFLOW_SNAPSHOT atom，2s）
```

计划中（尚未落地）：

```text
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
- **抽屉式面板**：16 个 Panel 不是全屏 modal，而是插入消息区底部，打开时隐藏输入区，减少焦点冲突。
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
│ │ │ InputArea：multiline prompt + @mention + slash completion            │ │ │
│ │ │ - inside SessionColumn                                               │ │ │
│ │ │ - hidden while panel is open                                         │ │ │
│ │ └──────────────────────────────────────────────────────────────────────┘ │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ StatusBar：permission · cwd · provider/model + hints（CPU/MEM/ctx→composer）│ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ BgTaskArea：后台任务状态区，每行一个 agent，格式　● coder  desc  2m15s    │ │
│ │ - 位于 AppShell 根层 StatusBar 下方                                      │ │
│ │ - 空态时高度收缩为 0                                                     │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│ PopupOverlay：Hitl / Rewind / OAuth / AskUser，居中覆盖，优先级最高           │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Issue 经验附录

### issue_2026-07-08-tui-drop-acp-messageid-boundary
**摘要:** TUI 丢弃 ACP agent_message_chunk 的 messageId，消息边界靠推断而非协议字段
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** messageId, ContentSegment 推断, TuiTextChunk, 消息边界
**问题本质:** TUI 解码 `session/update` → `agent_message_chunk` 时只提取 `content.text`，忽略协议自带的 `messageId`；`CurrentTurn` 只能靠 ContentSegment 变体切换推断消息边界，事件顺序异常时推断失效。
**通用模式:** 协议自带字段优先于本地推断——消息边界必须透传 `message_id`（`TuiTextChunk` / `TuiReasoningChunk` 增加 `message_id: Option<String>`，`CurrentTurn` 以 `last_message_id` 检测边界新建段）。
**涉及文件:** peri-tui/src/kit/stream_data.rs, peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_types.rs（TurnSegment 交错追踪 + last_message_id）；协议类型 `ContentChunk` 定义于 agent-client-protocol crate（schema::v1，原 peri-acp-types/src/message.rs 已不存在，协议类型迁至外部 agent-client-protocol 依赖）
**CLAUDE.md 链接:** false

### issue_2026-07-11-cancel-no-rollback-no-restore
**摘要:** Ctrl+C 取消后未回滚用户消息、未恢复文本到输入框
**状态:** fixed
**归档日期:** 2026-07-17
**关键词:** TurnInterrupted, 零产出回滚, last_submitted_text, INPUT_RESTORE_TEXT
**问题本质:** kit 单路径迁移（Phase 2.6）回归——v1 的完整回滚逻辑在迁移到 ratatui-kit 后丢失；`TurnInterrupted` 处理器缺少零产出回滚分支（`current_turn.is_empty() && last_submitted_text.is_some()`）。
**通用模式:** 取消类事件必须同时回滚视图与输入状态；render body 禁止写 atom，回滚文本用非 atom 存储（`OnceLock<Mutex<Option<String>>>`）并由 render 后 effect 消费。
**涉及文件:** peri-tui/src/kit/acp_events/turn.rs（handle_turn_interrupted，原 acp_events.rs 已拆分为 acp_events/ 子模块）, peri-tui/src/kit/acp_events/mod.rs（BridgeState.last_submitted_text）, peri-tui/src/kit/atoms.rs（INPUT_RESTORE_TEXT）, peri-tui/src/kit/input_area.rs（use_effect 消费恢复文本）, peri-tui/src/kit/acp_bridge.rs, peri-tui/src/acp_server/prompt.rs（过期注释已修正）
**CLAUDE.md 链接:** false

### issue_2026-07-05-message-flow-render-sync-freeze
**摘要:** 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** 渲染同步, loading 卡死, INPUT_BUFFER, history 草稿, RENDER_CACHE
**问题本质:** 本地提交直接写 `VIEW_MODELS.committed` 但不刷新渲染缓存；prompt RPC 失败后 loading 无法回落（`is_loading` 多方写入缺唯一事实源）；history 浏览回到底部时无法恢复空输入（草稿仅在非空白时保存）。
**通用模式:** 本地提交必须与 ACP 事件走同一事件管道（LocalUserBubble → LOCAL_EVENT_TX → dispatch_and_notify）；loading 必须有唯一事实源；history 浏览必须能回到空输入态（草稿始终保存，包括空串）。
**涉及文件:** peri-tui/src/kit/input_area.rs, peri-tui/src/kit/input_history.rs, peri-tui/src/kit/submit_consumer.rs, peri-tui/src/kit/message_area/（原 message_area.rs 已拆为目录）；render_bridge.rs 已退役（RENDER_CACHE 移除，本地回显改走 LOCAL_EVENT_TX 统一管道）
**CLAUDE.md 链接:** false

### issue_2026-07-18-duplicate-streaming-text-and-tool-cards
**摘要:** 流式输出时文本和工具调用卡片重复显示（render 事件双轨扇出）
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 双轨扇出, forwarder, render 事件, v2_bridge, 重复渲染
**问题本质:** render 事件经两条路径进入 VIEW_MODELS——同一内容被扇出到两个映射路径后各渲染一次，文本与工具卡片紧挨着重复出现。
**通用模式:** 事件必须单入口；同一 `AcpEventData` 只能有一个产生者；新增事件源（如 v2 直连桥）时检查是否与既有路径重复映射。
**涉及文件:** peri-acp/src/event/forwarder.rs（render 事件→ExecutorEvent 映射与扇出）, peri-tui/src/kit/acp_events/（原 acp_events.rs 已拆分，渲染映射位于 render.rs / streaming.rs）, peri-tui/src/kit/v2_bridge.rs（v2 事件直连桥，与 ACP 路径双轨的边界）
**CLAUDE.md 链接:** false

### issue_2026-07-08-history-replay-missing-tool-interactions
**摘要:** History 面板恢复的对话历史缺少工具调用和工具结果，消息内容与原始对话不一致
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** session replay, ReplayToolStarted/Ended, CommittedAssistantText, _meta 序列化
**问题本质:** replay 事件硬编码为 Replay* 气泡旁路写入 committed，绕过 current_turn 管道；且 `#[serde(rename = "_meta")]` 使序列化 key 带下划线，`is_session_replay` 检测只查无下划线 key 导致永远 false。
**通用模式:** replay 必须复用正常消息流路径；协议字段检测需与实际序列化 key（含下划线）一致（四级 fallback：`_meta` → `meta` → `content._meta` → `content.meta`）；AI 工具调用需按 content blocks + tool_calls 字段去重；空工具输出不得跳过。
**涉及文件:** peri-tui/src/kit/acp_notifier.rs（is_session_replay 四级 fallback）, peri-tui/src/kit/acp_types.rs（CommittedAssistantText / ReplayToolStarted / ReplayToolEnded 变体）, peri-tui/src/kit/acp_events/turn.rs 与 tool.rs（原 acp_events.rs 拆分后的对应 handler）, peri-tui/src/kit/acp_bridge.rs（event_kind_short）, peri-acp/src/dispatch/session_replay.rs（replay_session_history 逐 content block 分发）
**CLAUDE.md 链接:** false


---

> [返回总索引](tui-index.md)
