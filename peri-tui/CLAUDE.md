# peri-tui

TUI 应用，纯 ACP client 前端。运行时仅通过 `peri-acp` 的 `MpscTransport`（in-memory channel pair）与 ACP Server 通信，不直接依赖 `peri-agent`/`peri-middlewares` 的运行时路径（仅作为类型依赖）。

## 当前架构（ratatui-kit 单路径，S1–S13 完成，I14–I23 增量 + 渲染管道重写）

**单一路径**：`use-kit` feature 默认 ON。`main.rs` 调用 `kit::entry::run_kit_fullscreen(opts, panic_notify_rx)`，legacy `runtime/main_loop` / `state_machine` / `command/` / `panel/` / `ui/` / `render/` / `event/` 已全部物理删除（净减 ~18000 行）。

### kit 五链路（spawn 于 entry::run_kit_fullscreen）

```
1. spawn_kit_notifier     : AcpNotification → AcpEventData → bridge_tx + render_bridge_tx
2. spawn_acp_bridge       : bridge_rx → BridgeState → Atom 写入（VIEW_MODELS / ACP_STATE / POPUP_KIND ...）
3. spawn_render_bridge    : render_bridge_rx + resize_rx → 预计算 Vec<Line> + wrap_map → RENDER_CACHE atom
4. spawn_submit_consumer  : SUBMIT_TX (String) → acp_client.prompt()
5. spawn_service_snapshot : 2s tick → SERVICE_SNAPSHOT / THREAD_LIST / CRON_JOBS / FILE_LIST atoms

附加：
- spawn_rewind_consumer     : REWIND_ACTION_TX → session/execute-command ("/rewind")
- spawn_thread_load_consumer : THREAD_LOAD_TX → acp_client.load_session(thread_id)
```

### 全局状态（atoms.rs，OnceLock 延迟初始化）

类型别名：`pub type Atom<T> = ratatui_kit::prelude::StoreState<T>`。

| Atom | 用途 |
|------|------|
| `VIEW_MODELS` | `ViewModelsSnapshot { committed: Arc<[ViewModel]>, current_turn: Arc<[ViewModel]> }` —— 消息流单一数据源 |
| `RENDER_CACHE` | `RenderCache { entries, cumulative_heights, wrap_map }` —— render_bridge 预计算的 `Vec<Line<'static>>` + `WrappedLineInfo` 视觉行映射 |
| `ACP_STATE` | `AcpStateSnapshot { variant, is_loading, ... }` —— popup_active 已退役，弹窗状态走 POPUP_KIND.is_some() |
| `SERVICE_SNAPSHOT` | CPU/MEM/MCP/Cron/provider/model_name/permission_mode/cwd 投影 |
| `THREAD_LIST` / `CRON_JOBS` / `FILE_LIST` | Thread / Cron / cwd 文件列表 |
| `HOOK_LIST` / `PLUGIN_LIST` / `MCP_SERVERS` / `PROVIDER_LIST` | Hook / Plugin / MCP / Provider 列表（供面板渲染） |
| `SUBAGENT_LIST` / `MEMORY_LIST` | SubAgent 运行时状态 / Memory 文件条目 |
| `TODO_ITEMS` | `Vec<TodoItem>` —— ACP SessionUpdate::Plan 下发的 Todo 列表 |
| `OPEN_PANELS` / `ACTIVE_PANEL` / `POPUP_KIND` | 当前面板栈 + 激活面板 + 激活弹窗 |
| `INPUT_HISTORY` / `INPUT_HISTORY_INDEX` / `DRAFT` | 输入历史栈 + 浏览指针 + 浏览前草稿保存（容量 1000） |
| `INPUT_BUFFER` | agent loading 时缓存的待提交输入队列（上限 32） |
| `HITL_PENDING` / `ASK_USER_PENDING` / `OAUTH_INFO` | 3 popup 的真实 payload（由 dispatch_and_notify 写入，close_popup 统一清空） |
| `REWIND_PREVIEW` / `LAST_ESC_TIME` | Rewind 弹窗数据 + 双击 Esc 检测（500ms 窗口） |
| `DIFF_VISIBLE` | Ctrl+O toggle 的 diff 视图开关 |
| `PREDICTION` | `PredictionState { text, received_at }` —— agent 预测文本（灰色占位，Tab 接受） |
| `AT_MENTION_ACTIVE` / `MENTION_PREFIX` / `MENTION_SELECTED_INDEX` | @mention 状态 + SkimMatcherV2 模糊匹配 |
| `SLASH_HINT_ACTIVE` / `SLASH_PREFIX` / `SLASH_SELECTED_INDEX` | slash 补全状态 |
| `AVAILABLE_SLASH_COMMANDS` / `ACP_COMMANDS` / `SKILL_NAMES` | ACP 下发的命令/skill 列表 |
| `MODEL_HIGHLIGHT_UNTIL` / `PROVIDER_HIGHLIGHT_UNTIL` / `MODE_HIGHLIGHT_UNTIL` | Status bar 瞬时高亮（1.5s） |
| `COPY_CHAR_COUNT` / `COPY_MESSAGE_UNTIL` | 文本选中复制提示（"已复制 N 字符"，2s 消失） |
| `WIZARD_ACTIVE` / `QUIT_PENDING_SINCE` | SetupWizard 激活 / 双击 q 退出 |
| `INPUT_AREA_ESC_PREFIX` | Esc 清空输入提示前缀 |
| `PENDING_ATTACHMENTS` | 待粘贴附件队列 |

