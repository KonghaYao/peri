# TUI 面板系统

> 本文档描述 PanelOverlay 面板容器及 16 个 Panel 的完整设计规范，包括面板导航、互斥关系、快捷键约定。

---

## 5. PanelOverlay 面板容器

`PanelOverlay` 是 `SessionColumn` 内部组件（`peri-tui/src/kit/layout.rs`），不是 AppShell 根级浮层。它固定插在 `MessageArea` 和 `InputArea` 之间；当任意 panel 打开时，`InputArea(hidden: true)`（layout.rs 将 `panel_open` 传入 `hidden` prop），避免输入区抢焦点。Panel 与 InputArea 统一为上下边框样式：只有 top/bottom border，没有 left/right border（`panels/mod.rs` 的 `panel_shell!` 宏统一提供）。

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

- Panel 宽度全铺满终端：外层 View 与内层 View 均使用 `Constraint::Fill(1)`（`panel_overlay.rs::render_panel`），不随面板注册的 `PanelLayout` 固定尺寸缩放；高度自适应：`height = theme.component.panel.max_height.min(term_h - MESSAGE_RESERVE)`，并夹在 `min_height` 与 `max_height` 之间（`MESSAGE_RESERVE = 4`，为 MessageArea 预留最小行数）。
- Panel 与 InputArea 同样只有上下边框；禁止左右边框。
- 面板打开时隐藏 InputArea。
- 互斥组：Settings、Agent、Tools、Info、Thread、AskUser；同组只保留一个。`open_panel(kind)` 打开新面板前先关闭同组其他面板（`panel_registry.rs`），`OPEN_PANELS` 栈 + `ACTIVE_PANEL` 原子驱动渲染。

### 5.1 面板注册表（panel_registry.rs）

`PANELS: &[PanelMeta]` 是 16 个面板的单点注册：标题、slash command、互斥组、scope、尺寸元数据、render 函数。`PanelKind` 枚举在 `peri-tui/src/app/panel_types.rs`。

| PanelKind | 标题 | slash 命令 | 互斥组 | scope | 注册尺寸 |
|-----------|------|-----------|--------|-------|---------|
| Model | Model | /model | Settings | Session | 60×18 |
| Login | Login | /login | Settings | Session | 60×18 |
| Agent | Agent | /agent | Agent | Session | 60×18 |
| Hooks | Hooks | /hooks | Agent | Session | 60×18 |
| Config | Config | /config | Settings | Session | 60×18 |
| ThreadBrowser | Threads | /threads | Thread | Session | 60×18 |
| Mcp | MCP | /mcp | Tools | Global | 60×18 |
| Plugin | Plugin | /plugin | Tools | Global | 80×24 |
| Cron | Cron | /cron | Tools | Global | 60×18 |
| Status | Status | /status | Info | Global | 60×18 |
| Memory | Memory | /memory | Info | Global | 60×18 |
| Tasks | Tasks | /tasks | Tools | Global | 60×18 |
| Betas | Betas | /betas | Info | Global | 60×18 |
| Workflow | Workflow | /workflows | Tools | Global | 90×14 |
| AskUser | Ask User | （自动打开） | AskUser | Session | 60×18 |
| Theme | Theme | /theme | Settings | Global | 50×24 |

打开路径约定：

- 面板统一通过 slash command 打开（SlashCompletion 的 `SlashActionKind::Panel`，或 command/skill 映射到面板）；`/history`、`/resume`、`/his` 是 `/threads` 的别名（`panel_for_slash_command`，ACP server 将 history/resume 作为远程 command 下发时映射为打开 ThreadBrowser）。
- AskUser 面板由 AskUserQuestion 事件自动打开（`acp_events/system.rs` 收到 AskUser 事件后 `open_panel(PanelKind::AskUser)`），无 slash 命令、无快捷键。
- `shortcut_letter`（Ctrl+字母，如 Ctrl+M）仅存在于注册表元数据并有唯一性测试（`panel_registry_test.rs`），**生产事件链不消费**；不占全局快捷键。全局保留键：Ctrl+C（中断/退出）、Ctrl+O（diff）、Ctrl+T / Ctrl+Shift+T（模型/provider 循环）、Shift+Tab（权限模式循环）、双击 Esc（Rewind）。
- 关闭：`Esc` 走 `event_handlers::register_root_handlers` 的关闭优先级链（popup → @mention/slash → panel → input）；AskUser 面板在 Esc 时先发 `AskUserResponseAction::Cancel` 防止 agent 挂起（含防御性 guard）。

