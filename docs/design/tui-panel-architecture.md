# TUI 面板架构目标设计

> 状态：已批准目标设计（部分已落地）；迁移进度、批次与验收见
> [2026-10-15 TUI 面板架构重构](../../spec/issues/2026-10-15-tui-architecture-refactor.md)
> 范围：`peri-tui` 的面板外壳、列表视口、面板事件路由、面板操作的作用域与完成门
> 边界：不改变 ACP 协议、后端能力、依赖与 release profile；新增生产代码只落在 `peri-tui`

当前实现、入口与动态面板清单以 [`peri-tui` 代码索引](../code-index/peri-tui.md)、
源码与契约测试为准。

## 目标与原则

面板是消息流之上的独立工作区：打开后覆盖消息区底部，不遮挡输入区，键鼠语义等价，
状态不依赖另一个面板的存活。用户在任何时刻都应能判断：当前面板操作哪个对象、该对象
是否还在列表中、动作是否已发出、结果是否已知。

- **一个面板一套有效路径**：同一面板不保留被替代的旧实现；迁移完成即删除旧逻辑。
- **绘制与命中共源**：可见范围与鼠标命中来自同一次推导，终端高度变化后二者一致。
- **身份不随刷新漂移**：确认、入队与详情绑定业务稳定身份（id/name/path），不绑定列表下标。
- **渲染只读**：渲染只读状态；需要收敛的状态写入放在更新阶段（`use_effect`）或事件处理里。
- **hooks 无条件**：`hooks.use_*` 与事件注册都在任何条件返回之前调用。
- **动作有完成门**：非幂等操作一次只允许一个在途请求，晚到结果只被本次操作接受。
- **状态不只靠颜色**：符号、状态词与样式共同表达状态。
- **面板不绕过 ACP**：面板只消费投影、发起既有 ACP 操作，不直连后端、不新建第二套全局状态。

非目标：面板可视化重设计、面板内自建数据源、跨面板共享可变状态、为每个面板机械切分
固定数量的文件。

## 结构

### 面板外壳

`PanelOverlay` 按 `ACTIVE_PANEL` 渲染当前面板，`panel_shell!` 提供边框、标题与滚动容器。
面板高度是终端高度与主题上下限的唯一权威（`panel_height`/`panel_content_rows`），面板
组件在首帧尚未测量自身区域时按同一权威预测内容行数。

### 共享列表视口

`panels::shared::list` 是固定行高列表的可见范围、滚动跟随与命中布局的唯一推导处：
行度量（header/item/footer）与内容行数进入，`visible_items`、`scroll_start`、绘制窗口与
命中布局一起产出。列表面板不再各自持有写死的可见项常量；绘制取窗口、事件取命中布局，
两者必须来自同一实例。选择越界在选择推导处收敛，刷新缩短列表不残留越界选择。

接入范围：固定行高的列表面板（Hooks、MCP、Cron、Memory、Config、Model）共用本机制；
分段列表（Tasks）、可变行高（Agent 逐行折行测量）与无列表的只读面板（Status、Goal）按
「公共组件边界」不接入，在面板内写明理由与替代做法——复用既有测量结果，而不是把行高
压成常量。

视口接管后滚轮不再滚出被裁剪的行：可见范围即窗口，窗口跟随选中项；滚轮只在既有
`panel_scroll` 仲裁内保留注册，各面板行为一致。

### 面板事件入口

每个面板把事件判定集中在具名处理函数中：`handle_<panel>_event(event, ctx: &<Panel>EventCtx)`。
上下文结构体聚合本帧视口、面板句柄与列表快照，组件只做注册。测试经挂载探针取得真实
句柄后直接投喂事件，判定逻辑不必复制到测试里。

弹窗占用前景（OAuth、HITL、确认等）时，面板对全部事件让路：弹窗与面板同属一个输入层，
层内只按注册序投递，面板抢先消费会让弹窗收不到按键。让路判定只依据前景归属，不依据
事件类型。

鼠标只加速已有命令：列表区域内左键点击等价于“选中该行并执行该行的 Enter 动作”，
滚动条所在列不参与列表命中。

### 公共组件边界

公共组件只承载已有面板中真实重复的绘制与命中逻辑，不预设面板清单：

- 公共列表只表达交互意图（导航、激活、返回），“激活”具体是关闭详情、取消任务还是终止
  运行由各功能控制器保留。
- 业务身份由功能层提供；公共组件不得用列表下标或显示标签替代业务 key。
- 不建设 JSON 驱动的页面解释器，也不建立覆盖所有面板的巨型 action 枚举。
- 面板行度量差异明显时（分段列表、可变行高、无列表的只读面板）允许不接入公共组件，
  在面板内说明理由，不为了统一而扭曲语义。

