# peri-tui 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11（面板配置提交、下载生命周期与内存统计）
> 依据：peri-tui/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/tui-acp-data-flow.md、源码

## 架构速览

- 数据流：`ACP transport → acp_client pump（interaction_lifecycle 在 forward 前分配 semantic owner；ordinary notification 按 Stable/Transitioning/NoSession 路由）→ acp_notifier（owner + RequestId debug JSON + payload；同步发布 commands/plan/spinner/context 后转发）→ acp_bridge（publish_if_owned 持 operation gate 完成 final owner/projection check；bridge-local 50 ms single-pending scheduler 合并主 Agent Streaming publication，reset/terminal/receiver-close/shutdown 失效 pending）→ dispatch_for_bridge（canonical ingest + PublicationIntent）→ VIEW_MODELS/ACP_STATE → components；CurrentTurn mutation lazy projection，response action 只能按 owner first-claim，terminal cleanup compare-and-clear 同 owner surface`
- 提交链路：`InputArea → SubmitRequest → SUBMIT_TX → submit_consumer → AcpTuiClient::ensure_session（acp_client/client/session.rs）/ prompt（acp_client/client/requests.rs）→ ACP transport`；取消经 `CANCEL_TX → spawn_cancel_consumer → AcpTuiClient::cancel`
- 入口：`main.rs:613 main` → `run_tui`（:847）→ `kit/entry.rs:52 run_kit_fullscreen`（spawn kit 各链路）→ `launch.rs:41 build_app_and_acp`（App + AcpTuiClient + consumer 装配）
- 稳定不变量：ACP 是交互与 Agent 执行边界（ARC-BOUNDARY-001）；`BridgeState` 是事件 → 状态边界（切换会话/重置须过滤陈旧事件，BRIDGE_RESET_COUNTER 清理）；render body 不写 atom；hooks 稳定顺序；交互事件按焦点/优先级分发；用户可见文本走 i18n 双 FTL（i18n/mod.rs:35 `tr`）；文本按 Unicode 字符边界/显示宽度处理

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改主题下载、进度终态与重复准入 | `src/kit/panels/theme.rs` + `src/kit/panels/theme/download.rs` + `src/kit/panels/theme_download_test.rs` + `src/kit/popups/download_progress.rs` | `trigger_download_themes` / `DownloadRun::try_claim` / `run_download` / `download_file` / `DownloadRun::finish` | Ctrl+D 在 open/spawn 前同步取得唯一任务 lease；完整进度由任务持有并投影到 atom，关闭弹窗不取消任务或释放准入。目录/HTTP 错误与 abort 都收敛到完成态，catalog/notification/progress 写完才释放 owner；保留原下载 URL、HOME 保存路径与串行文件处理 |
| 改内存统计、回收与单位转换 | `src/alloc_config.rs` + `src/alloc_config_test.rs` | `query_stats` / `query_breakdown` / `alloc_collect` / `dump_stats` / `os_rss_mb` | 所有 jemalloc epoch 刷新与缓存读取共用私有快照锁；sysinfo 0.39 的 RSS 是 bytes，展示转换为 MiB；OS 采样与日志输出在锁外。回归位于 `alloc_config::tests`，覆盖真实并发采样、单位契约与子进程 MALLOC_CONF 隔离 |
| 改 System Reminder 展示 | `src/kit/acp_types/event_data.rs` + `src/kit/acp_events/system.rs` + `src/kit/tui_render_unit/{reminder,fold}.rs` + `src/kit/message_area/{entry_nav.rs,render/user.rs}` | `AcpEventData::SystemReminder`；canonical event handler；`TuiSystemReminder::from_wire` / `legacy`；`FoldKey::SystemReminder`；reminder render | canonical 路径只按 DTO 字段建模；reminder 默认只显示 dim 非 bold header，左侧以 `▸/▾` 表达折叠态，点击/Enter 经通用 fold override 展开正文；`detect_reminder` 仅处理无结构化事件的 legacy 文本 fallback；reminder 独立于用户气泡 |
| 改消息流渲染 | `src/kit/message_area/mod.rs` + `message_area/render.rs` + `message_area/vm_cache.rs` + `message_area/selection.rs` + `grid.rs` | `MessageArea`；`vm_to_lines_cached`；`MarkdownLineCache`；`SlotLines::{single,composite}`；`SlotIndex::{new,visual_lookup,logical_lookup}` | 变化 Markdown 复用 stable rendered chunks 与 local wrap map；slot 用 chrome/tail slice + stable chunk `Arc` composite access，避免把稳定 `Line` clone 回 contiguous Vec；全局坐标通过 O(VM) prefix + slot-local lookup，production 不 flatten transcript wrap map；Unicode 按显示宽度映射 |
| 改图片链接预览与打开行为 | `src/kit/message_area/{handlers,image_action,hits}.rs` + `src/kit/image_overlay.rs` + `src/kit/atoms.rs` | `register_image_hover`；`schedule_image_preview_hover`；`resolve_preview_target`；`request_preview`；`register_image_click` | 图片链接 Moved 命中后即时高亮，但需在同一目标稳定悬停 300 ms 才进入预览源；移出或换链接立即清空并使旧计时任务失效；预览仲裁为稳定 hover > cursor > focus，点击打开仍走 T5 校验与参数化命令 |
| 改图片读取与资源限制 | `src/kit/image_safety.rs` + `image_safety_test.rs` | `grade_path` / `read_validated_image` / `sanitize_for_terminal` / `classify_url` | 路径分级与头部/MIME/尺寸/字节限额在真实读取入口校验，显示文本过滤控制字符；测试经原私有 tests 模块挂载 |
| 改 keepgoing 按钮行为 | `src/kit/message_area/{handlers,hits,footer}.rs` + `src/kit/submit_consumer.rs` | `register_keepgoing_click`（handlers.rs:69）；`KEEPGOING_DEBOUNCE`（:32）；`compute_keepgoing_rect`（hits.rs:279）；`build_footer_lines`（footer.rs:100）；`handle_keepgoing_submit` | 命中最近一帧按钮 rect；冷却期内 Consumed 不提交；点击发送空内容 prompt，服务端仅继续 loop；布局与命中使用同一 KeepGoingLayout（ARC-KEEPGOING-001） |
| 改事件消费（新增/变更事件） | `src/kit/acp_bridge.rs` + `src/kit/acp_events/` + `acp_notifier.rs` | `spawn_acp_bridge_inner` / `PublicationScheduler`；`dispatch_for_bridge` → `PublicationIntent`；`push_view_models` | 事件先 canonical ingest，再由 bridge 合帧主 Agent Streaming publication；首 text/reasoning、block boundary 与 terminal 可立即，50 ms fixed deadline 不 debounce；reset/session/receiver-close/shutdown 失效 pending；终止事件必须离开 loading；root usage_update 仍按 session envelope 处理 |
| 改 Goal 状态栏与详情面板 | `src/kit/status_bar.rs` + `src/kit/panels/goal.rs` + `src/kit/acp_{notifier,bridge}.rs` + `src/kit/acp_events/system.rs` + `src/kit/session_boundary.rs` | `AcpEventData::GoalSnapshot`；`handle_goal_snapshot`；`GOAL_SNAPSHOT`；`PanelKind::Goal` | notifier 只解码并转发；bridge 完成 session ownership 校验后写 Goal atom；状态栏显示状态和主动接续次数，点击打开只读详情；session transition 清快照并关闭面板 |
| 改 compact 信息展示 | `src/kit/acp_notifier/agent_event.rs` + `src/kit/acp_types/event_data.rs` + `src/kit/acp_events/compact.rs` + 双语 `locales/*/main.ftl` | `decode_agent_event`；`handle_compact_started` / `handle_compact_completed` | 单一 ACP `peri/agent_event` 路径消费 started/completed；完成时按 strategy 显示压缩类型，并展示受影响消息数、估算节省 token、files/skills，manual 仍保留跨 replay note，auto 不触发 session/load |
| 改输入/滚动/选择 | `src/kit/input_area.rs` + `message_area/scroll.rs` + `focus_router.rs` | `InputArea`（input_area.rs:117）；`scroll::handle_event`（scroll.rs:516，滚轮节流/拖拽选中/键盘滚动）；`focus_router::active_layer`（:105）、`classify_global_shortcut`（:117）、`message_accepts_key`（:147）、`input_accepts_key`（:190） | 消息区只处理滚轮、编辑区处理键盘（按焦点层分发）；弹窗/面板遮挡时鼠标清理残留（scroll.rs:548 `is_occluded`）；同优先级按注册序分发（keepgoing 须先于 scroll） |
| 改命令面板（slash/@mention） | `src/kit/slash_completion.rs` + `input_area.rs` + `submit_request.rs` | `SlashCompletion`、`filter_slash_items`；词法 `detect_slash_token`、`apply_slash_selection`；本地命令解析 `parse_submit_request`；UI 命令上送 `AcpTuiClient::register_ui_commands`（acp_client/client/requests.rs） | 词法在 TUI、路由裁决在服务端 CommandRegistry（command-system.md）；补全模糊仅发生在搜索层，提交须完整全名；`/rewind` 等经 ACP 协议请求，不本地执行 |
| 改内嵌 host / print 退出 | `src/acp_client/deployment.rs` + `src/launch.rs` + `src/cli_print.rs` | `AcpDeployment::shutdown`（deployment.rs:20）/`run`（:27）；`teardown_app`（launch.rs:199）；`run_print`（cli_print.rs:25） | 显式 client close 打破 pump/atoms 的 Arc 保活后等待原 host；print 成功和 new/prompt 错误共用退出路径，资源 Incomplete/TaskFailed 为可见退出错误，HTTP 遥测失败保留旁路报告（ARC-HOST-SHUTDOWN-001） |
| 改配置/启动流程 | `src/main.rs` + `src/launch.rs` + `src/config/` + `src/app/mod.rs` | `main`；`build_runtime`；`run_tui`；`build_app_and_acp`；`App::new`；`TuiConfig::from_extra`；`save_effective` | `PeriConfig` 等类型事实源在 `peri-acp/src/provider/config.rs`，`config/mod.rs` 仅 re-export；CLI 权限使用 `--permission-mode` / `--dangerously-skip-permissions`，默认 Bypass；配置源句柄 `CONFIG_SOURCE_HANDLE` 启动时 set 一次，加载与保存共用同一决策；`teardown_app` 收尾 hooks/MCP 后经 AcpDeployment 关闭并 join ACP host/Langfuse |
| 改 `peri meta session` CLI | `src/main.rs` + `src/cli_meta.rs` + `src/thread/mod.rs` | `MetaAction::Session`；`try_run_meta_before_configuration`；`run_meta_session`；`SessionMetaDtoV1`；`open_thread_store_read_only` re-export | Meta 在 settings/config/env 初始化前按受限 grammar 路由；先校验 UUID，再经 `peri-resources` 只读 seam 调用 `ThreadStore::load_meta`；human/JSON 使用九字段 allowlist，稳定错误与退出码由 adapter 映射；不进入 ACP、Agent、Runtime 或 TUI session owner |
| 改 TUI MCP panel 生命周期 | `src/app/mod.rs` + `src/app/service_registry.rs` + `src/kit/acp_events/system.rs` + `src/launch.rs` | `spawn_mcp_init`；`ServiceRegistry::mcp_task_owner`；`handle_oauth_completed/restored`；`shutdown_mcp_pool` | panel 部署容器保留 non-Clone `McpTaskOwner`，init 和 OAuth-event reconnect 经 weak spawner 准入；teardown 按 pool begin-close → owner join → pool close，并检查 service transaction report，Incomplete 不得记为已关闭（ARC-HOST-SHUTDOWN-001） |
| 改 Plugin Discover 搜索与远端条目操作 | `src/kit/panels/plugin.rs` + `plugin/{discover,discover_handler,search_request}.rs` | `DiscoverState::{begin_search,complete,reset_session}`；`decide` / `apply` / `handle_event`；`launch_search` | 单一 Discover owner 持输入、结果、选中条目身份与取消 token；键盘/鼠标统一动作；RPC response 按 generation 和 session/reset identity 接收，编辑、替换查询、关闭/Drop、会话重置使旧请求失效；无 request ID 的异步通知不作为面板结果源；空结果不回退本地缓存，错误保留输入以重试 |
| 改 service snapshot / thread 列表刷新 | `src/kit/service_snapshot.rs` | `spawn_service_snapshot`；`tick_once`；`SlowSnapshotRefresh` | thread 列表经 `ThreadStore::list_thread_entries(cwd)` 获取轻量投影，存储层完成 cwd/hidden/空 thread 过滤；不得退回会计算 message content size 的完整 `list_threads` |
| 改 ACP 请求发送 / reverse interaction 生命周期 | `src/acp_client/client/{pump,interaction,requests}.rs` + `src/acp_client/interaction_lifecycle.rs` + `interaction_settlement.rs` + `interaction_response.rs` | `AcpTuiClient::spawn_pump` / `respond_interaction` / `publish_if_owned`；`InteractionLifecycle::{register_reverse,claim,begin_transition,open_prompt}`；`PromptLease` / `TransitionLease` / claimed batch lease | 单一 owner registry 管 Permission/Elicitation 的接受、claim 与 terminal；operation gate 线性化 UI publication、response、cancel 和 session transition；Drop settlement 使用 weak transport/notifier；headless print 仍按 token claim，且不依赖 kit atoms |
| 改 session new/load/delete 的 TUI 投影 | `src/acp_client/client/session.rs` + `src/kit/session_boundary.rs` + `src/kit/thread_load_consumer.rs` | `new_session` / `load_session` / `delete_session`；`ThreadLoadDispatcher::send`；`reserve_session_load` / `open_prompt_after_session_loads`；`project_session_boundary` | client 在 route Stable commit 前同步投影 ACTIVE_SESSION_ID，并统一清 interaction atom/popup/panel/confirm、loading/input/rewind/todo/history；普通 load 在同步入队边界取得引用计数 reservation，ensure/prompt 在选择 Stable/open lease 前等待，request drop 自动释放；compact 先 reserve replay 再 drain input（ARC-SESSION-LOAD-001）；new transition 用容量 64 的 exact-target FIFO 覆盖 response→commit 窗口 |
| 改消息累积模型 | `src/kit/acp_types/current_turn.rs` + `current_turn/{streaming,subagents,projection}.rs` + `src/kit/acp_events/{streaming,render}.rs` | `CurrentTurn`（`acp_types.rs` re-export）；`append_text` / `start_tool` / `start_subagent` / `stop_subagent`；`view_models` → `sync_cache` | 单一 canonical state 原地累积，只置 dirty；投影按 frozen segments → trailing → Agent/child 配对顺序增量更新，共享 `im::Vector`；停止后复用 child ID 创建新 occurrence，后续事件从尾部路由 |
| 改问答选项、输入与提交决策 | `src/kit/panels/ask_user.rs` + `ask_user/{form,typing}.rs` | `AskUserPanel`；`FormState::{handle_key,toggle_option,begin_custom_input,reset_for_owner_change}`；`FormOutcome` | 单一表单 state 管 focus/选择/编辑器；owner fingerprint 变更同步清答案与滚动，无通知 render reset；提交与确认弹窗只由面板携原 owner 执行 |

