# peri-tui 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-25
> 依据：peri-tui/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/tui-acp-data-flow.md、源码

## 架构速览

- 数据流：`ACP transport → acp_client pump（interaction_lifecycle 在 forward 前分配 semantic owner；ordinary notification 按 Stable/Transitioning/NoSession 路由）→ acp_notifier（owner + RequestId debug JSON + payload，不写 UI）→ acp_bridge（publish_if_owned 持 operation gate 完成 final owner/projection check 与同步发布）→ dispatch_and_notify → VIEW_MODELS/ACP_STATE → components；response action 只能按 owner first-claim，terminal cleanup compare-and-clear 同 owner surface`
- 提交链路：`InputArea（input_area.rs:148）→ SubmitRequest（submit_request.rs:5）→ SUBMIT_TX（atoms.rs:256）→ submit_consumer（submit_consumer.rs:46）→ AcpTuiClient::ensure_session / prompt（client.rs:719/:879）→ ACP transport`；取消经 CANCEL_TX（atoms.rs:257）→ `spawn_cancel_consumer`（submit_consumer.rs:412）
- 入口：`main.rs:409 main` → `run_tui`（:624）→ `kit/entry.rs:52 run_kit_fullscreen`（spawn kit 各链路）→ `launch.rs:41 build_app_and_acp`（App + AcpTuiClient + consumer 装配）
- 稳定不变量：ACP 是交互与 Agent 执行边界（ARC-BOUNDARY-001）；`BridgeState` 是事件 → 状态边界（切换会话/重置须过滤陈旧事件，BRIDGE_RESET_COUNTER 清理）；render body 不写 atom；hooks 稳定顺序；交互事件按焦点/优先级分发；用户可见文本走 i18n 双 FTL（i18n/mod.rs:35 `tr`）；文本按 Unicode 字符边界/显示宽度处理

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改消息流渲染 | `src/kit/message_area/mod.rs` + `message_area/render/` + `grid.rs` | `MessageArea`；`render_tool_card_lines` / `completed_header_suffix`；`GridSpec::grid_for` | 组件只读 VIEW_MODELS 渲染、render body 不写 atom；Read 后缀从 canonical output 保留行数与 truncation 状态；截断/坐标按 Unicode 显示宽度 |
| 改 keepgoing 按钮行为 | `src/kit/message_area/mod.rs` + `footer.rs` + `src/kit/submit_consumer.rs` | 点击 handler（mod.rs:956，Global+High，须在 scroll handler 前注册）；防抖常量 `KEEPGOING_DEBOUNCE`（mod.rs:58）+ `KEEPGOING_BLOCKED_UNTIL`；按钮布局 `build_footer_lines`/`KeepGoingLayout`（footer.rs:100/:85）、rect 每帧更新（`compute_keepgoing_rect` mod.rs:2040）；提交 `handle_keepgoing_submit`（submit_consumer.rs:255） | 命中检测用最近一帧按钮 rect；防抖期内点击 Consumed 不提交；提交 = 空白 user prompt（服务端不插消息仅继续 loop）；契约 ARC-KEEPGOING-001（唯一生产者） |
| 改事件消费（新增/变更事件） | `src/kit/acp_events/` + `acp_notifier.rs` + `acp_bridge.rs` | `dispatch_and_notify`（acp_events/mod.rs:301，穷尽 match 分派到 streaming/tool/turn/system/subagent/agent/compact handler）；`AcpEventData`（acp_types/event_data.rs:27）；`convert_agent_event`（acp_notifier.rs:92）；渲染产出 `push_view_models` / `push_acp_state` | 终止事件（TurnDone/TurnInterrupted）必须离开 loading；`LlmRetrying` 转为 Warning SystemNote，展示 attempt/max/delay/安全错误分类；transport 死亡兜底复位 loading；契约 ARC-EVENT-001 |
| 改输入/滚动/选择 | `src/kit/input_area.rs` + `message_area/scroll.rs` + `focus_router.rs` | `InputArea`（input_area.rs:148）；`scroll::handle_event`（scroll.rs:516，滚轮节流/拖拽选中/键盘滚动）；`focus_router::active_layer`（:105）、`classify_global_shortcut`（:117）、`message_accepts_key`（:147）、`input_accepts_key`（:190） | 消息区只处理滚轮、编辑区处理键盘（按焦点层分发）；弹窗/面板遮挡时鼠标清理残留（scroll.rs:548 `is_occluded`）；同优先级按注册序分发（keepgoing 须先于 scroll） |
| 改命令面板（slash/@mention） | `src/kit/slash_completion.rs` + `input_area.rs` + `submit_request.rs` | `SlashCompletion`（slash_completion.rs:118）、`filter_slash_items`（:99）；词法 `detect_slash_token`（input_area.rs:1465）、`apply_slash_selection`（:1278）；本地命令解析 `parse_submit_request`（submit_request.rs:49，/clear /rewind /export 等）；UI 命令上送 `AcpTuiClient::register_ui_commands`（client.rs:623） | 词法在 TUI、路由裁决在服务端 CommandRegistry（command-system.md）；补全模糊仅发生在搜索层，提交须完整全名；`/rewind` 等经 ACP 协议请求，不本地执行 |
| 改配置/启动流程 | `src/main.rs` + `src/launch.rs` + `src/config/` + `src/app/mod.rs` | `main`（main.rs:409）：env 注入（:260）→ `build_runtime`（:398）→ `run_tui`（:624）；`build_app_and_acp`（launch.rs:41）；`App::new`（app/mod.rs:47）；`TuiConfig::from_extra`（config/tui_config.rs:48，TUI 本地扩展，事实源在本 crate）；`save_effective`（config/mod.rs:21，写回路径决策一次性确定） | `PeriConfig` 等类型事实源在 `peri-acp/src/provider/config.rs`，`config/mod.rs` 仅 re-export；配置源句柄 `CONFIG_SOURCE_HANDLE` 启动时 set 一次，加载与保存共用同一决策；`teardown_app`（launch.rs:198）收尾 MCP 池/Langfuse |
| 改 TUI MCP panel 生命周期 | `src/app/mod.rs` + `src/app/service_registry.rs` + `src/kit/acp_events/system.rs` + `src/launch.rs` | `spawn_mcp_init`；`ServiceRegistry::mcp_task_owner`；`handle_oauth_completed/restored`；`shutdown_mcp_pool` | panel 部署容器保留 non-Clone `McpTaskOwner`，init 和 OAuth-event reconnect 经 weak spawner 准入；teardown 按 pool begin-close → owner join → pool close，并检查 service transaction report，Incomplete 不得记为已关闭（ARC-HOST-SHUTDOWN-001） |
| 改 service snapshot / thread 列表刷新 | `src/kit/service_snapshot.rs` | `spawn_service_snapshot`；`tick_once`；`SlowSnapshotRefresh` | thread 列表经 `ThreadStore::list_thread_entries(cwd)` 获取轻量投影，存储层完成 cwd/hidden/空 thread 过滤；不得退回会计算 message content size 的完整 `list_threads` |
| 改 ACP 请求发送 / reverse interaction 生命周期 | `src/acp_client/client.rs` + `interaction_lifecycle.rs` + `interaction_settlement.rs` + `interaction_response.rs` | `AcpTuiClient::spawn_pump` / `respond_interaction` / `publish_if_owned`；`InteractionLifecycle::{register_reverse,claim,begin_transition,open_prompt}`；`PromptLease` / `TransitionLease` / claimed batch lease | 单一 owner registry 管 Permission/Elicitation 的接受、claim 与 terminal；operation gate 线性化 UI publication、response、cancel 和 session transition；Drop settlement 使用 weak transport/notifier；headless print 仍按 token claim，且不依赖 kit atoms |
| 改 session new/load/delete 的 TUI 投影 | `src/acp_client/client.rs` + `src/kit/session_boundary.rs` | `new_session` / `load_session` / `delete_session`；`project_session_boundary` | client 在 route Stable commit 前同步投影 ACTIVE_SESSION_ID，并统一清 interaction atom/popup/panel/confirm、loading/input/rewind/todo/history；new transition 用容量 64 的 exact-target FIFO 覆盖 response→commit 窗口 |
| 改消息累积模型 | `src/kit/acp_types.rs` + `acp_events/streaming.rs` + `acp_events/render.rs` | `CurrentTurn`（acp_types.rs:42）：`start_tool`（:302）/`end_tool`（:339）/`start_subagent`（:359）/`stop_subagent`（:427）/`deactivate`（:511）/`mark_committed`（:517）；`handle_text_chunk`（streaming.rs:26）；提交态分组 `group_successful_tools`（render.rs:269）、折叠 `apply_fold_pass`（render.rs:414） | 流事件原地更新 current_turn、TurnDone 归档到 committed；`AcpEventData` 未知变体兜底 `Unknown` 前向兼容；replay 事件直接写 committed（turn.rs:292） |

