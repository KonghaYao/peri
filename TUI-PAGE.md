# TUI 页面能力设计图 v2

> 状态：v2 设计基线。本文档从当前可落地布局出发，继续承载未来 TUI 架构演进；后续所有 TUI 页面能力设计都应以本文件为入口，先更新设计，再落地代码。

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

- **单一事实源**：消息、面板、弹窗、任务、服务状态都从明确 store 派生，禁止组件私有状态变成业务真相。
- **布局与业务分离**：页面组件只声明布局和视觉；业务动作通过 command/effect 层执行。
- **焦点显式化**：输入区、消息区、面板、弹窗、补全浮层必须有统一 focus model，禁止靠事件处理顺序碰运气。
- **能力插件化**：新增 Panel / Popup / Inline Widget 只注册元数据、渲染器、事件路由，不改散落 switch。
- **可观测优先**：每个长任务、后台任务、工具调用、工作流都在 UI 有稳定位置和状态生命周期。
- **响应式终端**：窄屏、宽屏、高度不足、滚动场景都必须有明确降级策略。
- **视觉一致**：所有页面共用语义 token、间距、边框、选中态、空态、错误态。
- **测试友好**：核心布局和 ViewModel 渲染应可 headless snapshot，不依赖真实终端交互。
- **v2 不妥协升级**：版本更新以最终架构正确性为准，不为兼容临时坏设计牺牲结构；迁移可以分阶段，但目标设计不降级。

### v2 之后的演进方向

```text
Phase A：固化 v2 基线
  - 页面设计图成为所有 TUI 变更的入口
  - 明确 SessionColumn 内部结构不可破坏
  - 为 Panel / Popup / Input / Message 建立统一命名与能力边界

Phase B：建立 Focus Graph
  - Popup > InlineCompletion > Panel > Input > Message
  - Esc / Enter / Tab / arrows 走统一 focus router
  - 每个组件声明自己消费哪些事件

Phase C：Panel Registry 2.0
  - Panel metadata + render + event + command 一处注册
  - 消除新增面板时多文件同步遗漏
  - 支持 panel category、search、pin、recent

Phase D：可组合工作台
  - 默认单列聊天
  - 宽屏可启用 side inspector / task rail / trace rail
  - 长任务与 workflow 有独立观察区域，不挤压主聊天

Phase E：UI Snapshot Verification
  - 关键页面 ASCII/snapshot 固化
  - 每次 TUI 变更自动比较布局退化
  - 对窄屏、宽屏、popup、panel-open 状态做矩阵测试
```

## 设计原则

- **聊天优先**：默认界面始终以消息流为主体，所有能力围绕输入、回放、工具结果和状态反馈展开。
- **抽屉式面板**：14 个 Panel 不是全屏 modal，而是插入消息区底部，打开时隐藏输入区，减少焦点冲突。
- **弹窗高优先级**：HITL、AskUser、Rewind、OAuth 是根级覆盖，优先于面板和输入补全。
- **键盘可达**：面板统一 `j/k` 或方向键导航，`Enter` 确认，`Esc/q` 关闭。
- **透明信息层级**：主文字、次级说明、弱提示、状态色遵循 `TUI-STYLE.md` 的 TEXT/MUTED/DIM 与功能色规范。

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
│  ● Thinking...                                                               │
│                                                                              │
│                                                                              │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ > 输入你的任务...                                                        │ │
│ │ @ mention files    / commands                                           │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│ Auto Mode · perihelion · anthropic/sonnet · CPU 12% · MEM 430MB              │
│                 /: commands | Shift+Enter: newline | Ctrl+K: mode            │
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
│          │  按 Enter / q / Esc 跳过向导，进入主界面                 │         │
│          └──────────────────────────────────────────────────────────┘         │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- Provider 未配置时引导用户进入 Login / Config 或编辑配置文件。
- 可跳过，不阻断进入主界面。
- 已配置时显示当前 Provider 和模型 alias。

## 2. MessageArea 页面组件

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
│                 Enter send   Shift+Enter newline   @ mention files            │
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
│ ● 我会先梳理现有页面组件，然后写入 TUI-PAGE.md。                              │
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
│ ● Thinking...                                                                │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 统一渲染 7 类 `ViewModel`，包括文本、工具、SubAgent、系统事件等。
- 支持 diff 可见性切换，diff 内容自动使用增删行语义色。
- 鼠标滚轮滚动消息区；键盘 Up/Down 保留给输入区。
- loading 时底部显示 `● Thinking...`。