## 关键控件/组件（src/kit/）

| 组件 | 文件 | 职责 |
| --- | --- | --- |
| MessageArea（消息流 + footer + keepgoing 按钮） | `message_area/mod.rs`（MessageArea :91） | 消息流渲染主组件；滚动/点击/选区事件注册；footer 行与 keepgoing 按钮命中 |
| footer/spinner 行 | `message_area/footer.rs` | `build_footer_lines`（:100）：loading spinner / summary / todo 行 + `KeepGoingLayout`（:85）；防抖期按钮禁用样式 |
| GridSpec 网格 | `message_area/grid.rs` | 断点（`Breakpoint` :17）与行首/续行前缀宽度；全部行渲染的对齐基准 |
| scroll 滚动引擎 | `message_area/scroll.rs` | `handle_event`（:516）；滚轮节流、拖拽选中、键盘滚动、吸底跟随（`should_follow_after_user_scroll` :378） |
| 语义选区 | `message_area/selection.rs` | 拖拽选区与语义复制（`map_slice_to_semantic` :469，复制时剥视觉前缀） |
| markdown 渲染 | `markdown/`（convert.rs / code_block.rs / table.rs / scan.rs） | 文本 → 带样式的行渲染；代码块、表格、扫描 |
| subagent 工具行 | `message_area/render/group.rs` | `render_subagent_group_lines`（:29）、`subagent_tool_line`（:92，固定 2 格缩进 `SUBAGENT_TOOL_INDENT` :22、label 无 bold）、`subagent_error_reason_line`（:168，错误不弱化） |
| InputArea（输入区） | `input_area.rs` | 编辑、@mention、slash 补全、提交分发（`input_area/submit.rs::dispatch_submit_request` :21）；多行渲染按显示宽度 |
| input_history（输入历史） | `input_history.rs` | `push_history`（:23）/`history_up`（:54）；持久化 `~/.peri/input-history.json`（唯一存储，`load_history` :119） |
| StatusBar（状态栏） | `status_bar.rs` | `StatusBarProps`（:353）/`StatusBar`（:361）：Row1/Row2/NotifRow、模型点击区、权限模式显示 |
| BgTaskArea（后台任务栏） | `bg_task_area.rs` | `BgTaskArea`（:46）：bg agent 运行中条目 + 动画 |
| Welcome（空态欢迎屏） | `welcome.rs` | `Welcome`（:94）：logo + 会话空态引导 |
| AppShell / SessionColumn | `app_shell.rs` + `layout.rs` | `AppShell`（app_shell.rs:23）顶层外壳；`SessionColumn`（layout.rs:100）+ `layout_plan`（:75）垂直布局 |
| SlashCompletion / MentionPopup | `slash_completion.rs` + `mention_popup.rs` | slash 命令补全弹窗（fuzzy 过滤，仅搜索层）；文件 @mention 弹窗 |
| PanelOverlay / PopupOverlay | `panel_overlay.rs` + `popup_overlay.rs` | 面板层（`PanelOverlay` :34）与居中弹窗层（`PopupOverlay` :38，`open_popup` :99） |
| 面板目录 PanelRegistry | `panel_registry.rs` | 面板种类→渲染函数注册表（`render` :438、`open_panel` :475、快捷键 `from_shortcut` :448） |
| AskUserPanel（问答面板） | panels/ask_user.rs + ask_user/{form,typing}.rs | 面板保留主题布局、命中区域与 owner 响应副作用；`FormState` 处理选择/导航/编辑/答案构造，鼠标与 Space 共用操作 |
| 模型与 Workflow 面板回归 | panels/{model,workflow}_test.rs | 私有 tests 模块挂载；覆盖窄屏列宽、run 选择与 Unicode 截断，生产渲染仍在对应面板 |
| 模型配置编辑与提交 | panels/model.rs + model/edit.rs + model/commit_test.rs | `switch_active_alias` 保留面板/快捷弹窗共同入口；`edit_field` 修改唯一 PeriConfig，`commit_snapshot` 在释放配置锁后统一保存、通知、按 `ModelChange` 投影并推送 ACP；inactive 编辑不切换展示，alias 切换独有高亮；真实磁盘/MPSC 回归覆盖保存失败仍推送 |
| tool 展示 | `tool_display.rs` + `tool_semantics.rs` | `format_tool_name`（tool_display.rs:8，本地化动词）；skill/todo 语义展示（tool_semantics.rs:65/:79）、todo diff（:115） |