## 关键控件/组件（src/kit/）

| 组件 | 文件 | 职责 |
| --- | --- | --- |
| MessageArea（消息流 + footer + keepgoing 按钮） | `message_area/mod.rs`（MessageArea :627） | 消息流渲染主组件；滚动/点击/选区事件注册；footer 行与 keepgoing 按钮命中 |
| footer/spinner 行 | `message_area/footer.rs` | `build_footer_lines`（:100）：loading spinner / summary / todo 行 + `KeepGoingLayout`（:85）；防抖期按钮禁用样式 |
| GridSpec 网格 | `message_area/grid.rs` | 断点（`Breakpoint` :17）与行首/续行前缀宽度；全部行渲染的对齐基准 |
| scroll 滚动引擎 | `message_area/scroll.rs` | `handle_event`（:516）；滚轮节流、拖拽选中、键盘滚动、吸底跟随（`should_follow_after_user_scroll` :378） |
| 语义选区 | `message_area/selection.rs` | 拖拽选区与语义复制（`map_slice_to_semantic` :469，复制时剥视觉前缀） |
| markdown 渲染 | `markdown/`（convert.rs / code_block.rs / table.rs / scan.rs） | 文本 → 带样式的行渲染；代码块、表格、扫描 |
| subagent 工具行 | `message_area/render.rs` | `render_subagent_group_lines`（:1664）、`subagent_tool_line`（:1724，固定 2 格缩进 `SUBAGENT_TOOL_INDENT` :34、label 无 bold）、`subagent_error_reason_line`（:1800，错误不弱化） |
| InputArea（输入区） | `input_area.rs` | 编辑、@mention、slash 补全、提交分发（`dispatch_submit_request` :1165）；多行渲染按显示宽度 |
| input_history（输入历史） | `input_history.rs` | `push_history`（:23）/`history_up`（:54）；持久化 `~/.peri/input-history.json`（唯一存储，`load_history` :119） |
| StatusBar（状态栏） | `status_bar.rs` | `StatusBar`（:322）：Row1/Row2/NotifRow、模型点击区（:400）、权限模式显示 |
| BgTaskArea（后台任务栏） | `bg_task_area.rs` | `BgTaskArea`（:41）：bg agent 运行中条目 + 动画 |
| Welcome（空态欢迎屏） | `welcome.rs` | `Welcome`（:94）：logo + 会话空态引导 |
| AppShell / SessionColumn | `app_shell.rs` + `layout.rs` | `AppShell`（app_shell.rs:23）顶层外壳；`SessionColumn`（layout.rs:100）+ `layout_plan`（:75）垂直布局 |
| SlashCompletion / MentionPopup | `slash_completion.rs` + `mention_popup.rs` | slash 命令补全弹窗（fuzzy 过滤，仅搜索层）；文件 @mention 弹窗 |
| PanelOverlay / PopupOverlay | `panel_overlay.rs` + `popup_overlay.rs` | 面板层（`PanelOverlay` :34）与居中弹窗层（`PopupOverlay` :38，`open_popup` :99） |
| 面板目录 PanelRegistry | `panel_registry.rs` | 面板种类→渲染函数注册表（`render` :438、`open_panel` :475、快捷键 `from_shortcut` :448） |
| tool 展示 | `tool_display.rs` + `tool_semantics.rs` | `format_tool_name`（tool_display.rs:8，本地化动词）；skill/todo 语义展示（tool_semantics.rs:65/:79）、todo diff（:115） |