## 3. InputArea 输入页面组件

### 3.1 默认输入框

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ > 帮我实现一个功能，并写测试                                             │ │
│ │                                                                          │ │
│ │ Shift+Enter newline · Enter send · @ mention · / commands                │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 多行 buffer，Shift/Alt+Enter 换行。
- Enter 提交并写入输入历史。
- Up/Down 浏览历史；Esc 退出历史态或关闭局部浮层。
- 字符级光标移动，支持 Home/End、词级跳转、删除词。

### 3.2 @mention 文件选择

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                         ┌──────── @src/ ────────┐                            │
│                         │ > peri-tui/src/kit/...│                            │
│                         │   peri-agent/src/...  │                            │
│                         │   peri-acp/src/...    │                            │
│                         │   docs/architecture.md│                            │
│                         └───────────────────────┘                            │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ > 请阅读 @src/                                                           │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 输入 `@` 激活文件候选列表。
- 按 prefix 过滤文件候选。
- Up/Down 导航，Enter 插入，Esc 取消。

### 3.3 Slash completion

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                         ┌──────── / commands ─────────────┐                  │
│                         │ > /model      Model alias panel │                  │
│                         │   /login      Provider config   │                  │
│                         │   /agents     Subagent info     │                  │
│                         │   /threads    Thread browser    │                  │
│                         │   /compact    Compact context   │                  │
│                         └─────────────────────────────────┘                  │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ > /mod                                                                   │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 行首 `/` 激活命令补全。
- 面板命令直接映射到 `PanelKind`。
- 远端命令如 `/bg`、`/clear`、`/compact`、`/rewind` 交给 ACP server。

## 4. StatusBar 页面组件

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Auto Mode · perihelion · anthropic/sonnet · CPU 12% · MEM 430MB              │
│                 /: commands | Shift+Enter: newline | Ctrl+K: mode | Ctrl+O   │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 第 1 行显示 permission mode、cwd basename、provider/model、CPU、MEM。
- 第 2 行根据状态切换 hints：默认、popup、@mention、slash。
- 第 3 行留空作为视觉缓冲。

## 5. PanelOverlay 面板容器

`PanelOverlay` 是 `SessionColumn` 内部组件，不是 AppShell 根级浮层。它固定插在 `MessageArea` 和 `InputArea` 之间；当任意 panel 打开时，`InputArea(hidden: true)`，避免输入区抢焦点。

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ SessionColumn                                                                │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ MessageArea                                                              │ │
│ │ ...                                                                      │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
│                ┌──────────────── Active Panel ────────────────┐              │
│                │                                              │              │
│                │  panel content                               │              │
│                │                                              │              │
│                └──────────────────────────────────────────────┘              │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ InputArea（panel open 时 hidden，不参与交互）                            │ │
│ └──────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

能力：

- 高度：`min(term_h - 8, 28)`，最低 8 行。
- 水平居中，参与 `SessionColumn` 垂直布局。
- 面板打开时隐藏 InputArea。
- 互斥组：Settings、Agent、Tools、Info、Thread；同组只保留一个。

## 6. 14 个 Panel 页面设计

### 6.1 Model Panel

快捷键：`Ctrl+M`，命令：`/model`。

```text
┌─────────────────────────────── Model ───────────────────────────────┐
│  Model Alias Selection                                               │
│  Provider: anthropic                                                 │
│                                                                      │
│  ❯ Opus       claude-opus-4-20250514                                 │
│    Sonnet   ✔ claude-sonnet-4-20250514                               │
│    Haiku      claude-3-5-haiku-20241022                              │
│                                                                      │
│  Active: Sonnet                                                       │
│  Model ID: claude-sonnet-4-20250514                                  │
│  Effort: high                                                         │
│  Max Tokens: 64000                                                    │
│  1M Context: OFF                                                      │
│                                                                      │
│  j/k) Navigate  Enter) Select  Esc) Close                            │
└──────────────────────────────────────────────────────────────────────┘
```

能力：选择 active model alias，并同步 `SERVICE_SNAPSHOT` 与状态栏。

### 6.2 Login Panel

快捷键：`Ctrl+L`，命令：`/login`。