**Channels（OnceLock<UnboundedSender>）**：
- `SUBMIT_TX: String` —— InputArea Enter → submit_consumer
- `REWIND_ACTION_TX: RewindAction` —— RewindPopup → rewind_consumer
- `THREAD_LOAD_TX: String` —— ThreadBrowser Enter → thread_load_consumer
- `RESIZE_TX: u16` —— 终端 resize → render_bridge（触发 Line 重建）

**非 atom 全局句柄**：
- `PERI_CONFIG_HANDLE: Arc<RwLock<PeriConfig>>` —— ModelPanel / LoginPanel / ConfigPanel 直接 write；ACP server 持同一 Arc，立即生效
- `PERMISSION_MODE_HANDLE: Arc<SharedPermissionMode>` —— ConfigPanel 切换 permission_mode 直接 store
- `CRON_SCHEDULER_HANDLE: Arc<Mutex<CronScheduler>>` —— CronPanel 直接 toggle/remove；service_snapshot 下次 tick 自动刷新 CRON_JOBS atom

## 输入框（InputArea，kit/input_area.rs）

`#[component]` 组件，自管 EditorState（text + cursor）。完整功能：

- **多行**：Shift/Alt+Enter 换行；高度动态（3~12 行）
- **history**：Up/Down 浏览 `INPUT_HISTORY`（容量 1000，磁盘持久化 `~/.peri/input_history.json`）；进入历史模式时 `DRAFT` atom 保存当前草稿，Esc 或回到底部恢复
- **prediction**：agent 预测文本以灰色占位符显示在光标后，Tab 键接受预测，Esc 或继续输入清除
- **@mention**：`@` 触发 → MentionPopup 显示 `FILE_LIST`，`SkimMatcherV2` 模糊匹配过滤（大小写不敏感）
- **slash 补全**：行首 `/` 触发 → SlashCompletion 显示 `AVAILABLE_SLASH_COMMANDS`（ACP 下发 + 内置 33 条静态命令）
- **编辑快捷键**：Ctrl+W（删词）/ Ctrl+U（清空）/ Ctrl+Left/Right（跳词）/ Home/End / Backspace
- **粘贴**：Event::Paste 整段插入；Ctrl+V 粘贴剪贴板
- **macOS Option 键**：Option+Left/Right/Up/Down 映射为 Ctrl+ 对应功能（Alt 键兼容）
- **提交**：Enter 检查 `ACP_STATE.is_loading`：
  - **非 loading** → SUBMIT_TX.send(text)
  - **loading 中** → push 到 `INPUT_BUFFER`（上限 32，FIFO），TurnDone 时 `drain_input_buffer()` 顺序重新提交
- **slash Enter**：替换 editor 为命令并提交（同样检查 loading 入 buffer）