## 子系统

### ACP 事件链（src/kit/acp_notifier.rs / acp_bridge.rs / acp_events/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 通知解码任务 | kit/acp_notifier.rs | `spawn_kit_notifier_with_client`：AcpNotification → AcpEventData 推 bridge_tx；reverse 投递失败按 owner BridgeReject 结算；transport 死亡兜底复位 loading + 断连提示 |
| 状态桥 | kit/acp_bridge.rs | `spawn_acp_bridge`：interaction 经 `AcpTuiClient::publish_if_owned` 后才同步写 UI；普通事件维护 `BridgeState` 并检测 BRIDGE_RESET_COUNTER |
| 事件分派 | kit/acp_events/mod.rs | `dispatch_and_notify`（:301）；`SessionPhase`（:149）/`BridgeState`（:158） |
| 流式/工具/边界/系统 handler | kit/acp_events/{streaming,tool,turn,system}.rs | `handle_text_chunk`（streaming.rs:26）、`handle_tool_started`（tool.rs:13）、`handle_turn_done`（turn.rs:12）、`handle_hitl_pending`（system.rs:117）等；subagent（subagent.rs:6/:33）、agent（agent.rs:8）、compact（compact.rs:11/:17） |
| 渲染管线 | kit/acp_events/render.rs | `push_view_models`（:25）/`push_acp_state`（:615）/`push_view_models_for_reset`（:595）/`handle_plan_update`（:672） |

