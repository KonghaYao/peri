# TUI 面板系统

> 本文档描述 PanelOverlay 面板容器及 16 个 Panel 的完整设计规范，包括面板导航、互斥关系、快捷键约定。

---

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

#### 当前实现状态（H1c · Iteration 14）

当前代码为**极简只读列表**，与 v2 设计差距较大：

```
实际渲染结构 (peri-tui/src/kit/panels/plugin.rs:25, 60×18 固定尺寸):
┌─ [ Plugin ] ────────────────────────────────────────┐
│ N 个插件已加载                                       │
│ (只读 — 通过 config.json)                            │
│ > frontend-design v1.0.0                            │
│     Create production-grade frontend interfaces     │
│     ~/.claude/plugins/...                           │
│ supergoal v0.6.1                                    │
│     Plan and autonomously build tasks               │
│     ~/.claude/plugins/...                           │
│ ↑/↓ navigate Enter open Esc close                   │
└──────────────────────────────────────────────────────┘
```

关键现状：
- **无 Tabs**：仅单一 Installed 列表视图，Discover/Marketplaces/Errors 未实现
- **无操作**：仅 ↑↓ 导航 + Esc/Enter 关闭面板，安装/卸载/启用/禁用/搜索均未实现
- **静态数据**：`PLUGIN_LIST` atom 从启动时 `launch.rs` 派生一次（`PluginLoadResult.plugins → SnapshotSource.plugins → PLUGIN_LIST`），`service_snapshot` 2s tick 不刷新此字段
- **数据模型**：`PluginSummary { name, version, enabled, root, description }` (atoms.rs:124)，enabled 字段已定义但面板未使用
- **视口**：每插件固定 4 行（名称+描述+路径+空行），可见 3 个插件（18 - 6 行开销 = 12 行 ÷ 4），选中项保持在上 1/3

#### 能力总述

Plugin Panel 目标为深度操作面板，不是只读插件列表。包含 Installed / Discover / Marketplaces / Errors 四个 view，支持安装、卸载、启用/禁用、查看详情、搜索 discover、添加 marketplace、删除 marketplace、安装到 user/project scope、展示 load error。v2 顶部使用 tabs 切换 view；详情和操作菜单在面板内部完成，不跳出根布局。数据来源走 ACP-only：`session/query` 获取 plugin summary，动态变化通过 `peri/unstable-event` 的 `plugin-snapshot` / `plugin-action-result` 推入。

#### v2 实现方案（分 3 阶段）

**Phase 1 — 面板结构升级（≈ 200 行 insert）**

目标：把只读单视图升级为 4 Tab 多视图骨架。面板尺寸从 60×18 扩大为 80×24。

实现要点：

1. **Tab 栏**：使用 `peri-widgets` 的 Tabs 组件渲染 `Installed | Discover | Marketplaces | Errors` 四个 tab。Tab 切换用 `Ctrl+Tab` / `Ctrl+Shift+Tab`。

2. **PluginSummary 扩展**：为支持详情视图和操作菜单，需扩展 atom 数据模型（`atoms.rs:124`）：

   ```rust
   struct PluginSummary {
       // 现有字段
       name: String, version: String, enabled: bool, root: String, description: String,
       // v2 新增
       marketplace: String,       // marketplace 来源名
       author: Option<String>,    // 插件作者
       skills: Vec<String>,       // 注册的 skill 列表
       commands: Vec<String>,     // 注册的命令列表
       agents: Vec<String>,       // 注册的 agent 列表
       mcp_servers: Vec<String>,  // 注册的 MCP server 列表
       install_scope: String,     // "user" | "project"
       load_error: Option<String>,// 加载失败时的错误信息
       install_count: Option<u64>,// Discover 视图中的安装数
   }
   ```

3. **子视图状态机**：用 `use_state` 管理 `selected_view: TabKind` + `selected_index: usize` + `detail_open: bool` + `action_menu_open: bool`。