```text
┌─────────────────────────────── Login ───────────────────────────────┐
│  2 providers configured                                              │
│  Enter) Activate  Esc) Close                                         │
│                                                                      │
│  > ✔ default  (anthropic)                                            │
│      api key: configured                                             │
│      base url: https://...                                           │
│                                                                      │
│    openai  (openai)                                                  │
│      api key: missing                                                │
│      base url: https://...                                           │
│                                                                      │
│  j/k) Navigate  Enter) Activate  Esc) Close                          │
└──────────────────────────────────────────────────────────────────────┘
```

能力：展示 provider 列表、API key 配置状态，Enter 激活 provider。

### 6.3 Agent Panel

快捷键：`Ctrl+G`，命令：`/agent`。

```text
┌─────────────────────────────── Agent ───────────────────────────────┐
│  Current Agent Session                                               │
│  ----------------------                                              │
│                                                                      │
│  > Provider:          anthropic (default)                            │
│    Model:             sonnet (alias: sonnet)                         │
│    Permission Mode:   auto-mode                                      │
│    CWD:               /Users/.../perihelion                          │
│    Messages:          36 committed / 2 current                       │
│    Total Messages:    38                                             │
│                                                                      │
│  SubAgents                                                           │
│    coder        completed     修改文档                               │
│    reviewer     running       审查变更                               │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：只读展示当前会话元信息和从 `VIEW_MODELS` 派生的 SubAgent 状态。

### 6.4 Hooks Panel

快捷键：`Ctrl+H`，命令：`/hooks`。

```text
┌─────────────────────────────── Hooks ───────────────────────────────┐
│  5 hooks registered                                                  │
│  (read-only — configured via plugins)                                │
│                                                                      │
│  > 1. pretooluse        Before tool execution                        │
│       plugin: security-guard                                         │
│       matcher: Bash                                                  │
│                                                                      │
│    2. posttooluse       After tool execution                         │
│       plugin: telemetry                                              │
│       matcher: *                                                     │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：只读展示插件声明的 hooks、事件说明、来源和匹配器。

### 6.5 Config Panel

快捷键：`Ctrl+F`，命令：`/config`。

```text
┌─────────────────────────────── Config ──────────────────────────────┐
│  Configuration (persisted to ~/.peri/settings.json)                  │
│                                                                      │
│  > Show Diff          [ON]                                           │
│    Cache Warning      [ON]                                           │
│    Streaming Mode     streaming      < block | none >                │
│    1M Context         [OFF]                                          │
│    Language           zh             < en | zh >                     │
│    Active Alias       sonnet         < opus | sonnet | haiku >       │
│    Permission Mode    auto-mode      < default | accept-edit | ... > │
│                                                                      │
│  j/k) Navigate  Space/Enter) Toggle  ←/→) Cycle  Esc) Close          │
└──────────────────────────────────────────────────────────────────────┘
```

能力：编辑核心 PeriConfig 字段；permission mode 写运行时共享状态，其余配置持久化到 `~/.peri/settings.json`。

### 6.6 Thread Browser Panel

快捷键：`Ctrl+T`，命令：`/threads`。

```text
┌────────────────────────────── Threads ──────────────────────────────┐
│  Recent Threads                                                      │
│  Enter) Open thread  Esc) Close                                      │
│                                                                      │
│  > 2026-07-03  TUI 页面能力设计                                      │
│      id: 01J...   42 messages   perihelion                           │
│                                                                      │
│    2026-07-02  修复 panel overlay 白屏                               │
│      id: 01J...   18 messages   perihelion                           │
│                                                                      │
│    2026-07-01  v2 stages cutover                                     │
│      id: 01J...   96 messages   perihelion                           │
│                                                                      │
│  j/k) Navigate  Enter) Open  Esc) Close                              │
└──────────────────────────────────────────────────────────────────────┘
```

能力：浏览历史 thread，选择后切换当前会话上下文。

### 6.7 MCP Panel

快捷键：`Ctrl+X`，命令：`/mcp`。

```text
┌──────────────────────────── MCP Servers ────────────────────────────┐
│  MCP Pool: ready   3/4 connected                                     │
│                                                                      │
│  > filesystem  ✔ connected                                           │
│      transport: stdio  tools: 8                                      │
│    langfuse    ✔ connected                                           │
│      transport: http   tools: 12                                     │
│    slack       ✗ failed                                              │
│      transport: sse    tools: 0                                      │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：只读展示 MCP 初始化阶段、连接数量、server 状态、transport 和工具数量。

### 6.8 Plugin Panel

快捷键：`Ctrl+P`，命令：`/plugin`。

```text
┌────────────────────────────── Plugins ──────────────────────────────┐
│  8 plugins loaded                                                    │
│  (read-only — toggle via ~/.claude/plugins/config.json)              │
│                                                                      │
│  > claude-plugins-official v1.0.0                                    │
│      Official plugin collection                                      │
│      ~/.claude/plugins/...                                           │
│                                                                      │
│    supergoal v0.6.1                                                  │
│      Long-running goal automation                                    │
│      ~/.claude/plugins/...                                           │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：只读展示已加载插件、版本、描述和 root 路径。