---

## 6. 16 个 Panel 页面设计

统一约束：所有 Panel 只使用上下边框，不使用左右边框；所有 Panel 的数据来源必须可追溯到 ACP standard、`session/query` snapshot 或 `peri/unstable-event` custom event。

### 6.1 Model Panel

Model Panel 采用左右分栏：左侧为 Profile 列表（固定顺序 `fable → opus → sonnet → haiku`，active 高亮），右侧为当前选中 Profile 的单行 K/V 编辑。数据来自 `PERI_CONFIG_HANDLE`（Profile 是请求参数唯一事实源）。

```text
──────────────────────────────── Model ────────────────────────────────────────
  Profiles                         fable · anthropic
  ─────────────────────────        ─────────────────────────────────────────
  ❯ fable · anthropic              Provider                        anthropic
    claude-opus-4-6                Model                       claude-opus-4-6
    xhigh · 200k                   Effort                              xhigh
                                    Max tokens                          32000
    opus · anthropic               1m enable                              off
    claude-opus-4-6
    xhigh · 200k

    sonnet · openai                ←/→ change value     esc close
    gpt-5.6-luna
    xhigh · 1m

    haiku · anthropic
    claude-haiku-4-5
    medium · 200k

  ↑/↓::switch profile Tab/→::edit · Esc::close
────────────────────────────────────────────────────────────────────────
```

- 左侧：`↑/↓` 切换选中 Profile 并写 `active_alias` 持久化；`Tab`/`→` 进入右侧编辑焦点。
- 右侧：`↑/↓` 在字段间移动焦点；`←/→` 切换当前字段值，**切换即写入内存并持久化**（无需 Enter/Save）；`Tab` 切回左侧；`Esc` 退出右侧焦点，再 `Esc` 关闭面板。
- 字段固定：`Provider` / `Model` / `Effort` / `Max tokens` / `1m enable`，单行 `key` 左对齐、`value` 右对齐，无包围符号。
- `Provider` 切换联动：优先选择目标 provider 下同档位 Model；无同档位时选择该 provider 默认 Model。
- `Model` 允许选择该 provider 下任意模型（不做档位过滤与能力兼容性检查）。
- `Effort` 五档循环：`low → medium → high → xhigh → max`；`Max tokens` 五档预设循环：`4096/8192/16000/32000/64000`；`1m enable` 切换 1m/200k 上下文窗口。
- 显示规则：模型名内 `high`（如 `gpt-5.6-luna high`）使用 model accent 色；摘要中 effort 值使用独立 effort 色，二者颜色语义不同。

能力：选择 active profile，编辑该 profile 独立的 `provider/model/effort/max_tokens/context_1m`。数据来源：`PERI_CONFIG_HANDLE`；变更通过统一 config action 返回 snapshot。

### 6.2 Login Panel

Login 面板为 Browse / Edit / DeleteConfirm 三模式（`peri-tui/src/kit/panels/login.rs`），provider 列表来自 `PROVIDER_LIST` atom（由 service_snapshot 从 `peri_config.providers` 派生）。

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

  ↑/↓::navigate Enter::edit Ctrl+N::new Ctrl+D::delete · Esc::close