### 状态与模型（src/kit/atoms.rs + acp_types.rs）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 全局 atoms | kit/atoms.rs | `ACP_STATE`（:220）/`VIEW_MODELS`（:249）/`SUBMIT_TX`（:256）/`CANCEL_TX`（:257）；`init_atoms`（:653） |
| 消息累积模型 | kit/acp_types.rs | `CurrentTurn`（:42）、`ToolCardAccumulator`（:996）、`SubAgentAccumulator`（:1054）、`AcpEventData`（:1170） |

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
| 弹窗（HITL/AskUser/OAuth/Confirm/Rewind/下载进度） | kit/popups/ + kit/popup_overlay.rs + kit/event_handlers.rs | `open_popup`/`close_popup`/`is_popup_active`；Rewind Enter 由根级 Global 模态仲裁发送既有 `REWIND_ACTION_TX`，鼠标与渲染留在 popup |

### App/配置/启动（src/app/ src/config/ src/acp_client/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 应用状态 | src/app/mod.rs | `App`（:29）/`App::new`（:47）；`spawn_mcp_init`（:127）、`get_compact_config`（:182）；子模块 agent.rs/cron_state.rs/provider.rs/service_registry.rs/setup_wizard/ |
| 配置 | src/config/ | `PeriConfig` 等 re-export 自 `peri-acp/src/provider/config.rs`（事实源）；`TuiConfig`（tui_config.rs:9，本地扩展，`from_extra` :48 / `sync_to_extra` :80）；`save_effective`（mod.rs:21） |
| ACP 客户端 | src/acp_client/client.rs | `AcpTuiClient`（:189）；`spawn_pump`（:292）fan-out notification；startup `ensure_session`（:719）与 reserved restore 在 operation gate 内裁决；reverse `respond_interaction`（:1075）/`publish_if_owned`（:1102） |
| 启动/CLI | src/main.rs、launch.rs、cli_args.rs、cli_plugin.rs、update.rs | `main`（main.rs:409）/`run_tui`（:624）；`build_app_and_acp`（launch.rs:41）/`teardown_app`（:198）；`run_kit_fullscreen`（kit/entry.rs:52）；插件/更新 CLI 子命令 |

### 设备同步与线程存储（src/sync/ src/thread/ src/components/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 设备间同步 CLI | src/sync/ | re-export：`run_sync_receiver`/`run_sync_sender`/`run_receive_cli`/`run_send_cli`/`run_device_command`（mod.rs；main.rs:523-530 调用）；子模块 protocol/noise_session/crypto/packer 等 |
| 线程存储 | src/thread/mod.rs | 仅 re-export `ThreadStore`/`ThreadMeta`/`SqliteThreadStore`（事实源 peri-acp-types / 实现在 peri-resources） |
| 通用组件 | src/components/textarea/、spinner/ | 文本编辑 widget（widget.rs/state.rs/word.rs/history.rs）；动画 spinner（animation.rs/verb.rs） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-BOUNDARY-001：TUI 交互主路径经 ACP transport；不得从 TUI 直驱 Agent/Middleware 运行时
- ARC-EVENT-001：事件链路单事实源 Agent →(ACP 映射) → TUI；新增事件须覆盖发射、映射与消费；终止事件必须使客户端离开 loading
- ARC-KEEPGOING-001：空白 user prompt（`MessageContent::is_empty()` 判空）是「继续跑 loop」指令，唯一生产者是 TUI keepgoing 按钮；空历史 + 空白 prompt 时服务端短路且必须 push_done
- ARC-CANCEL-001：cancel 按 (session_id, turn_id, attempt_id) 三元组定位；TUI 只经 ACP 发送 cancel，幂等判定与终态归 Agent 层
- ARC-HITL-001：Permission 与 AskUser 独立能力；TUI reverse interaction 由 semantic owner registry、operation gate、prompt/transition leases 与 token-aware UI terminalization 共同 first-claim
- ARC-SECRET-001：真实密钥/token/连接串不得写入界面、日志、错误响应或测试 fixture
- ARC-HOST-SHUTDOWN-001：TUI MCP panel 保留 external task owner，OAuth-event reconnect 经 owner 准入并按固定顺序 teardown