### 6.9 Cron Panel

快捷键：`Ctrl+R`，命令：`/cron`。

```text
┌─────────────────────────────── Cron ────────────────────────────────┐
│  2 scheduled tasks                                                   │
│  In-memory only; tasks are lost after restart                        │
│                                                                      │
│  > */15 * * * *     next: 12:45                                      │
│      prompt: 检查后台任务状态                                        │
│      id: cron_abc123                                                 │
│                                                                      │
│    0 9 * * 1        next: Monday 09:00                               │
│      prompt: 生成周报                                                │
│      id: cron_def456                                                 │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：查看已注册定时任务、cron 表达式、下次触发时间和 prompt。

### 6.10 Status Panel

快捷键：`Ctrl+S`，命令：`/status`。

```text
┌────────────────────────────── Status ───────────────────────────────┐
│  Runtime Snapshot                                                    │
│                                                                      │
│  > Provider          anthropic                                       │
│    Model Alias       sonnet                                          │
│    Permission Mode   auto-mode                                       │
│    CWD               /Users/.../perihelion                           │
│    Git Repo          yes                                             │
│    MCP               ready 3/4                                       │
│    Plugins           8 loaded                                        │
│    CPU               12%                                             │
│    Memory            430MB                                           │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：集中展示 `SERVICE_SNAPSHOT` 的环境、运行时和资源状态。

### 6.11 Memory Panel

快捷键：`Ctrl+N`，命令：`/memory`。

```text
┌────────────────────────────── Memory ───────────────────────────────┐
│  4 memory files in ~/.claude/memory                                  │
│  Enter) Edit in $EDITOR  Esc) Close                                  │
│                                                                      │
│  > perihelion-architecture.md   12 KB  2h ago                        │
│    tui-traps.md                 4.8 KB  1d ago                       │
│    workflow-notes.md            8.1 KB  5d ago                       │
│    style-guide.md               2.0 KB  2026-06-01                   │
│                                                                      │
│  j/k) Navigate  Enter) Edit  Esc) Close                              │
└──────────────────────────────────────────────────────────────────────┘
```

能力：展示 memory 文件、大小和更新时间；Enter 使用 `$EDITOR` 打开。

### 6.12 Tasks Panel

快捷键：`Ctrl+J`，命令：`/tasks`。

```text
┌─────────────────────────────── Tasks ───────────────────────────────┐
│  Background Tasks                                                    │
│                                                                      │
│  > task_001  running     code-reviewer                               │
│      审查刚才生成的 TUI-PAGE.md                                      │
│      started: 2m ago                                                 │
│                                                                      │
│    task_000  completed   coder                                       │
│      写入 TUI 页面设计文档                                           │
│      result: available                                               │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：查看后台 Agent 任务状态、任务描述、执行者和结果可用性。

### 6.13 Betas Panel

快捷键：`Ctrl+B`，命令：`/betas`。

```text
┌─────────────────────────────── Betas ───────────────────────────────┐
│  Feature Flags                                                       │
│                                                                      │
│  > ratatui-kit-ui       enabled                                      │
│      New kit-based TUI rendering path                                │
│                                                                      │
│    workflow-panel       enabled                                      │
│      Workflow visibility panel                                       │
│                                                                      │
│    theme-system         disabled                                     │
│      Experimental theme loader                                       │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：展示实验功能开关与说明，用于理解当前 TUI 能力边界。

### 6.14 Workflow Panel

快捷键：`Ctrl+W`，命令：`/workflow`。

```text
┌───────────────────────────── Workflow ──────────────────────────────┐
│  Workflow Engine                                                     │
│  Multi-agent orchestration via @peri-workflow CLI                    │
│                                                                      │
│  > Engine:                    @peri-workflow (external CLI)          │
│    Binary:                    peri-workflow                          │
│    Current session sub-agents: 3                                     │
│    Self-check:                Run `which peri-workflow`              │
│                                                                      │
│  Workflows are spawned from agent prompts;                           │
│  progress surfaces here as SubAgent groups in the message stream.    │
│                                                                      │
│  j/k) Navigate  Esc) Close                                           │
└──────────────────────────────────────────────────────────────────────┘
```

