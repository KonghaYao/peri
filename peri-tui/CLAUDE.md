# peri-tui

## Scope

`peri-tui` 是基于 ratatui-kit 的终端客户端。用户交互主路径经 ACP transport；crate 仍直接依赖 `peri-middlewares` 和 `peri-resources` 做宿主装配与部分面板数据访问，并依赖 ACP、主题等 crate 的协议和展示类型。TUI 不直接依赖 `peri-agent`，也不得直接驱动 agent loop；Agent 执行入口保持在 ACP 会话执行路径。

## 数据流/架构

```text
ACP notification → acp_notifier → acp_bridge / BridgeState
                 → VIEW_MODELS + atoms → components
```

用户提交、取消、会话加载及交互响应经 ACP client/transport 发送；通知在 `kit/acp_notifier.rs` 解码并进入 bridge，`kit/acp_bridge.rs` 维护 `BridgeState` 并发布渲染状态。组件只订阅和渲染状态，不能在 render 中驱动 Agent。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 入口、任务启动、ACP client 生命周期 | `src/kit/entry.rs`、`src/acp_client/` |
| ACP notification 解码与状态发布 | `src/kit/acp_notifier.rs`、`src/kit/acp_bridge.rs`、`src/kit/acp_events/` |
| 全局状态与 ViewModel | `src/kit/atoms.rs`、`src/kit/acp_types.rs` |
| 输入、提交、历史、@mention、slash | `src/kit/input_area.rs`、`src/kit/input_history.rs`、`src/kit/submit_consumer.rs` |
| 消息渲染、滚动、选择 | `src/kit/message_area/`、`src/kit/markdown/`、`src/kit/text_selection.rs` |
| 增量布局与派生缓存预算 | `src/kit/message_area/transcript.rs`、`transcript_index.rs`、`src/kit/entry_render_cache.rs` |
| 键盘、鼠标、焦点与事件优先级 | `src/kit/event_handlers.rs`、`src/kit/focus_router.rs` |
| 面板、弹窗与确认交互 | `src/kit/panels/`、`src/kit/popups/`、`src/kit/panel_overlay.rs`；面板操作的作用域与完成门在 `src/application/operations/` |
| 配置落盘与保存提示（Config / Model / Login / Theme） | `src/kit/config_service.rs`（即时 `persist_candidate`；长草稿 `persist_draft` + expected revision/accepted；显式 `reload_configuration`）+ 各面板动作入口 |
| ChatGPT 登录/状态/注销能力边界 | `src/chatgpt_auth/{mod,controller,dispatch,actions}.rs`（typed backend、共享凭据目录、`AuthController`、后台派发）+ `src/kit/panels/login/{auth_policy,auth_ui,auth_render}.rs`（认证就绪推导、宿主作用域与认证子视图）+ `src/kit/setup_wizard/handler.rs` 与 `src/kit/app_shell.rs`（`/setup` 订阅入口与首次启动 Login → Model 接力）；不读 token、不启动 CLI 子进程 |
| 国际化与主题 | `src/i18n/`、`locales/`、`peri-theme` atoms |
| 测试 | 与目标模块同目录的 `*_test.rs` 或 `#[cfg(test)]` 模块 |

输入历史持久化由 `src/kit/input_history.rs` 管理，路径为 `~/.peri/input-history.json`；不要另建平行存储。

## 稳定不变量