## 子系统

### ACP 事件链（src/kit/acp_notifier.rs / acp_bridge.rs / acp_events/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 通知消费与状态发布 | kit/acp_notifier.rs | `spawn_kit_notifier_with_client` / `forward_notification` / `handle_session_update` / `convert_agent_event`；commands/plan/spinner/context 保持同步发布后送 bridge，reverse 投递失败按 owner 结算；transport 关闭复位 loading + 断连提示 |
| 标准 session/update 解码 | kit/acp_notifier/session_update.rs | `decode_commands` / `decode_stream_update` / `StreamUpdate`；纯解析返回事件及可选 spinner 计数；root 缺省 cacheReadTokens 只更新进度并保留旧 sample，显式零值在 input>0 时替换为零样本，字段可用但 input=0 或 cached>input 时清空；auxiliary/replay 不更新父 sample |
| Cache coverage 提示 | kit/acp_events/turn.rs | `handle_cache_usage_updated` / `inject_cache_coverage_warning_if_needed` / `handle_turn_done`；配置开启时每次 root sample 在 0<cached/input<0.8 时即时注入提示，TurnDone 清 pending，不补发或撤销提示；wire 契约在 acp_notifier_test.rs，逐次提示在 acp_events_test/turn_archive_test.rs |
| Agent DTO 与 reverse wire | kit/acp_notifier/{agent_event,interaction}.rs | `decode_agent_event`；`handle_elicitation` / `handle_request_permission` / `parse_elicitation_questions`；reverse 将 owner、request ID、payload 封装为同一 envelope 后投递 |
| 状态桥 | kit/acp_bridge.rs | `spawn_acp_bridge`：interaction 经 `AcpTuiClient::publish_if_owned` 后才同步写 UI；普通事件维护 `BridgeState` 并检测 BRIDGE_RESET_COUNTER |
| 事件分派 | kit/acp_events/mod.rs | `dispatch_and_notify`（:301）；`SessionPhase`（:149）/`BridgeState`（:158） |
| 流式/工具/边界/系统 handler | kit/acp_events/{streaming,tool,turn,system}.rs | `handle_text_chunk`（streaming.rs:26）、`handle_tool_started`（tool.rs:13）、`handle_turn_done`（turn.rs:12）、`handle_hitl_pending`（system.rs:117）等；subagent（subagent.rs:6/:33）、agent（agent.rs:8）、compact（compact.rs:11/:17） |
| 渲染管线 | kit/acp_events/render.rs | `push_view_models`（:25）/`push_acp_state`（:615）/`push_view_models_for_reset`（:595）/`handle_plan_update`（:672） |