能力：说明 workflow 外部 CLI 运行模型，并展示当前会话内可观察的 SubAgent 活跃度。

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
│  Enter confirm · ←/→ choose · Esc deny                                 │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示工具名和输入参数，支持用户审批或拒绝工具执行。

### 7.2 AskUser Popup

```text
┌──────────────────────────── Question ────────────────────────────────┐
│  请选择布局方案                                                       │
│                                                                        │
│  ○ 单列聊天优先                                                        │
│    适合窄屏和默认工作流                                                │
│                                                                        │
│  ● 抽屉面板                                                            │
│    面板插入消息流底部，输入区隐藏                                      │
│                                                                        │
│  ○ 双栏监控                                                            │
│    适合长期运行任务                                                    │
│                                                                        │
│  Space select · Enter submit · Esc cancel                              │
└────────────────────────────────────────────────────────────────────────┘
```

能力：展示 Agent 发起的结构化问题，支持单选/多选和提交。

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
│  Enter open/copy URL · Esc close                                       │
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
- **禁止裸单字母全局快捷键**：例如 `m`、`p`、`q` 不能在全局直接触发页面能力；裸字母属于输入区文本。
- **禁止在输入区抢占普通字符**：当 focus 在 InputArea 时，未带 `Ctrl/Alt` 的字符都应进入文本编辑。
- **禁止 PageUp/PageDown**：遵循项目约束，滚动使用鼠标滚轮或显式组合键。
- **禁止同一快捷键多处隐式分发**：所有全局快捷键必须能从注册表追踪到唯一能力。

### 9.2 推荐规则

- **全局页面能力优先使用 `Ctrl+字母`**：例如 `Ctrl+M` 打开 Model，`Ctrl+P` 打开 Plugin。
- **局部导航使用方向键或 `j/k`**：仅在 Panel / Popup 已获得 focus 时生效，不作为全局快捷键。
- **确认/取消语义固定**：`Enter` 确认，`Esc` 取消或关闭当前最高优先级 UI。
- **Tab 仅用于候选项/焦点遍历**：不得用于提交危险动作。
- **所有快捷键必须在 StatusBar hints 或面板底部提示中可发现。**

### 9.3 焦点优先级

```text
PopupOverlay
  > InlineCompletion（@mention / slash）
  > PanelOverlay
  > InputArea
  > MessageArea
```

规则：同一个按键事件只允许被最高优先级的激活层消费；消费后不再下传。

### 9.4 当前面板快捷键表

| 页面 | 快捷键 | Slash | 主要能力 |
|------|--------|-------|----------|
| Model | Ctrl+M | /model | 切换模型 alias |
| Login | Ctrl+L | /login | 激活 provider |
| Agent | Ctrl+G | /agent | 查看会话和 SubAgent |
| Hooks | Ctrl+H | /hooks | 查看 hooks |
| Config | Ctrl+F | /config | 编辑配置和权限模式 |
| Threads | Ctrl+T | /threads | 切换历史会话 |
| MCP | Ctrl+X | /mcp | 查看 MCP server |
| Plugin | Ctrl+P | /plugin | 查看插件 |
| Cron | Ctrl+R | /cron | 查看定时任务 |
| Status | Ctrl+S | /status | 查看运行状态 |
| Memory | Ctrl+N | /memory | 查看/编辑 memory |
| Tasks | Ctrl+J | /tasks | 查看后台任务 |
| Betas | Ctrl+B | /betas | 查看实验功能 |
| Workflow | Ctrl+W | /workflow | 查看 workflow 能力 |

## 10. 设计落地注意事项

- 根级 overlay 空态必须返回零尺寸 `Positioned`，避免白屏或挤压布局。
- Panel 空态可以返回零高度 View，因为它在 `SessionColumn` 内参与布局。
- 面板打开时应隐藏输入区，避免输入焦点与面板事件处理冲突。
- 消息区只处理鼠标滚轮；键盘导航优先留给输入区或当前面板。
- 新增 PanelKind 时需要同步：`panel_types.rs`、`panel_registry.rs`、`panel_overlay.rs`、slash command、快捷键 hints 和本文档。