## 消息渲染（kit/view_render.rs + render_bridge.rs + message_area.rs）

### 渲染管道（三层分离）

```
ACP 事件 → VIEW_MODELS atom 写入
              ↓
render_bridge (独立 tokio task) ：
  ├─ 监听 ACP 事件 + RESIZE_TX 宽度变化
  ├─ content_hash 增量检测：仅 hash 变化的 ViewModel 重建 Line
  ├─ 预计算 Vec<Line<'static>> + WrappedLineInfo（视觉行 → 渲染行映射）
  └─ 写入 RENDER_CACHE atom（entries + cumulative_heights + wrap_map）
              ↓
message_area (ratatui-kit ScrollView)：
  ├─ LineCache：仅 RENDER_CACHE 内容变化时重建，滚动/选区复用缓存
  ├─ 视口裁剪：基于 wrap_map 二分查找 (viewport_clip)，只传可见行给 Paragraph
  ├─ Todo 渲染：TODO_ITEMS atom → ◼/✔/◻ 图标 + 状态行
  ├─ Sticky Header + 滚动按钮：当前用户消息摘要在顶部固定 + ▲/▼ 按钮
  └─ 智能跟随：CurrentTurn 出现时自动滚到底；用户主动上滚时不抢夺滚动位
```

**[TRAP]** RENDER_CACHE 与 VIEW_MODELS 分离——渲染层不直接 touch ViewModel。
**[TRAP]** `/clear` 命令必须同步重置 RENDER_CACHE，否则旧缓存残留。

### ViewModel 变体（view_render.rs）

`render_v2_vm` 处理 7 种变体（UserBubble / AssistantBubble / ToolCard / SystemNote / SubAgentGroup / CollapsedGroup / ReasoningBlock），外加 AskUserBlock、DiffBlock、DividerData 子类型。

- **ToolCard**：format_tool_name 映射（Bash→Shell、WebFetch→Browse 等），format_tool_args 参数摘要提取，工具折叠/展开逻辑
- **SubAgentGroup**：DTO.view_models 优先；fallback probe.recent_messages；prefix ◆→❯，final_result→⎿
- **CollapsedGroup**：emoji→● 前缀，折叠/展开切换
- **SystemNote**：Info/Warning/Error 三级，SystemNote prefix 分类

### 文本选中复制（kit/text_selection.rs）

鼠标 Drag → 选区高亮（selection_bg 主题色） → 松开自动复制到剪贴板（arboard）。通过 `MsgAreaTracker` Hook 记录消息区外边界，`mouse_visual_position` 将终端坐标转为视觉坐标，`extract_selected_text` 提取纯文本。

## 14 面板（kit/panels/，PanelKind 枚举）

| Panel | 数据源 | 切换功能 |
|-------|--------|----------|
| **Model** | SERVICE_SNAPSHOT + 静态 MODEL_ALIASES | ✅ Enter 修改 `PERI_CONFIG_HANDLE.active_alias` |
| **Login** | PROVIDER_LIST atom（H1f） | ✅ Enter 切换 `active_provider_id` + 持久化 |
| **Agent** | SERVICE_SNAPSHOT + PERI_CONFIG_HANDLE + VIEW_MODELS（H1e） | 只读 |
| **Hooks** | HOOK_LIST atom（H1b） | 只读 |
| **Config** | PERI_CONFIG_HANDLE + PERMISSION_MODE_HANDLE（H1a） | ✅ 切换 + 持久化到 settings.json |
| **ThreadBrowser** | THREAD_LIST atom | ✅ Enter 通过 THREAD_LOAD_TX 调用 `load_session` |
| **Mcp** | MCP_SERVERS + SERVICE_SNAPSHOT.mcp（H1d） | 只读 |
| **Plugin** | PLUGIN_LIST atom（H1c） | 只读 |
| **Cron** | CRON_JOBS atom + CRON_SCHEDULER_HANDLE（H1g+） | ✅ Enter toggle / d+Enter delete |
| **Tasks** | CRON_JOBS + VIEW_MODELS SubAgentGroup（H1g） | 只读总览 |
| **Status** | SERVICE_SNAPSHOT + VIEW_MODELS 派生（H1a+） | 只读，Service + Context 双 Tab |
| **Memory** | MEMORY_LIST atom（H1h） | ✅ Enter 调用 `$EDITOR` 打开文件 |
| **Betas** | 构建期 feature flags | 只读 |
| **Workflow** | VIEW_MODELS SubAgent 计数 + 外部 CLI 说明 | 只读 |
| **SetupWizard** | App.global_ui | 配置向导（首次启动触发） |