### 状态与模型（src/kit/atoms.rs + acp_types.rs）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 全局 atoms | kit/atoms.rs | `ACP_STATE`（:220）/`VIEW_MODELS`（:249）/`SUBMIT_TX`（:256）/`CANCEL_TX`（:257）；`init_atoms`（:653） |
| 消息累积模型入口 | kit/acp_types.rs + acp_types/{current_turn,tool_card,event_data}.rs | `acp_types.rs` re-export `CurrentTurn`、`ToolCardAccumulator`、`SubAgentAccumulator` 与 `AcpEventData`，canonical turn state 与生命周期在 current_turn.rs |
| 主回合流式变更与子回合路由 | kit/acp_types/current_turn/{streaming,subagents}.rs | `append_text` / `append_reasoning` / `flush_text_segment` / `start_tool`；`start_subagent` / `stop_subagent` / `append_subagent_text`；冻结边界与 rolling hash 保持同一 state，child 路由取最后一次 occurrence |
| 回合渲染投影 | kit/acp_types/current_turn/projection.rs | `view_models` / `sync_cache` / `sync_segments` / `sync_trailing` / `pair_agent_tool_cards`；dirty 读取才投影，冻结片段复用、trailing 一次性消费 freeze，最后配对计数 |

### 输入与提交（src/kit/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 提交/清除/keepgoing 消费者 | kit/submit_consumer.rs | `spawn_submit_consumer`（:46）、`handle_submit`（:91）、`handle_keepgoing_submit`（:202）、`spawn_cancel_consumer`（:412） |
| 本地请求解析 | kit/submit_request.rs | `parse_submit_request`（:49，/clear、/rewind、/export 等本地命令） |
| 键盘/鼠标/焦点分发 | kit/event_handlers.rs、focus_router.rs、mouse_router.rs | `register_global_handlers`（event_handlers.rs:74，Ctrl+C 判定 :53）、`register_root_handlers`（:156）；`active_layer`/`classify_global_shortcut`（focus_router.rs:105/:117） |
| 快照与回写消费者 | kit/service_snapshot.rs、rewind_action.rs、hitl_response.rs、ask_user_action.rs、thread_load_consumer.rs | `spawn_service_snapshot`（:66，CPU/MEM/MCP 2s 轮询）、`spawn_rewind_consumer`（rewind_action.rs:123）、`spawn_hitl_response_consumer`（hitl_response.rs:40）、`spawn_ask_user_consumer`（ask_user_action.rs:47）、`spawn_thread_load_consumer`（thread_load_consumer.rs:30） |