────────────────────────────────────────────────────────────────────────
```

- **Browse 模式**：只读列表 + `↑/↓` 导航；`Enter`/鼠标点击进入 Edit 模式；`Ctrl+N` 新建 provider，`Ctrl+D` 进入删除确认。
- **Edit 模式**：原地编辑 provider 字段（provider_type / provider_id / api_key / base_url / 四档模型 fable/opus/sonnet/haiku，样式与 setup_wizard 表单统一）；`Enter` 保存并持久化，`Esc` 放弃，`Ctrl+S` 快捷保存。
- **DeleteConfirm 模式**：`Enter` 确认删除，`Esc` 取消。
- 敏感值只显示 configured/missing，不展示 secret。

能力：provider 增删改查与激活；变更通过 `PERI_CONFIG_HANDLE` 写 PeriConfig 并持久化。数据来源：ACP service snapshot / config snapshot。

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

能力：只读展示当前会话元信息和从 ACP view models 派生的 SubAgent 状态。数据来源：`SERVICE_SNAPSHOT`（provider/model/permission_mode/cwd）+ `VIEW_MODELS`（消息计数、`TuiSubAgentGroup` 变体扫描）；切换 provider/model 在 Login/Model 面板，permission_mode 在 Config 面板。

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

能力：只读展示插件声明的 hooks、事件说明、来源和匹配器。数据来源：`HOOK_LIST` atom（service_snapshot 从 plugin_data.all_hooks 派生，2s 刷新）；hooks 在插件 `hooks/<event>.json` 中声明，UI 不修改。

### 6.5 Config Panel

```text
──────────────────────────────── Config ───────────────────────────────
  Configuration (persisted to ~/.peri/settings.json)

  > Show Diff          [ON]
    Cache Warning      [ON]
    Streaming Mode     streaming      < block | none >
    1M Context         [OFF]
    Language           zh             < en | zh-CN >
    Active Alias       sonnet         < fable | opus | sonnet | haiku >
    Permission Mode    auto-mode      < default | accept-edit | auto-mode | bypass >
    Scroll FPS         60             < 60 | 30 | 20 >

  ↑/↓::navigate Space/Enter::toggle · ←/→::cycle · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：编辑核心 PeriConfig 字段；`Toggle` 行（Show Diff / Cache Warning / 1M Context）用 `Space`/`Enter` 切换，`Cycle` 行（Streaming / Language / Active Alias / Permission Mode / Scroll FPS）用 `←/→` 循环；permission mode 写运行时共享状态（`PERMISSION_MODE_HANDLE`，不持久化），其余配置持久化到 `~/.peri/settings.json`。数据来源：`PERI_CONFIG_HANDLE` + `PERMISSION_MODE_HANDLE`；变更通过 config action 后返回新 snapshot，UI 不直接读写配置文件。

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

能力：浏览历史 thread，选择后切换当前会话上下文。数据来源：`THREAD_LIST` atom（service_snapshot 周期性从 thread_store 派生）；切换通过 `THREAD_LOAD_TX` → AcpClient 触发。

技术实现：ThreadBrowser 采用手动渲染模式（仿 Login 面板），不再使用 VirtualList。VirtualList 在 `panel_shell!` 的 `border` 内 `Fill(1)` 会被 ratatui 解析为 0，导致不可见。当前实现：`Vec<Line>` → `Paragraph` → `ScrollView(Text)`，手动处理 ↑/↓/Enter 键盘事件，条目间有空行分隔，选中行使用 `>` 标记 + bold 高亮。

### 6.7 MCP Panel

```text
───────────────────────────── MCP Servers ─────────────────────────────
  Project: /Users/.../perihelion

  > ✓ filesystem        stdio   tools 8
    △ langfuse          http    tools 12   oauth needed
    ✗ slack             sse     tools 0    reconnect failed
    ◯ browser           http    disabled

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：**只读摘要面板**——展示 MCP server 列表（status/transport/tools 数）与初始化阶段摘要（`SERVICE_SNAPSHOT.mcp`：Pending/Initializing/Ready/Failed）。数据来源：`MCP_SERVERS` atom（service_snapshot 从 mcp_pool.all_server_infos 派生，2s 刷新）；MCP 配置通过 `~/.claude/settings.json` 管理，面板不做 ViewTools/ReAuthenticate/Reconnect 等深度操作。

### 6.8 Plugin Panel

Plugin Panel 已实现 **v2 Phase 2**：4-tab 多视图 + list/detail 双模式状态机（`peri-tui/src/kit/panels/plugin.rs`）。←/→ 切换视图（Installed / Discover / Marketplaces / Errors），↑/↓ 导航，Enter 进详情/执行操作，Esc 返回列表；无 ScrollView（避免其内置 handler 与自定义 ↑/↓ 冲突）。

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
    skills:      1     commands: 0    agents: 0    mcp: 0
    scope:       user

  Actions
    > Disable plugin
      Uninstall
      Back to plugin list

  ←/→::view · ↑/↓::navigate · Enter::detail/execute · Esc::back/close
────────────────────────────────────────────────────────────────────────
```