### 详情视图

详情视图（列表之外的子视图）绑定业务稳定身份而不是进入时的下标：列表刷新改变顺序后
详情仍指向同一对象。详情目标消失时由更新阶段的 effect 收敛回列表，渲染过程不写状态。
详情视图的行构造、按钮布局与命中几何是纯推导，渲染与命中共用同一结果。

共享外壳只提取真实重复：目前由 `panel_shell!` 提供框线与底栏，`panel_height` /
`panel_content_rows`（`kit/panel_overlay.rs`）与 `panels/shared/list.rs` 的视口提供高度、
滚动与命中，其他「详情外壳」不因形态相似而抽取——例如滚动条几何（`ScrollbarGeometry`）
目前唯一使用者是 SubAgent 详情，保持面板私有，不预先抽象成通用组件。

### 操作作用域与完成门

面板操作用三元组表达作用域：会话身份、面板代次与取消标记。发起操作时取票并固定目标
身份；完成时校验代次与目标一致才落地，否则丢弃；面板关闭或切换时把在途操作标记为
未确认，不显示为成功。非幂等操作在在途期间忽略重复触发，且不自动重发。

结果状态类型化：成功、失败（携带原因）与结果未知三态分开表达，结果未知不得呈现为取消
成功。操作反馈（成功、失败、结果未知）必须对用户可见。

原语已落在 `application/operations/`（`OperationScope` / `OperationTicket` /
`CompletionGate` / `OperationOutcome`）：保持纯状态机，不读 atom，作用域由调用方构造，
依赖方向不反向指向 `kit`。作用域是「活动会话 id + 前端重置代次（`BRIDGE_RESET_COUNTER`）」，
请求代次由门自己维护；面板关闭由门的 `Drop` 取消在途票，取消表达为 `OutcomeUnknown`。

已接入：插件搜索与插件操作、Cron 动作、Tasks 取消、Workflow 终止运行。同作用域同目标的
在途请求不再重复发送；晚到结果经 `accept` 拒收后只留日志，不改动可见状态（面板已关闭也
不产生提示——关闭是观察者状态，不是业务终态）。

尚未落地（按 active issue 记录）：搜索路径的取消仍是静默丢弃而不产生结果；已发送变更的任务
所有权与应用层结果跟踪（方案 §6.2）未接管，本层只做请求协调；客户端内部超时目前仍映射为
失败而非结果未知（区分它需要客户端返回类型化错误，字符串匹配不予采用）。

「确认的目标消失」是统一的守卫结果：确认时固定的身份在列表刷新后已不存在则不执行，
给出可见提示（`app-confirm-target-missing`），不回退为按下标取「现在这一项」。

### 配置落盘边界

四个配置编辑面板（Config / Model / Login / Theme）共用同一条写入规则，规则只有一份实现：
`kit/config_service.rs::persist_candidate`。

1. 调用方在候选副本上改（不动全局 handle），把候选交给 `persist_candidate`。
2. 该入口落盘并给出可见提示：成功写 `config-saved`（1s），失败写 `tracing::warn` 与
   `config-save-failed`（2s，带原因），返回 `false`。
3. 只有返回 `true` 时调用方才提交内存句柄、投影与 ACP 推送——失败时用户看到的仍是已
   持久化的值。不可逆动作同样适用（删除 provider 落盘失败则不删：不能显示一个重启后
   会复活的结果）。

被收敛的是「落盘 + 提示」这一真实重复的边界；候选的构造方式与成功后的发布动作（提交
哪些句柄、是否推送 ACP）属各面板语义，不并入公共实现。测试经 `persist_candidate_with`
注入落盘结果，不写真实磁盘。

### 快照与投影

面板消费的列表、计数与错误来自 `service_snapshot` 等投影协调器：它们按类别维护上次
成功值与本次错误，并用变更失效与节流减少查询。面板只订阅投影，不直接写回投影字段。

投影写入保持单点：唯一发布入口是 `service_snapshot/commit.rs` 的 `commit_projection`
（短锁内逐字段 compare-and-set，身份失效则整体放弃），会话级写入走窄入口
`publish_session_fields`；`SERVICE_SNAPSHOT` 写锁与失效门只允许出现在 `commit.rs`，
刷新单元只负责采样并产出 patch；拆分不得把提交或失效门分散进各刷新单元。

### 面板职责映射

每个面板的状态 owner、数据来源、动作入口与完成门（`file:line` 以代码为准）：