### 面板与弹窗（src/kit/panels/ + popups/ + overlay）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 面板目录（tasks/cron/agent/model/config/thread_browser/mcp/plugin/…） | kit/panels/ + kit/panel_registry.rs | `open_panel`（panel_registry.rs:475）；面板渲染按 PanelKind 分发（:438）；`PanelOverlay`（panel_overlay.rs:34） |
| Plugin 面板装配与展示 | kit/panels/plugin.rs + plugin/{data,render,search_handler,panel_handler}.rs | `PluginPanel` 保持公共入口；`data` 管本地目录缓存，`render_discover_list` 保留 loading/error 下的可编辑输入；`handle_search_event` 路由 Discover 与 marketplace 输入，`handle_panel_event` 保留其他 tab 与既有安装操作生命周期 |
| Plugin 搜索生命周期回归 | kit/panels/plugin/search_request_test.rs | `plugin_search_*`：真实 mpsc 请求/响应及 notifier → bridge → render；覆盖错误重试、同 query 乱序与无身份旧通知、session switch/reset、关闭/Drop、空/无效结果、键鼠提交可达和远端条目身份；回调暂拒收保留结果、可取消延迟重试，接收后只投影一次 |
| 弹窗（HITL/AskUser/OAuth/Confirm/Rewind/下载进度） | kit/popups/ + kit/popup_overlay.rs + kit/event_handlers.rs | `open_popup`/`close_popup`/`is_popup_active`；Rewind Enter 由根级 Global 模态仲裁发送既有 `REWIND_ACTION_TX`，鼠标与渲染留在 popup |