- **Installed**：已安装插件列表（名称/来源 scope/版本/技能与命令计数/加载错误/禁用态）；详情视图展示 marketplace、author、path、skills/commands/agents/mcp 计数、install scope，操作菜单含 Disable/Uninstall。
- **Discover**：搜索（`PLUGIN_SEARCH_RESULTS` atom，`plugin-search-result` 事件写入）+ 结果列表；详情动作 Install user scope / Install project scope / Back（`DiscoverDetailAction`）。Discover 数据有磁盘缓存（`DISCOVER_CACHE` OnceLock，锁外 I/O）。
- **Marketplaces**：marketplace 列表，数据同样走磁盘缓存（`MARKETPLACE_CACHE`）。
- **Errors**：仅展示 `load_error != None` 的插件。
- 数据模型：`PluginSummary`（`atoms.rs`）：name/version/enabled/root/description/marketplace/author/skills_count/commands_count/agents_count/mcp_count/install_scope/load_error（计数型字段而非 Vec，无 install_count）。

能力：查看已安装插件、搜索 Discover 并安装到 user/project scope、管理 marketplace、查看加载错误。数据来源：`PLUGIN_LIST` atom（启动时从 `PluginLoadResult.plugins` 派生为 `SnapshotSource.plugins`，service_snapshot 每 tick 写入；`PLUGIN_SEARCH_RESULTS` 由 `plugin-search-result` 事件写入）。

相关文件索引：

| 内容 | 位置 |
|------|------|
| PluginPanel v2 实现 | `peri-tui/src/kit/panels/plugin.rs` |
| 面板注册元数据 | `peri-tui/src/kit/panel_registry.rs`（Plugin 条目） |
| PluginSummary / PLUGIN_LIST atom | `peri-tui/src/kit/atoms.rs` |
| Plugin 加载入口 | `peri-tui/src/launch.rs`（load_enabled_plugins_aggregated） |
| 中间件 PluginLoadResult | `peri-middlewares/src/plugin/loader.rs` |
| 插件类型定义 | `peri-middlewares/src/plugin/types.rs` |
| Installer（安装/卸载） | `peri-middlewares/src/plugin/installer/` |
| Marketplace 搜索 | `peri-middlewares/src/plugin/marketplace/` |
| ACP DTO（plugin-snapshot / plugin-action-result / plugin-search-result） | `peri-acp-types/src/event_data.rs` |
| i18n en / zh-CN | `peri-tui/locales/en/main.ftl`、`peri-tui/locales/zh-CN/main.ftl` |

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

  ↑/↓::navigate Enter/Space::toggle · d::delete · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：Cron Panel 是操作面板——`Enter`/`Space` toggle enabled，`d` 进入删除确认（`Enter` 确认 / `Esc` 取消 / `Ctrl+C` 退出确认），空列表引导和鼠标选择。toggle/delete 直接调用 `CRON_SCHEDULER_HANDLE`（共享 CronScheduler 句柄，非 ACP RPC），service_snapshot 2s tick 自动派生新列表。数据来源：`CRON_JOBS` atom（service_snapshot 从 cron_scheduler 派生）。

### 6.10 Status Panel

Status Panel 为**双 Tab（Service / Context）**：