4. **render 分支**：

   ```
   PluginPanel render body:
     ├── Tab 栏 (positioned)
     ├── match selected_view:
     │   ├── Installed → render_installed_list()    // 复用现有逻辑，扩展行高到 5 行
     │   ├── Discover  → render_discover()          // 搜索框 + 列表
     │   ├── Marketplaces → render_marketplaces()   // marketplace 列表 + 增删
     │   └── Errors    → render_errors()            // 仅展示 load_error != None 的插件
     ├── if detail_open → render_detail_pane()      // Detail + Actions 右/下半区
     └── nav-hint (统一 footer)
   ```

5. **文件变更清单**（Phase 1 仅结构，无后端交互）：

   | 文件 | 变更 |
   |------|------|
   | `peri-tui/src/kit/panels/plugin.rs` | 主体重写：Tab 栏、子视图 render、状态机 |
   | `peri-tui/src/kit/atoms.rs` | `PluginSummary` 扩展字段，`TabKind` 枚举 |
   | `peri-tui/src/kit/panel_registry.rs` | 面板尺寸 60×18 → 80×24 |
   | `peri-tui/locales/en/main.ftl` | 新增 tab/discover/marketplace/errors 翻译 key |
   | `peri-tui/locales/zh-CN/main.ftl` | 同上 |
   | `peri-tui/src/launch.rs` | 派生更多 `PluginSummary` 字段（从 `LoadedPlugin` 中取） |

**Phase 2 — 操作交互打通（≈ 400 行 insert）**

目标：实现安装/卸载/启用/禁用/搜索 discover。交互通过 ACP `session/query` + `peri/unstable-event` 通道与中间件通信。

1. **ACP 自定义事件 schema**（新增 `peri-acp-types/src/event_data.rs` 中的数据结构）：

   ```rust
   // plugin-snapshot: 插件列表变更后全量推送
   struct PluginSnapshotData {
       plugins: Vec<PluginSummary>,  // 全量快照
   }

   // plugin-action-result: 操作结果通知
   struct PluginActionResultData {
       action: String,            // "install" | "uninstall" | "toggle"
       plugin_id: String,
       success: bool,
       error: Option<String>,
   }

   // plugin-search-result: Discover 搜索返回
   struct PluginSearchResultData {
       query: String,
       results: Vec<PluginSummary>,
       from_cache: bool,
   }
   ```

2. **操作命令映射**（用户按键 → ACP command → 中间件 → 事件回传）：

   | 用户操作 | 快捷键 | ACP 通道 | 事件回传 |
   |---------|--------|----------|---------|
   | 安装插件 (user) | Enter (Discover) | `session/query` + agm install | `plugin-action-result` + `plugin-snapshot` |
   | 安装插件 (project) | Ctrl+Enter | 同上 | 同上 |
   | 卸载插件 | `Ctrl+D` (Installed) | agm uninstall | 同上 |
   | 启用/禁用 | `Space` (Installed) | 修改 config.json | `plugin-snapshot` |
   | 搜索 marketplace | 输入键入 | debounce → `session/query` | `plugin-search-result` |
   | 添加 marketplace | `Ctrl+A` (Marketplaces) | `session/query` | `plugin-snapshot` |
   | 删除 marketplace | `Ctrl+D` (Marketplaces) | `session/query` | `plugin-snapshot` |
   | 查看详情 | `Enter` (Installed) | 无（本地已有数据） | — |
   | 返回列表 | `Esc` | — | — |

3. **Atom 动态刷新**：Phase 2 中 `PLUGIN_LIST` 从静态改为可刷新。`service_snapshot.rs` 的 `tick_once()` 需为 plugins 字段增加真正的重检逻辑（监听 `plugin-snapshot` 事件 → 更新 atom）。或者直接让 Plugin Panel 的 `use_effect` 订阅 `PLUGIN_LIST` 变更。

4. **操作确认弹窗**：卸载和删除 marketplace 需二次确认（仿 Cron Panel 的 `Confirm delete` 模式，`Enter` 确认 `Esc` 取消）。

**Phase 3 — 错误展示与持久化优化（≈ 100 行 insert）**

目标：展示插件加载错误、缓存 Discover 搜索结果、marketplace 状态指示。