### App/配置/启动（src/app/ src/config/ src/acp_client/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 应用状态 | src/app/mod.rs | `App`（:29）/`App::new`（:47）；`spawn_mcp_init`（:127）、`get_compact_config`（:182）；子模块 agent.rs/cron_state.rs/provider.rs/service_registry.rs/setup_wizard/ |
| 配置 | src/config/ | `PeriConfig` 等 re-export 自 `peri-acp/src/provider/config.rs`（事实源）；`TuiConfig`（tui_config.rs:9，本地扩展，`from_extra` :48 / `sync_to_extra` :80）；`save_effective`（mod.rs:21） |
| ACP 客户端入口与构造 | src/acp_client/client.rs | `AcpTuiClient` / `AcpNotification` / `ClientProjectionMode` 保持公共路径；`new_with_mode` 装配 lifecycle、weak notifier 与 Drop settlement worker |
| ACP 通知泵与 reverse wire admission | src/acp_client/client/pump.rs | `spawn_pump` / `run_pump` / `plan_reverse_request` / `flush_buffered`；按原始接收顺序解码，ordinary notification 经 lifecycle 路由，reverse 在投递前注册 owner |
| Session 切换与 load reservation | src/acp_client/client/session.rs | `ensure_session` / `new_session` / `load_session` / `delete_session`；`SessionLoadReservation` / `reserve_session_load` / `open_prompt_after_session_loads` 保持同步 reservation 与 operation gate 的线性化 |
| ACP 请求封装 | src/acp_client/client/requests.rs | `register_ui_commands` / `prompt` / `prompt_with_bg_results` / `cancel` / `set_config_option` / `send_raw_request`；prompt 持 lease，返回后在 gate 内结算 |
| Interaction response 与 UI publication | src/acp_client/client/interaction.rs | `respond_interaction` / `publish_if_owned` / `reject_interaction` / `settle_claims_owned`；owner first-claim 与同步 UI publication 共用 gate，通知仅升级 weak sender |
| ACP client 契约测试 | src/acp_client/client_test.rs + client_reverse_test.rs | `client::tests` 验证 done identity / 删除过滤，`client::reverse_tests` 覆盖 owner、gate、startup/load reservation 与 Drop settlement |
| 启动/CLI | src/main.rs、launch.rs、cli_args.rs、cli_plugin.rs、update.rs | `main`（main.rs:613）/`run_tui`（:847）；`build_app_and_acp`（launch.rs:41）/`teardown_app`（:199）；`run_kit_fullscreen`（kit/entry.rs:52）；插件/更新 CLI 子命令 |