面板栈互斥组（MutexGroup）：Settings / Agent / Tools / Info / Thread。打开新面板按栈压入；关闭弹栈。

## 4 弹窗（kit/popups/）

由 ACP 事件触发，统一通过 `POPUP_KIND` atom 路由。dispatch_and_notify 在写入
`POPUP_KIND` 的同时把完整 payload 写入对应 atom（I20-D / I21-A / I21-B）；
`close_popup()` 关闭时根据 kind 统一清空 payload atom，避免陈旧数据残留（I21-C）。

| Popup | 触发源 | payload atom | 功能 |
|-------|--------|--------------|------|
| **HITL** | `AcpEventData::HitlPending` | `HITL_PENDING: Option<HitlPending>` | 显示真实 tool_name + tool_input + batch；Enter approve / Esc reject（I21-A） |
| **AskUser** | `AcpEventData::AskUser` | `ASK_USER_PENDING: Option<AskUser>` | Panel 内联渲染；Tab 切问题、↑↓ 导航、Space 选、Enter 下一题/提交、Esc 取消 |
| **Rewind** | `AcpEventData::RewindPreview` 或双击 Esc | `REWIND_PREVIEW: Option<RewindPreview>` | 回退预览 + 确认；REWIND_ACTION_TX → /rewind RPC |
| **OAuth** | `AcpEventData::OauthNeeded` | `OAUTH_INFO: Option<OauthNeeded>` | 显示真实 server_name + auth_url；Ctrl+O 开浏览器、Enter 关闭（I20-D） |

## Status Bar（kit/status_bar.rs）

双行布局：
- 第一行：权限模式 → cwd → model alias → CPU% → MEM → context
- 第二行：瞬时状态（复制提示 "已复制 N 字符" /MCP/LSP）→ 快捷键 hints

瞬时高亮通过 `MODEL_HIGHLIGHT_UNTIL` 等 atom + Instant 控制 1.5s 消失。复制提示通过 `COPY_CHAR_COUNT` + `COPY_MESSAGE_UNTIL` 控制 2s 消失。

## ACP 数据流

```
用户 Enter → InputArea → SUBMIT_TX
                            ↓
            spawn_submit_consumer → acp_client.prompt()
                            ↓
                MpscClientTransport.send_request()
                            ↓
            ACP Server (tokio::spawn) → ExecutorEvent
                            ↓
                TransportEventSink.push_event()
                            ↓
            AcpTuiClient.pump_notifications() → AcpNotification
                            ↓
            spawn_kit_notifier → AcpEventData::decode
                            ↓
                bridge_tx → spawn_acp_bridge
                            ↓
            dispatch_and_notify → BridgeState → Atom 写入
                            ↓
            ratatui-kit 组件 use_store → 自动重渲染
```

**[TRAP]** TUI 层数据必须通过 ACP 协议到达 ACP 层，禁止直连 peri-agent / peri-middlewares 运行时。本地状态变更（如 ModelPanel 切 alias）通过共享 `Arc<RwLock<PeriConfig>>` 直接 write，ACP server 持同一 Arc。

## Rewind 完整路径

1. 双击 Esc（500ms 内）或 ACP `RewindPreview` 事件 → 设置 `REWIND_PREVIEW` + `POPUP_KIND = Rewind`
2. RewindPopup 渲染预览：消息列表 / 文件改动（Tab 切换视图）
3. Enter → `REWIND_ACTION_TX.send(Confirm { target_message_id, revert_files })`
4. `spawn_rewind_consumer` → `acp_client.send_raw_request("session/execute-command", { command: "/rewind", args: {...} })`
5. ACP server RewindCommand 完成 → 推送 `ExecutorEvent::RewindCompleted` → kit_notifier → ViewCommit → VIEW_MODELS 刷新