```text
─────────────────────────────── Status ────────────────────────────────
  Service │ Context

  > Provider          anthropic
    Model Alias       sonnet
    Permission Mode   auto-mode
    CWD               /Users/.../perihelion
    CPU               12%
    Memory            430MB
    MCP               ready 3/4
    Cron              2 / 5 enabled

  Tab::switch · ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

- **Service Tab**：直接读 `SERVICE_SNAPSHOT` atom（CPU/MEM/provider/model/permission_mode/cron 统计），无需 mock。
- **Context Tab**：从 `VIEW_MODELS` 派生消息计数（committed + current_turn 的 TuiRenderUnit 分类统计），反映当前会话上下文状态。

数据来源：ACP service snapshot + VIEW_MODELS；不直接读取 runtime/global state。

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

能力：展示 memory 文件、大小和更新时间；Enter 使用 `$EDITOR`（fallback `vi`）打开——通过 spawn_blocking + Detach 执行，避免阻塞渲染线程。数据来源：`MEMORY_LIST` atom（service_snapshot 扫描 `~/.claude/memory/*.md`，2s 刷新）。

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

能力：跨调度源的任务总览——聚合 **Cron 任务**（`CRON_JOBS` atom）与 **SubAgent 运行时**（`VIEW_MODELS` 扫描 `TuiSubAgentGroup`）。只读面板：Cron 的 enable/disable/delete 在 Cron 面板，SubAgent 详情在 Agent 面板。

### 6.13 Betas Panel

```text
──────────────────────────────── Betas ────────────────────────────────
  Feature Flags
  (read-only)

  > ratatui-kit-ui       on
      New kit-based TUI rendering path

    workflow-panel       on
      Workflow visibility panel

    theme-system         off
      Experimental theme loader

  ↑/↓::navigate Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：实验功能开关列表（只读，on/off 状态展示，`↑/↓` 导航、`Esc`/`Enter` 关闭）。**当前为 Mock 数据**（`betas.rs` 内 `BETA_ENTRIES` 常量，Phase 8 计划通过 Atom/props 注入真实 feature 列表），尚未接入 ACP feature-flags 数据源。

### 6.14 Workflow Panel

```text
────────────────────────────── Workflow ───────────────────────────────
  [● run_01JZ]  ✓ run_01JY  ✗ run_01JX
────────────────────────────────────────────────────────────────────────
  Phases                       │  Agents
  ─────────────                 ─────────────
  > ✓ Design               [1] │  ● coder-2            128k tok   14
    ● Build                [2] │  ✓ coder-1             42k tok    8
    ○ Verify                   │  ○ reviewer             0 tok     0
    ○ Ship                     │
                               │
  Tab::next · Shift+Tab::prev · ↑↓::navigate · ←→::pane · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：展示多个 workflow run 的可切换工作台。顶部 tabs 切换不同 workflow run（`Tab`/`Shift+Tab`），状态 emoji 放在文本之前。主体左右分栏，左侧 Phase 列（40%宽），右侧 Agents 列（60%宽），`│` 分隔线；`←/→` 切换 pane，`↑/↓` 导航；`Enter` 为 MVP no-op（不显示 Selected Phase / Selected Agent 详情区）。选中某个 Phase 后右侧 Agents 自动过滤为该 Phase 下的 agent，agent 行不再重复显示 phase 标签。Agents 列展示 agent 名称、token 用量、工具调用数。所有状态以 emoji + theme status token 同时区分（`✓`/`●`/`○`/`✗`），不重复显示英文状态。每列独立 ScrollView，选中项离开视口时自动跟随滚动（`scroll_start_for_selected`）。数据来源：`WORKFLOW_SNAPSHOT` atom（`peri/unstable-event` 的 `workflow-snapshot` 事件写入）。

### 6.15 Theme Panel

```text
──────────────────────────────── Theme ────────────────────────────────
  [Dark]  Light                       Preview
  ─────────────────────────           ──────────────────────────────────
  > peri-dark                         # Heading
    synthwave-84                      **bold** · *italic* · ~strike~
    monokai-classic                   `code` and plain text
                                      > blockquote sample

  ↑/↓::navigate Enter::apply Ctrl+T::daily-color Ctrl+D::download · Esc::close
────────────────────────────────────────────────────────────────────────
```

能力：列出可用主题（builtin + `~/.peri/themes/`），顶部为选中主题的 markdown 预览（SAMPLE_MD 覆盖标题/粗体/斜体/删除线/行内代码/引用，预览宽度 46 列）。交互模式：

- `Tab` 切换 Dark/Light 主题分组；`↑/↓` 导航 → 实时切换全局颜色（实时预览）。
- `Enter` → 持久化到 `~/.peri/settings.json`（TUI_CONFIG_HANDLE + PERI_CONFIG_HANDLE 同步 extra 字段；写盘在独立线程，不阻塞 TUI 事件循环）。
- `Esc` → 恢复打开面板前的原始主题色并关闭。
- `Ctrl+T` → 切换 daily color；`Ctrl+D` → 触发主题下载（打开 `DownloadProgressPopup`，见 tui-popups.md §7.6），下载成功后重新扫描主题目录。

数据来源：peri-theme loader + 主题目录扫描；持久化走统一 config save。

### 6.16 AskUser Panel

AskUser 面板是用户问答面板——当 agent 调用 AskUserQuestion 工具时，`acp_events/system.rs` 收到 AskUser 事件后**自动作为 Panel 打开**（`open_panel(PanelKind::AskUser)`），内联渲染在 MessageArea 和 InputArea 之间（`MutexGroup::AskUser`，仅 AskUser 自身）。面板逻辑复用原 ask_user_popup 的 Tab 交互模型，但通过 `panel_shell!` 渲染（`peri-tui/src/kit/panels/ask_user.rs`）。

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

- 支持 1-4 个问题批量接收；顶部 tabs 展示所有问题（`Tab`/`Shift+Tab` 切换），已回答项旁显示 ✓。
- 每个问题可为单选（●/○）或多选（☑/☐），`Space` 选中/取消；支持自定义文本输入（TextAreaState，视口上限 3 行）。
- `Enter` 跳到下一个未确认问题或全部答完后提交；`Esc` 不直接取消——先弹 Confirm 确认弹窗（`RejectAskUser`，见 tui-popups.md §7.5），确认后经 `ASK_USER_RESPONSE_TX` 发送 `AskUserResponseAction::Reject` 并关闭面板（防止 agent 永久挂起；`event_handlers.rs` 另有一份防御性 guard 兜底发 `Cancel`，正常流程不触发）。
- 面板打开时隐藏 InputArea，与其他 Panel 行为一致。
- 响应链路：`AskUserResponseAction`（Submit/Cancel/Reject）→ `ask_user_action.rs` 消费者 task → `AcpTuiClient::send_response`（`ElicitationAction` 内部标签：`{"action": "accept", "content": {q_id: label}}` / `cancel`）。

---

## Issue 经验附录

### issue_2026-08-01-model-panel-profile-row-click-no-response
**摘要:** Model 面板 profile 行点击无响应——click-as-enter 覆盖遗漏
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** click-as-enter, hit_test, 滚动偏移, 面板覆盖
**问题本质:** a8d0ff79 "click as enter" 覆盖 8 面板唯独漏掉 model.rs；无鼠标 handler + occluded 让路导致点击落空。
**通用模式:** 批量统一模式改造的"遗漏项"需核对覆盖清单；滚动后点击命中要读 ScrollView offset 防漂移。
**涉及文件:** peri-tui/src/kit/panels/model.rs, panel_mouse.rs
**CLAUDE.md 链接:** false

### issue_2026-08-02-plugin-panel-uninstall-enter-freeze
**摘要:** Plugin 面板卸载按 Enter 卡死——scrutinee 中 .read() 临时 guard 重入死锁
**状态:** Fixed
**归档日期:** 2026-08-11
**关键词:** RwLock 重入死锁, 临时生命周期, scrutinee, parking_lot
**问题本质:** match/if-let scrutinee 中 `.read()` 临时 guard 存活至整个表达式结束，分支内同 atom `.write()` 同线程重入死锁（5 处同型，均已修）。
**通用模式:** 事件 handler 写 state 先 `let` 提取值再 match；scrutinee 含 `.read()` 时检查分支体是否对同 atom `.write()`——入 code review checklist。
**涉及文件:** peri-tui/src/kit/panels/plugin.rs（5 处）
**CLAUDE.md 链接:** false

---

> [返回总索引](tui-index.md)