### 设备同步与线程存储（src/sync/ src/thread/ src/components/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 设备间同步 CLI | src/sync/ | re-export：`run_sync_receiver`/`run_sync_sender`/`run_receive_cli`/`run_send_cli`/`run_device_command`（mod.rs；main.rs:523-530 调用）；子模块 protocol/noise_session/crypto/packer 等 |
| 线程存储 | src/thread/mod.rs | 仅 re-export `ThreadStore`/`ThreadMeta`/`SqliteThreadStore`（事实源 peri-acp-types / 实现在 peri-resources） |
| 通用组件 | src/components/textarea/、spinner/ | 文本编辑 widget（widget.rs/state.rs/word.rs/history.rs）；动画 spinner（animation.rs/verb.rs） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-BOUNDARY-001：TUI 交互主路径经 ACP transport；不得从 TUI 直驱 Agent/Middleware 运行时
- ARC-EVENT-001：事件链路单事实源 Agent →(ACP 映射) → TUI；新增事件须覆盖发射、映射与消费；终止事件必须使客户端离开 loading
- Cache coverage 用户现场验收仍在 [#114 active issue](../../spec/issues/2026-09-01-long-context-cache-evicted-each-round-114.md)；当前 wire 与逐样本提示契约见 ARC-EVENT-001。
- ARC-KEEPGOING-001：空白 user prompt（`MessageContent::is_empty()` 判空）是「继续跑 loop」指令，唯一生产者是 TUI keepgoing 按钮；空历史 + 空白 prompt 时服务端短路且必须 push_done
- ARC-CANCEL-001：cancel 按 (session_id, turn_id, attempt_id) 三元组定位；TUI 只经 ACP 发送 cancel，幂等判定与终态归 Agent 层
- ARC-HITL-001：Permission 与 AskUser 独立能力；TUI reverse interaction 由 semantic owner registry、operation gate、prompt/transition leases 与 token-aware UI terminalization 共同 first-claim
- ARC-SECRET-001：真实密钥/token/连接串不得写入界面、日志、错误响应或测试 fixture
- ARC-HOST-SHUTDOWN-001：TUI MCP panel 保留 external task owner，OAuth-event reconnect 经 owner 准入并按固定顺序 teardown