1. **Errors 视图**：从 `PluginSummary.load_error` 字段筛选，单插件展示错误原因（仿 MCP 的 `✗` + 错误摘要）。
2. **Discover 搜索 debounce**：用 `use_state` + `tokio::time::sleep(300ms)` 实现防抖。
3. **marketplace 可达性**：在 Marketplaces tab 中对每个 marketplace 标注 `● 在线` / `○ 离线`。

#### 与 MCP Panel 的设计对齐

Plugin 和 MCP 同属 `MutexGroup::Tools`，应该在下列方面保持一致：

| 维度 | MCP Panel | Plugin Panel（目标） |
|------|-----------|---------------------|
| 面板尺寸 | 70×22 | 80×24 |
| 列表 → 详情交互 | Enter 进入详情 | Enter 进入详情 |
| 操作菜单 | 列表下 Action sheet | 详情下 Action sheet |
| 数据刷新 | `mcp-snapshot` unstable-event | `plugin-snapshot` unstable-event |
| 状态指示 | ✓/△/✗/◯ | ✓/✗/◯（load error 同 ✗） |
| 空态引导 | 显示配置帮助 | 显示 `agm install` 帮助 |
| 确认删除 | 显式 Confirm | 显式 Confirm |

#### 关键文件速查

| 关注点 | 文件:行号 |
|--------|----------|
| 面板组件 | `peri-tui/src/kit/panels/plugin.rs:25` |
| PluginSummary 定义 | `peri-tui/src/kit/atoms.rs:124-130` |
| PLUGIN_LIST atom | `peri-tui/src/kit/atoms.rs:201` |
| service_snapshot 写入 | `peri-tui/src/kit/service_snapshot.rs:242` |
| 面板注册元数据 | `peri-tui/src/kit/panel_registry.rs:252-263` |
| Plugin 加载入口 | `peri-tui/src/launch.rs:128` |
| 中间件 PluginLoadResult | `peri-middlewares/src/plugin/loader.rs` |
| Installer（安装/卸载） | `peri-middlewares/src/plugin/installer/` |
| Marketplace 搜索 | `peri-middlewares/src/plugin/marketplace/` |
| ACP DTO（plugin_types） | `peri-acp-types/src/plugin_types.rs` |
| ACP DTO（summary PluginDto） | `peri-acp-types/src/summary.rs:183-206` |
| i18n en | `peri-tui/locales/en/main.ftl:732,750,804-809` |
| i18n zh-CN | `peri-tui/locales/zh-CN/main.ftl:731,749,803-808` |

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
      审查刚才生成的 TUI 设计文档
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
  Phases                       │  Agents
  ─────────────────────────────│────────────────────────────────────────
  > ✓ Design               [1] │  ● coder-2            128k tok   14
    ● Build                [2] │  ✓ coder-1             42k tok    8
    ○ Verify                   │  ○ reviewer             0 tok     0
    ○ Ship                     │
                               │
  Tab::next · Shift+Tab::prev · ↑↓::navigate · ←→::pane · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示多个 workflow run 的可切换工作台。顶部 tabs 切换不同 workflow run，状态 emoji 放在文本之前。主体左右分栏，左侧 Phase 列（40%宽），右侧 Agents 列（60%宽），`│` 分隔线；Workflow Panel 不显示 `Selected Phase` / `Selected Agent` 详情区，避免重复信息。选中某个 Phase 后右侧 Agents 自动过滤为该 Phase 下的 agent，agent 行不再重复显示 phase 标签。Agents 列展示 agent 名称、token 用量、工具调用数。所有状态必须以 emoji + theme status token 同时区分，列表中只显示 `✓`、`●`、`○`、`✗`，不要重复显示英文状态。每列独立 ScrollView，选中项离开视口时自动跟随滚动（scroll_start_for_selected）。数据源必须来自 ACP-only flow：JSON-RPC notification method 为 `peri/unstable-event`，其中 `event` 为 `workflow-snapshot`；payload 在 `WorkflowRunListDto` 基础上扩展 phase/agent 运行态。

---

> [返回总索引](tui-index.md)