## 关键 [TRAP]

- **绝不推远程** —— 见 `~/.claude/projects/-Users-konghayao-code-ai-perihelion/memory/never-push-remote.md`
- **`AcpNotification::AgentEvent` 暂忽略** —— 携带的 AcpEvent DTO（TurnCommitted/StateSnapshotMeta/CompactCompleted）属于低频 v2 事件，kit 以 UnstableEvent 为主通道
- **SUBMIT_TX / REWIND_ACTION_TX / THREAD_LOAD_TX / RESIZE_TX 必须 OnceLock 而非 lazy** —— rx 端在 entry::run_kit_fullscreen 中 spawn 任务，必须在 build_app_and_acp 完成后由 entry 显式 `set(tx)`
- **`ACP_STATE.is_loading` 是 InputArea 判断 loading 的唯一来源** —— 不能依赖 `props.loading`（渲染时快照，事件触发时可能已变化）
- **`RENDER_CACHE` 与 `VIEW_MODELS` 分离** —— message_area 不直接 touch ViewModel；渲染预计算在 render_bridge 独立 task 中完成
- **`/clear` 必须同步重置 RENDER_CACHE** —— 否则旧缓存残留导致消息闪烁
- **render_bridge 宽度变化触发全量重建** —— terminal resize 通过 RESIZE_TX 通知，rebuild_all 重建所有 entry
- **PERI_CONFIG_HANDLE 共享 Arc** —— ACP server 持同一 Arc，write 后立即可见；service_snapshot 2s 内捕获变化刷新 SERVICE_SNAPSHOT
- **drain_input_buffer 必须在 TurnDone 而非 TurnInterrupted** —— Interrupted 表示用户主动打断，不应自动续跑
- **FILE_LIST 扫描深度=2** —— 浅扫避免 node_modules / target 等大目录爆栈；MAX_FILES=500 上限
- **`scan_cwd_files_shallow` 必须 spawn_blocking 包裹** —— std::fs 同步操作阻塞 async runtime
- **ModelPanel 切换不立即触发 status bar 刷新** —— 依赖 service_snapshot 2s tick；如需立即刷新可手动改 SERVICE_SNAPSHOT atom
- **ThreadBrowser Enter 后必须手动关闭面板** —— load_session 的 ViewCommit 通知不会自动关面板
- **process_resource_monitor 独立新建** —— service_snapshot 不复用 ServiceRegistry.resource_monitor（非 Arc），新建实例采样进程级数据不影响正确性
- **面板 Up 键 deadlock 已修复** —— ReactiveHandle read/write 不能在同一个表达式中自死锁（bdf7bb55）

## 测试风格

- `kit::input_area::tests` —— EditorState / 多行渲染 / @mention 触发 / slash 触发 / 文件过滤
- `kit::acp_events::tests` —— drain_input_buffer 顺序 / 空队列 / SUBMIT_TX 缺失安全性
- `kit::service_snapshot::tests` —— tick_once / 派生 provider+model / cwd 文件扫描 / MAX_FILES 上限
- `kit::submit_consumer::tests` —— 空文本跳过 / 首次创建 session / shutdown / dropped tx
- `kit::thread_load_consumer::tests` —— 空 thread_id 跳过 / shutdown / dropped tx
- `kit::rewind_action::tests` —— /rewind RPC payload / has_session 检查 / 双击 Esc 时序

`#[serial]` 用于依赖全局 atom 的并发敏感测试。

## 编码规范

- `#![allow(clippy::needless_update)]` —— ratatui-kit element! 宏展开触发，模块级抑制
- 字符串截断用 `chars().take(N)`（CJK 安全）
- 终端列宽用 `unicode-width`
- 快捷键：禁止 `Shift+字母`；优先 `Ctrl+字母`；不用 PageUp/Down
- 测试隔离：`App::save_config(cfg, self.config_path_override.as_deref())`
- `std::sync::RwLockReadGuard` 不是 Send → async 跨 `.await` 用 `parking_lot::RwLock`