| 面板 | 状态 owner | 数据来源 | 动作入口 | 作用域 / 完成门 |
| --- | --- | --- | --- | --- |
| Hooks | `HOOK_LIST` | `plugin/list` 投影 | `handle_hooks_event` | 无 mutation（只读） |
| MCP | `MCP_SERVERS` + 面板内 `view`/`detail_key` | `mcp/list` 投影 | `handle_mcp_event` | 无 mutation；详情按 server 名，目标消失由 effect 收敛 |
| Cron | `CRON_JOBS` + 面板内 `selected`/`confirm_delete` | `cron/list` 投影 | `handle_cron_event` → `actions::dispatch` | `OperationScope` + `CompletionGate`；确认固定 `job.id` |
| Memory | `MEMORY_LIST` | 本地扫描 `memory_scan.rs` | `handle_memory_event` | 无 mutation（只读，编辑器经既有入口） |
| Tasks | `BG_TASKS`/`BG_TASK_REVISION` + 面板内 `CancelGates` | bridge `handle_bg_task_snapshot` + `CRON_JOBS` + `VIEW_MODELS` | `handle_tasks_event` → `dispatch_cancel` | `OperationScope` + `CompletionGate`；同目标在途重复触发为拒绝 |
| Agent | 面板内 `cursor`（只读） | `SERVICE_SNAPSHOT` + `VIEW_MODELS` | `handle_agent_event` | 无 mutation（只读） |
| Status | 面板内 `active_tab`（只读） | `SERVICE_SNAPSHOT` + `VIEW_MODELS` | `handle_status_event` | 无 mutation（只读） |
| Goal | `GOAL_SNAPSHOT` | bridge `handle_goal_snapshot` | 内联 handler | 无 mutation（只读） |
| Config | `PERI_CONFIG_HANDLE` | 宿主配置 handle | `handle_config_event` → `activate_row` | CAS 保存，失败回滚不投影 |
| Model | `PERI_CONFIG_HANDLE` | 宿主配置 handle | `handle_model_event` → `switch_active_alias` | CAS 保存；保存失败不写内存、不投影 |
| Login | `PROVIDER_LIST` | providers 投影 + 配置 handle | `handle_login_event` → `save_login_edit` | 候选副本落盘成功后才发布；按 provider id 定位 |
| Theme | 面板内 `selected_theme`/`original_theme` + 模块内目录缓存 | 本地目录扫描 + 下载 | `handle_theme_event` → `trigger_download_themes` | 下载任务唯一租约（`DownloadRun::try_claim`） |
| Plugin | `PLUGIN_LIST` + `DiscoverState`/`OperationState` | `plugin/list` 投影 + marketplace 只读缓存 | `handle_search_event` / `handle_panel_event` | `OperationScope` + `CompletionGate`；身份见 `plugin/identity.rs` |
| ThreadBrowser | `THREAD_LIST` + 浏览作用域 atoms | `thread_list_refresh` 分页 cursor | 内联 handler + `request_more_threads` | 归档/删除按 `ThreadListViewKey` 逐项复验 |
| Workflow | `WORKFLOW_SNAPSHOT`（独立轮询单元） | `workflow/list_runs`（2s 轮询） | `handle_workflow_event` → `dispatch_kill` | `OperationScope` + `CompletionGate`；发送前 `snapshot_contains_run` |
| AskUser | `ASK_USER_PENDING` + 面板内 `FormState` | bridge `handle_ask_user` | 内联 handler | owner 指纹重置；提交携固定 owner/request_id |
| SubAgentDetail | `SELECTED_SUBAGENT_ID` | `VIEW_MODELS` / `BG_LIVE_DETAIL` | 内联 handler | 无 mutation（只读） |

固定行高列表统一走 `panels/shared/list.rs` 视口；分段（Tasks）、可变行高（Agent）与无列表
（Status、Goal）面板按「公共组件边界」自持测量，理由写在各自文件内。

## 键位与优先级约定

- Esc 由面板内的子视图（详情、表单、确认）先消费，再落到面板关闭。
- 确认类状态是面板内的一次性门：进入确认后，列表刷新不改变待执行目标。
- 确认弹窗执行的是「渲染时看到的那次动作」：版本与渲染帧不一致时不执行、不关闭，
  并给出被替代提示（`popup-confirm-superseded`），等待用户对当前内容重新确认。
- 主题、语言与终端尺寸变化对面板是普通更新，不触发面板重建。

## 验证要求

行为验证覆盖入口、键鼠、不同终端尺寸、空/加载/失败、返回与主题语言；并发验证覆盖
确认期间列表刷新、切会话后旧响应、关闭重开后面板状态；性能与体积验证按 active issue
记录的口径执行，未测量的平台必须标注。