- ACP 是交互与 Agent 执行的边界；新增请求、通知或终止事件须覆盖 ACP 映射、bridge 和组件消费，终止事件必须离开 loading 状态。
- `BridgeState` 是 ACP 事件到 `VIEW_MODELS` 与 atoms 的状态边界。切换会话或重置时，必须过滤陈旧 session 事件并清理旧会话状态。
- 会话列表经 ACP 的 `peri.sessionWorkspaceV1` scope 查询：默认 Project、可切 Workspace / All；分页未结束时数量标明“已加载/还有更多”，向下键、PageDown 或 End 接近已加载末尾时追加下一页，重复按键合并同页请求。未绑定旧历史继续列出，`v` 通过只读 history RPC 预览且不切换执行会话；`-c` 精确选择启动工作区内当前相对目录，`-r` / 普通恢复先查询保存 binding 或旧会话保存的 cwd，再由 load 接纳旧根。查询或恢复失败不得通过 `ensure_session` 无声新建或把排队输入发给旧会话。会话执行唯一性与接管由 `peri-sdk` 负责；TUI 不投影 ownership 只读准入、不请求取回执行权或接管 proof、不维护 `SESSION_READ_ONLY`。执行环境不可用时 load 明确失败，history 预览仍可读。宿主在响应 `session/load` 前回放历史，每次可能回放的 load 前均须 `project_session_boundary`，防止消息区重复；客户端 prompt request lease、load reservation 与 operation gate 只协调请求和交互生命周期，不代表会话执行所有权。
- `ACTIVE_EXECUTION_CWD` 仅在 session 初始化提交后发布，驱动路径展示、文件补全和本地导出；启动 cwd 独立保留给新会话。Hooks / Plugin / MCP 面板按 active session ID 查询实际环境，不能持续回写启动快照。
- History 面板使用单行会话列表与固定详情/操作栏；按容器高度计算视口，列表和只读预览各持有独立滚动状态。刷新按 thread ID 保留选择，执行操作使用已选身份，删除确认固定待删 ID，不能用旧索引查新列表决定目标。
- 面板列表的可见范围、滚动起点与鼠标命中由 `kit/panels/shared/list.rs` 的同一次推导给出（`ListMetrics` + `ListViewport`），不写死可见项常量；面板高度以 `panel_overlay::panel_height` 为唯一权威。详情与操作绑定业务稳定身份（id/name/path）而非列表下标；目标消失由 `use_effect` 在更新阶段收敛，渲染只读状态。分段列表、可变行高与无列表面板可不接入该机制，但须在面板内写明理由与替代做法。契约见 [TUI 面板架构](../docs/design/tui-panel-architecture.md)。
- 面板操作的作用域与完成门由 `application/operations/` 统一表达（`OperationScope` / `OperationTicket<S>` / `CompletionGate<S>` / `OperationOutcome`）：发起取票、完成校验代次与作用域、取消按结果未知表达；面板不自建票据或代次，同作用域同目标的在途重复触发判定为拒绝（不发、不提示）。宿主配置认证使用 `HostConfigAuthScope`（配置源 + 冻结 store/account），不伪造会话 id；中间事件用 `is_current`，注销取消用 `cancel_wait` 保留本地清理结果。确认执行只按确认时固定的稳定身份，目标消失则不执行并给可见提示，不回退为按下标取当前项。事件判定集中在 `handle_<panel>_event(event, ctx)`，组件只做注册。
- Config / Model / Login / Theme 的持久配置仍编辑宿主启动时选中的 `ConfigSource`，面板明确标识“宿主配置”和实际保存路径；权限切换（配置行、Shift+Tab、slash）及会话模型选择等运行请求继续按 session ID 路由。整份配置上送不带 session ID；切换会话不重定位宿主配置写入。同配置源会话刷新 provider 连接并失效模型缓存，保留各自的模型/profile 选择和 frozen 数据。四个面板的落盘与可见提示只有一份实现（`src/kit/config_service.rs`）：候选先落盘，成功才提交内存句柄、投影与 ACP，失败只提示不改可见状态——不可逆动作（删除 provider）同样适用。Login 的长草稿必须捕获进入时的 revision 并使用 `persist_draft`，只发布 accepted snapshot；Model 的每行编辑在提交时基于当前配置即时落盘；冲突不覆盖外部新值。打开 Login/Model 及冲突后的 `Ctrl+R` 经 `reload_configuration` 显式重读 authority；重载后再次进入编辑以最新 revision 为基线，不自动 rebase 或重试保存；只有真正的 revision/authority 冲突显示重新载入提示。保存入口在未清除的普通 `inline_error` / `input_blocked` 存在时拒绝提交（认证类陈旧错误由 Login 保存门先清除）。Model 的 `Ctrl+L` 经一次性 `LOGIN_PANEL_REQUEST` 定向打开 Login，未保存草稿先提示保存或取消；动态表单与内联 editor 的鼠标命中与同一帧语义行同源。
- Login 的认证就绪由 `login/auth_policy.rs` 单一推导，渲染、动作可用性与保存门共用：只对当前作用域（配置源 + store/account）、Plan 已授予且草稿身份为空/兼容的状态自动采用；已有不同身份必须显式“使用此账号”，不得自动覆盖。认证状态变化必须清除 `IdentityRequired`/`AuthReference` 类陈旧错误；采用后动作索引复位到“查询状态”，不得滑到注销。`Checking` 保存只提示等待、不写红字；access token 过期只描述本地记录与自动刷新，不阻断保存。Ctrl+R 与 Model→Login 必须同步认证作用域，不得展示或采用旧 store/account 的“已登录”。新建 Responses + ChatGPT plan Provider 的四档默认模型只从已保存的同类 plan Provider（其次其他 Responses Provider）预填空字段，手动输入优先；无来源时提示，绝不伪造模型 ID；预填只写 Login 草稿，不写 profile。
- render body 不写 atom；render 内派生缓存使用既有无通知写入模式，副作用放在事件或 effect 边界。
- 同一个 `State` 的读守卫不得跨过对该状态的写：在条件或 `match` 里「先 `read()` 再在同一表达式内 `write()`」会自锁挂死（内部 RwLock 同线程不可重入），表现为测试永久挂起而非 panic。先把值取到局部变量（`.clone()` / 拷贝）再写，测试探针与事件处理同样适用。
- cancel 关闭 reverse interaction owner，不等于执行终态已确认；匹配当前 execution 的 done 仍须透传一次以结束 loading，重复 done 与旧 execution 终态不得影响新 turn。
- 消息区按 generation 验证 publication，复用不可变布局；冷缓存保持选择、复制与高度。主消息/详情共用 entry 缓存；预算不含 canonical 历史、轻量索引或整体 RSS。
- Cron/MCP 经 ACP 消费；快照/last-good 按 session generation 与前端重置代次隔离，失败可见；无会话明确标注启动插件/Hooks。scheduler/pool 及关闭权属宿主。服务快照的投影写入保持单点：唯一发布入口是 `kit/service_snapshot/commit.rs` 的 `commit_projection`（短锁内逐字段 compare-and-set，失败即整体放弃），会话字段用窄入口 `publish_session_fields`；`SERVICE_SNAPSHOT` 写锁与失效门只允许出现在 `commit.rs`，刷新单元只产出 patch。
- Assistant VM 的完整气泡以 `Arc<TuiAssistantBubble>` 发布，快照与 im 节点 COW 共享不可变 payload；修改 fold/终态前先判定是否真的变化，再复制或 `Arc::make_mut`，不能改动旧快照。后台流式累积仍持有独占正文，不能把共享发布气泡直接当逐 chunk 写入缓冲。
- Markdown 稳定分片仅冻结安全空行边界，fence 内部空行不能切断代码块；引用、列表、表格与未闭合 fence 保留可变尾部，terminal full parse 校正。正文前景与代码背景须同时进入内外层缓存 key，主题变化只复用 parsed blocks，不复用旧样式。
- `#[component]` 的 hooks 必须在所有条件分支、`match` 与提前返回前按稳定顺序调用。
- 消息区、输入区、状态栏与后台任务栏的绘制区域由 `kit/layout.rs` 的 `CenterBand` 包装组件在 **draw 期**收进居中带（§3.1）；透明布局保持父级按原子组件约束分配。带内的换行宽度、命中列与光标列都以带内相对坐标为准，位置 tracker 看到的区域即 band——不要再注册改 `drawer.area` 的 hook（ratatui-kit 0.12 会在 `pre_component_draw` 后重置）。滚动条是窗口级 chrome（锚在终端最右列）：`ScrollbarHook` 每帧接收终端宽度与网格，在 `pre_component_draw` 由 band 显式还原整幅外框（`term_width == 0` 时回退 band），渲染与命中测试共用该矩形。
- 交互事件按 focus owner、语义命中区域、z-order 与 pointer capture 分发；弹窗/面板前景事件和遮罩必须先于背景处理，避免 click-through。
- 用户可见文本使用 i18n；新增 key 同步更新 `locales/en/main.ftl` 和 `locales/zh-CN/main.ftl`。主题从 `peri-theme` atoms 获取，不硬编码颜色。
- 文本编辑、截断与坐标按 Unicode 字符边界和终端显示宽度处理；不得用字节长度替代显示宽度。
- MCP 详情的重启/关闭经 `peri/mcp/control`；initialize 服务端明确回显 `peri.mcpControlV1=true` 且条目 `control` 允许才显示动作。操作按 session/reset/server 完成门互斥，取消等待和超时表示结果未知、不自动重发；完成后经 `service_snapshot/mcp_refresh.rs` 查询并由唯一快照提交门发布，迟到周期采样不能覆盖主动刷新。
- TUI 不建 MCP 池、不持 `McpTaskOwner`（遗留 panel pool 已删除，勿重引入）：池生命周期、OAuth 注入与关闭顺序（pool begin-close → owner abort/join → pool close）全归 ACP Host 部署持有；退出只经 `teardown_app` 显式关闭 ACP deployment，Incomplete 不得记录为已关闭（ARC-HOST-SHUTDOWN-001）。

## 目标命令

从仓库根目录执行：

```bash
cargo run -p peri-tui
./dev.sh
cargo build -p peri-tui
cargo check -p peri-tui
cargo test -p peri-tui --lib
```

## 按需引用 / Verify

- 稳定 UI 规则：`../docs/standards/tui.md`。
- TUI 专属环境变量：`../docs/standards/tui-environment-variables.md`；其他运行变量见 `../docs/standards/environment-variables.md`。
- 跨模块边界、事件与冻结数据：`../docs/standards/architecture-contracts.md`，重点遵守 `ARC-BOUNDARY-001` 与 `ARC-EVENT-001`。
- 修改 ACP 数据流时，核对 `src/kit/acp_notifier.rs`、`src/kit/acp_bridge.rs` 和对应组件；修改用户界面文本时核对两份 FTL。
- 完成后运行相关 `cargo test -p peri-tui --lib`，并运行 `git diff --check`。不得把密钥、token、密码或连接串写入界面、日志、错误或测试 fixture。
