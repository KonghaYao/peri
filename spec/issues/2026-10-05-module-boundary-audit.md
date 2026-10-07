# 模块边界审计：当前实现与批准目标

状态：静态审计完成，待按分类确认治理范围；本报告不是新的架构权威，也不授权实施修改。

日期：2026-10-05。参考提交：`0056ab2c`，实际证据来自审查时工作树，包含未提交改动。
采用六组并行模块审查、一组独立反证复核及主审交叉核查。只新增本报告，不改实现。

汇总：21 项，包含 9 项现行违反、10 项批准目标缺口、1 项职责契约冲突、1 项待验证。
2026-10-05 用户撤销原 11 项；该项已删除，其余保留原编号，避免已有裁决与交叉引用错位。
审计期间其他工作持续修改同一工作树；本报告针对读取时证据，不将并行补丁归因于本次审计，也不宣称覆盖交付后的变化。

## 判断与阅读口径

- 权威顺序不是“代码覆盖文档”：按 [STD-INDEX-002](../../docs/standards/index.md) 区分现行违反、批准目标缺口及职责契约冲突。
- 模块边界依据 [总体架构](../../docs/design/architecture.md)、[跨模块契约](../../docs/standards/architecture-contracts.md)、专门设计及模块指引；具体授权与部署例外优先核实，不仅凭 Cargo 依赖或文件 I/O 字样定性。
- 保留[既有用户裁决](2026-09-30-filesystem-dependencies-inventory.md)：Store、日志、TUI 本地状态不机械定级；插件与缓存暂缓；本地 JS 是特例；Web/Artifact 留待 builtin 组合。[增量扫描](2026-10-04-filesystem-boundary-residual-scan.md)的已修项不重复登记。
- 以下各发现按模块排列，说明使用编号列表，从用户操作、预期、具体模块行为及影响展开；每项正文 200 可见字符，标点、空格和英文计入，列表序号、换行及 Markdown 标记不计。标题、分类、依据、证据和用户裁决另列。代码行号仅定位本次快照，不作为长期规则。
- “现行违反”表示源码与明确契约不符，不等于线上事故已复现；“批准目标缺口”不计作已落地契约违规；“待验证”仅是有源码支持的假设。

## 模块覆盖

覆盖根 `Cargo.toml` 的全部 25 个成员及下表外围模块。完成目录枚举、依赖与风险模式扫描，深入追踪关键入口及相关测试；**不是逐文件逐行人工穷尽，也不是全库通过证明**。

| 模块 | 审查重点与结果 |
| --- | --- |
| `peri-agent` | RCRA、会话、子 Agent、异步交付与事件边界，见 21、22 |
| `peri-acp` | Goal、显式关闭、Cron 桥及协议/执行职责，见 01–03 |
| `peri-acp-types` | 共享契约、消息/队列/任务能力；可靠消息缺口须与 Agent、Store 联合治理 |
| `peri-controller` | 定位转发、身份与观测；取消接线属于已注明的过渡态，不新增确定违规 |
| `peri-runtime` | 登记、事件补打、销毁转发；未新增确认越界，跨销毁身份由外部 owner 提供 |
| `peri-middlewares` | 子 Agent 写入、辅助模型、MCP 取消任务，见 04–06 |
| `peri-mcp-core` | Agent 定义解析、任务 scope 契约；未新增确认越界 |
| `mcp-packages/common` | 工具适配、输出留存、Shell；共享文件视图见 09 |
| `mcp-packages/config` | bootstrap、来源 I/O、字节 CAS；未新增确认越界 |
| `mcp-packages/credentials` | provider 注入、凭据通道、错误脱敏；未新增确认越界 |
| `mcp-packages/web` | Fetch/Search 与完整输出落点，见 09 |
| `mcp-packages/artifact` | 路径与上传读取，见 09 |
| `mcp-packages/cron` | 任务目录、订阅、触发及宿主端口；桥接关闭见 03 |
| `mcp-packages/workspace` | 文件工具、Bash owner、发现、scope 关闭，见 04、07、08 |
| `peri-resources` | 本地/远端 Store、准入、关闭权及提醒持久化，见 10 |
| `peri-process` | 进程组、broker、退出证据；原疑问已按用户裁决撤销，未作 Windows 实机验收 |
| `peri-model` | 协议、provider、HTTP/SSE、重试与观测；未见引入 Agent 业务；使用关系见 05 |
| `peri-time` | 时钟、日历、等待后端及业务独立性；未新增确认越界，不宣称 WASM 完整验收 |
| `peri-js-runtime` | 通用进程/RPC owner；未见解释 Workflow 或 Agent 业务，保留本地 JS 特例 |
| `peri-wasm` | 原始帧桥、配置注入、Turso 与宿主关闭装配；虚拟 cwd 初始化不判工作区内容访问 |
| `langfuse-client` | 准入、批次、重试和 worker 关闭；未新增确认越界，Chrono 契约不机械判违规 |
| `peri-config` | source/resolver/snapshot/revision/CAS；consumer 缺口见 12–15 |
| `peri-tui` | ACP 主路径、配置面板、部署关闭；见 12–15，未见直接驱动 Agent loop |
| `peri-theme` | 主题来源、继承、解析、投影；本地文件属已批准客户端范围 |
| `peri-workflow` | 执行契约、恢复 journal、终态发布，见 18–20 |
| `npm-packages/@peri-sdk` | 新建 cwd、claim、关闭及重启接管，见 16、17 |
| `npm-packages/@peri-workflow` | RPC 委托、journal 恢复及读取，见 18、19 |
| `peri-cool` | 文档站入口与内容结构，未套用生产执行边界 |
| `e2e`、`e2e/tui-tester` | 测试 runner、fixture、tmux/runtime adapter，不把验证依赖判为生产越界 |
| `example` | minimal MCP 演示入口，不把独立示例当生产 ACP 客户端 |

独立 `side-projects/`、第三方 engine 内部、构建产物和供应商依赖不在核心生产模块审计范围；本报告不对它们出具合规结论。

## 逐项发现

### 01. ACP / Goal 业务状态机归层存在契约冲突

分类：职责契约冲突；归层方向已于 2026-10-05 经用户裁决，待实施；不计作确定违规。

依据：[总体架构](../../docs/design/architecture.md) §0、§2、§7；`peri-acp/CLAUDE.md` Scope；[Goal 清理任务](2026-10-01-remove-unused-session-cache-and-legacy-goals.md)。
证据：`peri-acp/src/session/goal_state/mod.rs:126`，`GoalState::set_status_with_reason`。

说明：

1. 用户给 Peri 一个目标：“修复登录问题，在预算内完成验证。”用户随后查看任务的进度。
2. 遇到阻塞时，用户补充信息；任务结束后，用户核对结果，确认目标是否真的完成。
3. 用户无论用终端还是其他客户端，看到的完成、阻塞和预算状态，都应遵循同一套规则。
4. 规则在 `peri-acp` 服务的 `GoalState` 中；文档中归属说法不一。
5. 风险是以后新增客户端时出现不同判定。尚未发现实际差异；应先统一职责，保留目标功能。

**用户裁决（2026-10-05）**

1. Goal 能力单独放在 `peri-middlewares` 实现，目标状态、完成与阻塞判定、预算规则及提醒时机归 Goal Middleware。
2. 新增通用“提醒器”结构，下沉到 `peri-agent`；Goal Middleware 使用提醒器交付提醒，在其上实现目标引导能力。
3. 提醒器基于 `peri-agent` 底层 RCRA 的 MQ 机制实现，封装提醒的构造与提交；消息接纳、消费及唤醒语义沿用会话 MQ 与 Receive 的统一规则，不另建消息队列、独立唤醒通道或激活调度，也不直接启动循环。
4. 提醒器不承载 Goal 业务规则；Goal Middleware 决定何时提醒、提醒什么，提醒器负责经 RCRA MQ 提交，由会话执行机制统一处理。
5. `peri-acp` 保留协议适配，不再实现 Goal 状态机；当前 `GoalState` 的目标业务随此方向迁入 Middleware。

实施状态：方向已确认，迁移尚未实施。本次只记录裁决，不修改生产代码；落地时同步相关架构、模块指引及契约测试。

### 02. ACP / 显式关闭先移除会话，再等待资源收尾

分类：现行违反；静态确认取消窗口，未运行故障复现。

关联：与 03 同属“会话资源关闭”问题。02 关注会话记录过早移除，关闭被打断后可能失去继续清理和重试的入口。

治理归组：与 13 一并纳入 P0「程序退出语义权威契约」治理；保留本项证据，不另造一套关闭规则。

依据：`ARC-HOST-SHUTDOWN-001`；[总体架构](../../docs/design/architecture.md) §9 销毁顺序。
证据：`peri-acp/src/session/mod.rs:286`，`SessionManager::close_session`；`peri-acp/src/session/mod.rs:302`，`take_for_close`。

说明：

1. 用户关闭会话时，要收尾的包括运行中的 Agent、后台命令和会话动态 MCP 实例。
2. 这些资源的句柄保存在 `peri-acp` 的 `AcpSession` 会话记录中。
3. 关闭由 `SessionManager` 会话管理器处理；“负责人”不是指人。
4. 当前记录先被移除，再等动态 MCP 等资源清理；等待被取消时，重试可能找不到原记录。
5. 应等待全部清理完成再移除，未完成则留存重试；尚未运行复现，不宣称资源残留已发生。

### 03. ACP / Cron 桥接任务没有参与关闭完成证明

分类：现行违反；未证明已发生任务泄漏。

关联：Cron 桥接任务就是 02 所说的会话资源之一。03 单列的是“未等待该任务实际退出”；仅修复 02 的会话记录保留，不能补上这份退出确认。

治理归组：与 13 一并纳入 P0「程序退出语义权威契约」治理；保留本项证据，不另造一套关闭规则。

依据：`ARC-HOST-SHUTDOWN-001`；[总体架构](../../docs/design/architecture.md) §9。
证据：`peri-acp/src/session/cron_bridge.rs:53`，`SessionCronBridge::shutdown`；`peri-acp/src/session/mod.rs:183`，`AcpSession::close_resources`。

说明：

1. 用户关闭带定时任务的会话，期待关闭完成意味着所有会话任务都已退出，不再后台运行。
2. `peri-acp` 当前只向 Cron 桥接任务发出取消和中止请求，没有等待它实际退出。
3. 关闭检查任务管理器和 Agent，却遗漏桥接任务，无法证明全部收尾完成。
4. 尚未证明任务泄漏；问题是桥接任务的负责人遗漏收尾，不意味着协议桥必须迁到其他模块。
5. 应把桥接任务纳入统一关闭，等待退出并记录结果；超时保留任务及上下文，报告未完成。

### 04. Middleware / 子 Agent 沙箱写入绕过 Workspace 通道

分类：现行违反；关闭范围例外不等于宿主文件视图例外。

级别：P1（2026-10-05 用户裁决）。

依据：[总体架构](../../docs/design/architecture.md) §4；[MCP 适配设计](../../docs/design/mcp-adaptation-v4-part-1.md) Middleware 归属；[增量扫描](2026-10-04-filesystem-boundary-residual-scan.md)判断口径；`docs/meta-harness.md` 的 WriteSandbox 关闭例外。
证据：`peri-middlewares/src/subagent/tool/build_agent.rs:79`；`mcp-packages/workspace/src/filesystem/write_sandbox.rs:50`，`with_draft`。

说明：

1. 用户允许子 Agent 只写指定目录，期待操作 Workspace 文件。
2. `peri-middlewares` 直接创建沙箱写工具，在宿主解析路径、建目录和写文件。
3. 未走 Workspace 通道，远端目录可能落到宿主；代码在 MCP 包不代表经过通道。
4. 沙箱工具的工作区关闭范围例外仍有效，但不允许另用宿主文件视图；未声称已写错。
5. 应保留目录白名单，把建目录和写文件交给绑定的 Workspace，消除旁路。

### 05. Middleware / 辅助推理消费了未声明的 Model 依赖边

分类：现行违反；仅定性为声明边不符，不推断主循环或安全机制失效。

裁决状态：辅助推理统一收敛到 `fork query` 的方向已确认，迁移待实施。

依据：[总体架构](../../docs/design/architecture.md) §0“未声明边一律禁止”；独立复核未找到专门批准的该依赖例外。
证据：`peri-middlewares/Cargo.toml:13`；`peri-middlewares/src/permission/auto_classifier.rs:125`；`peri-middlewares/src/goal/tool.rs:80`。

说明：

1. 用户开启 Auto Mode，Peri 调用模型判断操作是否安全，决定放行或转人工审批。
2. 目前 `peri-middlewares` 调用模型；下次输入预测在 `peri-agent` 有单独调用路径。
3. 两者应共用底层查询能力，权限判断和输入建议仍由上层解释。
4. 裁决统一使用 `fork query`：底层负责查询执行，上层获取结果，不自建调用路径。
5. 迁移尚未实施；原证据还含 Goal 验收调用，其接入范围需另行核对。

**用户裁决（2026-10-05）**

1. Auto Mode 的决策模型查询与推断用户下一次输入，共用底层 `fork query` 实现。
2. 上层功能通过 `fork query` 获取查询结果，分别保留权限决策、输入建议的业务规则，不各自实现直接模型调用路径。

设计待明确：底层具体归层、上下文使用、取消与结果契约；本次仅记录方向，不宣称已有通用实现。输入预测的现行执行入口是 `peri-agent/src/session/exec/executor/prediction.rs` 的 `execute_prediction`；原证据中的 Goal 验收用途另行核对接入范围。

### 06. Middleware / 请求析构派生的取消任务绕过任务 owner

分类：待验证；确认存在未登记 spawn，关闭影响仍需复现。

治理归组：与 13 一并纳入 P0「程序退出语义权威契约」治理；保留本项证据，不另造一套关闭规则。

依据：`ARC-HOST-SHUTDOWN-001` 的准入、owner 和 drain 契约。
证据：`peri-middlewares/src/workspace_io.rs:65`，`PendingWorkspaceRequest::drop`；`peri-middlewares/src/mcp/client/output_store.rs:19`，`PendingOutput::drop`。

说明：

1. 用户读取工作区文件时点取消，期待后台请求也结束，不再继续占用连接。
2. 析构就是请求结束时，程序自动清理请求对象；不是删除文件，也不是用户命令。
3. `peri-middlewares` 在自动清理时另开任务，向 MCP 发送“取消刚才请求”的通知。
4. 通知任务没有交给 `McpTaskOwner` 记录和等待，关闭连接时也可能没有等它收尾。
5. 当前只确认漏登记，未复现泄漏；应让关闭流程一并等待这些通知，失败时保留重试入口。

### 07. Workspace / 创建任务未携带预期生命周期 epoch

分类：批准目标缺口；不是 Store 执行 fencing。

领域定位：会话生命周期准入与重开隔离。这里的重开指重新允许工具请求进入，不是恢复或重启已有进程；旧请求不能借重开的会话启动新任务。

依据：[Session 异步任务](../../docs/design/session-async-tasks.md) §5；`ARC-RCRA-MESSAGE-001`。
证据：`mcp-packages/workspace/src/workspace.rs:176`，`task_scope`；`mcp-packages/workspace/src/shell_tasks.rs:237`，`admit`；`mcp-packages/workspace/src/shell_tasks.rs:566`，`open_scope`。

说明：

1. 用户发送后台命令，但网络延迟，请求还没到 Workspace；随后关闭并重新开放会话执行。
2. 旧请求此时才到达，Workspace 只看会话编号和当前可运行，就可能把旧命令当新任务启动。
3. 这里不是恢复已经运行的进程，而是防止上一次开放期间的迟到请求跨到新一次执行。
4. 创建请求需携带所属代次，让 Workspace 核对它是否仍可接纳。
5. 它属于会话生命周期准入问题，与恢复相关；批准目标未落地，未声称发生误执行。

### 08. Workspace / 任务发现缺少原始调用身份关联

分类：批准目标缺口。

领域定位：会话复原中的任务发现与调用对账。与 07 同属会话生命周期大领域，但 07 防旧请求迟到，08 找回已发出的调用及任务结果；两者不是同一种进程恢复能力。

依据：[Session 异步任务](../../docs/design/session-async-tasks.md) §1、§4。
证据：`mcp-packages/workspace/src/workspace.rs:202`，`call_owned_bash`；`mcp-packages/workspace/src/shell_tasks.rs:285`，`spawn_scoped_owned`。

说明：

1. 用户启动后台命令后连接断开，没收到任务编号；重连时希望找回原任务，而不是再执行。
2. Workspace 能列出任务，却没保存哪次请求创建哪项任务，同样命令执行多次时无法对账。
3. 这是会话复原中的任务发现与对账问题，不是恢复死掉的进程，也不同于 07 的迟到请求。
4. 应在创建时记录稳定调用身份，并关联任务编号，重连后按该身份查询结果。
5. 属于目标缺口；无法确认命令是否执行时要保留未知，不能把发现任务等同于安全重试。

### 09. Web / Artifact / 完整输出与上传没有统一文件视图

分类：批准目标缺口；工具共享文件视图的架构设计空白，完整方案待设计。

级别：P1（2026-10-05 用户裁决）。

依据：[增量扫描](2026-10-04-filesystem-boundary-residual-scan.md)“2026-10-04 用户裁决”F15/F16。
证据：`mcp-packages/web/src/web_fetch.rs:71`，`truncate_content`；`mcp-packages/common/src/shell.rs:365`；`mcp-packages/artifact/src/tool.rs:41`，`resolve_path`。

说明：

1. 用户用 WebFetch 抓完整网页，再用 Artifact 上传，期待两个工具读取同一份工作区文件。
2. 目前两者分别使用自己的临时目录或工作目录，分机部署时可能读不到同一文件。
3. 用户将此项定为 P1，认定这是工具文件共享方式的架构设计空白。
4. 当前补偿是所有 MCP 部署同一台机器，暂时减少分机路径不一致的问题。
5. 同机仍要确保目录和文件实际共享；长期共享文件视图方案待设计，不能把同机部署当作存算分离已完成。

**用户裁决（2026-10-05）**

1. 本项定级为 P1，性质是架构设计空白。
2. 当前补偿方案：所有 MCP 实例启动在同一台机器上。

部署约束：同机仍须保证相关工具访问同一份目录和文件，容器隔离、挂载或权限差异不能被同机假设掩盖。既有 builtin 组合方向保留，长期共享文件视图及跨机契约另行设计；同机补偿不等于目标架构已实现。

### 10. Resources / 历史提醒去重尚不等于可靠 Inbox 接纳

分类：批准目标缺口；当前 append 幂等能力不因此判错。

术语澄清：标题保留原审计术语；Inbox 不指独立模块或新输入通道。这里讨论 RCRA MQ 输入在重启后仍可找回的接纳与处理记录，当前易失 MQ 不被倒推为已有持久保证。

依据：`ARC-RCRA-MESSAGE-001`；[RCRA 消息权威](../../docs/design/rcra-message-activation.md) §5.1；[Session 异步任务](../../docs/design/session-async-tasks.md) §3.2。
证据：`peri-resources/src/sessions/resources.rs:517`，`append_reminder_if_absent`；`peri-resources/src/sessions/sqlite_store/session_data/async_task.rs:6`；`peri-resources/src/sessions/remote/session_history.rs:102`。

说明：

1. 用户等待后台任务提醒时，期待结果不仅存进历史，还能进入 RCRA 的 MQ，被模型处理。
2. Inbox 只是输入接纳记录的叫法，不是另一套队列。
3. `peri-resources` 按提醒身份去重并保存正文，尚未同时保存交给谁、是否待处理的状态。
4. 历史里有结果，不代表 RCRA MQ 重启后还能找回该输入。
5. 应围绕 RCRA MQ 保存接纳与处理责任，Store 负责持久化，Receive 负责消费，不另建调度。

### 12. TUI / 保存失败后仍发布模型配置并请求 ACP 更新

分类：现行违反；现有测试固化的是冲突行为，不能覆盖设计要求。

依据：[配置权威](../../docs/design/configuration-authority.md)“更新与保存”“Consumer 生命周期”；`STD-INDEX-002`。
证据：`peri-tui/src/kit/panels/model/edit.rs:203`，`commit_snapshot`；`peri-tui/src/kit/panels/model/commit_test.rs:315`。

说明：

1. 用户在模型面板保存配置时，期待保存失败后仍使用上一个有效状态，原持久化文件也保持不变。
2. `peri-tui` 当前先改共享配置，保存失败仍更新界面，并向 ACP 请求更新。
3. 用户确认这是要修复的问题：失败不能把任何编辑草稿变成已生效配置，也不能破坏原文件。
4. 保存应先完成验证与安全提交，成功后再更新内存、界面和执行配置。
5. 失败时保留旧状态与原文件，显示错误且不发布候选；现有测试若认可失败后更新，也需随裁决修改。

**用户裁决（2026-10-05）**

1. 保存失败必须继续使用前一个有效状态，不更新内存、界面或执行配置，不向 ACP 发布未保存的候选。
2. 保存失败不得修改原始持久化文件；写入失败的回归验证须同时检查旧状态和原文件内容不变。
3. 本次确认修复要求，未修改实现，不宣称现有保存路径已满足该保证。

### 13. TUI / 退出路径丢弃未完成的部署关闭上下文

分类：现行违反；代码未谎报 Complete，但不能证明排空。

级别：P0（2026-10-05 用户裁决）；统一权威契约工作，不仅是单点 TUI 清理补丁。

依据：`ARC-HOST-SHUTDOWN-001`；[总体架构](../../docs/design/architecture.md) §9。
证据：`peri-tui/src/launch.rs:244`，`teardown_app`；`peri-tui/src/kit/entry.rs:52`，`run_kit_fullscreen`。

说明：

1. 用户退出应用时，期待清楚知道任务是否结束、资源是否关闭，以及失败后还能怎样处理。
2. `peri-tui` 当前关闭失败仅记警告，随后释放应用，丢失继续等待和重试的上下文。
3. 已有宿主关闭契约，但正常退出、断连、取消和异常退出的完整语义仍需统一明确。
4. 用户将这些问题统一归为权威文档缺口，定为 P0，而不是分散修补各处退出逻辑。
5. 先明确退出场景的责任、顺序、失败结果及恢复入口，再按契约修复，不能把释放对象当清理完成。

**用户裁决（2026-10-05）**

1. 各处程序退出语义问题统一归为「权威文档需明确程序退出语义」，定为 P0；02、03、06 作为关联证据一并治理。
2. 现有 `ARC-HOST-SHUTDOWN-001` 已规定部分关闭责任；应在该权威入口补齐整体场景，而非声称已有契约不存在，或新增平行规则。
3. 需明确正常退出、显式关闭、取消、EOF/断连与异常崩溃的区别，列出任务和资源责任、关闭顺序、共享资源边界、失败/超时结果及重试或恢复入口。
4. 本报告只记录治理范围；具体退出策略与验收行为待统一设计，不将裁决记录冒充已完成的权威契约或实现。

### 14. TUI / 独立面板 MCP 池未绑定宿主配置快照

分类：批准目标缺口；用户已裁决收紧客户端边界，相关权威规则与实现待同步，旧 C 类过渡豁免不能作为目标方案。

依据：[配置权威](../../docs/design/configuration-authority.md)“Consumer 生命周期”；[配置 active issue](2026-10-01-configuration-authority.md) consumer followup；`scripts/import-exemptions.conf`。
证据：`peri-tui/src/app/mod.rs:137`，`App::spawn_mcp_init`；`peri-tui/src/kit/service_snapshot/session_services.rs:38`。

说明：

1. 用户查看工具面板时，期待看到 ACP 服务提供的 MCP 状态，而不是终端自己连接出的另一套状态。
2. `peri-tui` 当前自行构建 MCP 池，无活动会话时会回退使用本地池。
3. 用户裁决：TUI 不可构建 MCP，所有 MCP 相关状态只能从 ACP 传递过来。
4. 因此应删除独立池和本地回退，而不是给本地池补一份配置快照继续运行。
5. 无会话或断连时展示未连接状态；过渡豁免需随实施移除，不能作为保留独立池的理由。

**用户裁决（2026-10-05）**

1. TUI 不可构建 MCP；所有 MCP 相关状态只允许从 ACP 传递到 TUI。
2. 删除 TUI 独立 MCP 池、直接初始化/连接路径和无会话本地回退；撤销此前「独立池注入同一配置快照」的建议。
3. MCP 生命周期由服务侧负责，TUI 只投影 ACP 状态；未连接、无会话或服务不可用时明确展示该状态，不自行补建 MCP。
4. 实施时同步现行契约、模块指引和相应依赖豁免；本次仅记录边界裁决，不改实现。

### 15. TUI / 保存时才读取版本，未保护编辑起始基线

分类：批准目标缺口；按配置 followup 跟踪。

级别：P1（2026-10-05 用户裁决），明确要求修复。

依据：[配置权威](../../docs/design/configuration-authority.md)“更新与保存”；[配置 active issue](2026-10-01-configuration-authority.md)未完成 followup。
证据：`peri-tui/src/config/mod.rs:21`，`save_effective`；`peri-tui/src/kit/panels/model/edit.rs:203`。

说明：

1. 用户编辑配置后保存时，期待识别编辑期间他人已提交的更新，避免旧草稿覆盖新配置。
2. `peri-tui` 收到候选才读取版本提交，并丢弃接纳快照。
3. 这能发现采集期间的文件变化，却不能识别编辑开始以来草稿已过期，旧草稿可能拿新版本覆盖更新。
4. 此项是配置 followup 中未完成的批准目标，不计作新增违规。
5. 应打开编辑时保留基线版本，随草稿经 ACP 传递，并用接纳快照发布；现场读取新版本不等于已保护并发编辑冲突。

**用户裁决（2026-10-05）**

1. 修复编辑起始基线丢失的问题，定为 P1。
2. 编辑开始即保存基线版本，随草稿提交并经 ACP 传递；过期草稿必须拒绝覆盖，成功后使用接纳快照更新状态。

### 16. SDK / Agent 新建路径仍按宿主真实路径改写

分类：现行违反；这是已修 Sandbox 入口之外的残留链路。

依据：`ARC-REMOTE-ENV-001`；[增量扫描](2026-10-04-filesystem-boundary-residual-scan.md) F8。
证据：`npm-packages/@peri-sdk/src/agent/agent.ts:21`，`Agent.constructor`；`npm-packages/@peri-sdk/src/agent/session.ts:73`，`Session.startOnce`。

说明：

1. 用户选择远端工作区创建 Agent，再新建会话，期待会话目录始终指向所选工具环境。
2. `@peri-sdk` 的 Agent 构造器仍尝试解析本机真实路径，新会话和列表过滤会使用改写结果。
3. 远端路径不存在时会回退；若恰好对应本机符号链接，工具环境身份可能被混淆。
4. 按编号加载使用持久目录，不在此问题范围，不能称所有恢复均失败。
5. 应按环境统一新建入口的路径规则，远端路径保持原样，并验证创建到新建会话的完整行为流程。

### 17. SDK / 共享 claim 尚不能完成崩溃后的安全接管

分类：批准目标缺口；不得把执行租约或 owner CAS 加回 Rust Store。

依据：`ARC-WORKSPACE-001`；[Session 异步任务](../../docs/design/session-async-tasks.md) §5；`npm-packages/@peri-sdk/README.md` 当前限制。
证据：`npm-packages/@peri-sdk/src/kv/agent-claims.ts:12`，`AgentClaims`；`npm-packages/@peri-sdk/src/sandbox/process-supervisor.ts:15`。

说明：

1. 用户在主 Agent 崩溃后重启服务，期待原会话能被安全接管，且旧执行不再干扰。
2. `@peri-sdk` 已用共享领取记录防止重复占用，关闭时先清理再释放。
3. 但记录只保存所有者，进程代际留在内存，尚无重启发现、旧执行隔离和跨实例接管流程。
4. 占位可能无法释放；仅凭过期释放不能证明旧执行停止，这是批准目标缺口。
5. 应由 `@peri-sdk` 保存代际与退出证据，明确接管条件，不把协调责任下推给 Rust Store。

### 18. Workflow / 两端重复裁决 journal 的可恢复成功前缀

分类：现行违反——业务规则重复；未证实行为分歧或数据损坏。

级别：P1（2026-10-05 用户裁决）。

治理归组：Workflow 整体重构；本项作为重构输入和验收关注点，不单独建立临时恢复或交付规则。

依据：根 `CLAUDE.md` 核心原则 7；[Workflow 设计](../../docs/design/workflow.md) §3.8（Rust 传连续成功前缀、Node 按调用 key 匹配）。
证据：`peri-workflow/src/runner/run_protocol.rs:97`，`reusable_journal_prefix`；`npm-packages/@peri-workflow/src/server.ts:95`，`journalStore.read`。

说明：

1. 用户恢复中断的工作流，期待已连续成功的步骤可复用，失败步骤及后续重新执行。
2. `peri-workflow` 先筛出连续成功日志，`@peri-workflow` 又按相同条件排序截断。
3. 两端重复决定复用范围，当前规则一致且操作可重复，未证实恢复错误或数据损坏。
4. 风险是以后调整失败边界时两端判定漂移，不能把风险写成已经发生的故障。
5. 应保留唯一规则所有者，另一端校验并拒绝异常，保留调用键匹配与身份转换，不另加适配层。

**用户裁决（2026-10-05）**

1. Workflow 整个能力将大幅重构，所有 Workflow 疑问点统一定为 P1；本报告涵盖 18、19、20，涉及 Rust 与 TS 两端。
2. 保留现有疑问与证据供重构核对，不把各项局部建议直接当作已批准的独立实施方案。
3. 编号说明：用户本轮以「16」指代 Workflow；原报告 16 是 SDK 路径问题，故不改其归属或自动赋予 P1，也不重排编号。

### 19. Workflow / 成果恢复仍要求原宿主文件视图

分类：批准目标缺口；本地 JS 特例继续有效。

级别：P1（2026-10-05 用户裁决）。

治理归组：Workflow 整体重构；本项作为重构输入和验收关注点，不单独建立临时恢复或交付规则。

依据：[Session 异步任务](../../docs/design/session-async-tasks.md) §4；[文件系统清单](2026-09-30-filesystem-dependencies-inventory.md) §3.5。
证据：`peri-workflow/src/journal.rs:85`，`WorkflowJournalStore::new`；`peri-workflow/src/runner.rs:170`；`npm-packages/@peri-workflow/src/reader.ts:22`。

说明：

1. 用户换机器凭运行编号查看或恢复任务，期待脚本、日志和成果可定位。
2. `peri-workflow` 在工作目录存取成果，`@peri-workflow` 从本机向上查找运行目录。
3. 无共享卷时运行编号不能定位宿主成果，但本地 JS 特例仍有效，不凭文件操作判违规。
4. 这是批准目标缺口，读到旧日志不等于远端执行恢复，不等于计算已被接管。
5. 应将成果定位与保留交给独立资源能力，迁移脚本、Git 证据和恢复规则，只换目录不足。

### 20. Workflow / 终态广播未建立可靠交付责任

分类：批准目标缺口；与 Agent 接纳和 Store 处理义务一并治理。

级别：P1（2026-10-05 用户裁决）。

治理归组：Workflow 整体重构；本项作为重构输入和验收关注点，不单独建立临时恢复或交付规则。

依据：`ARC-RCRA-MESSAGE-001`；[Session 异步任务](../../docs/design/session-async-tasks.md) §3.1；[RCRA active issue](2026-10-05-rcra-message-activation.md)。
证据：`peri-workflow/src/tool/completion.rs:158`，`RunCompletion::spawn`；`peri-workflow/src/registry.rs:98`，`WorkflowTaskRegistry::complete`。

说明：

1. 用户等待后台工作流结束，期待结果不仅留存，还能在重启后送达并被 Agent 处理。
2. `peri-workflow` 依靠内存广播通知结束，无订阅者时只记录警告，日志保留成果。
3. 路径未保存终态身份、收件人和待接纳责任，消费者消失后不能证明可靠交付。
4. 这是批准目标缺口，成果存在也不能证明模型处理完成，不将易失广播称为持久保证。
5. 应接入可恢复发件记录与接纳确认，记录执行、清理、接纳和处理，复用会话规则，不另建调度。

### 21. Agent / 领域生产点仍构造 v1 事件并执行协议映射

分类：现行违反；涉及含用户改动的完整快照，不归因于该补丁。

级别：P1（2026-10-05 用户裁决），明确要求清理。

依据：`ARC-EVENT-001`；[总体架构](../../docs/design/architecture.md) §2、§7、§9；`ARC-BOUNDARY-001` 的执行归位不授权协议归位。
证据：`peri-agent/src/agent/stages/receive.rs:157`；`peri-agent/src/session/retry_events.rs:21`；`peri-agent/src/session/exec/executor_helpers/v2_execute.rs:586`；`peri-agent/src/agent/subagent_event_forwarder.rs:139`。

说明：

1. 用户查看提醒、模型重试和后台完成消息，期待各客户端得到一致消息含义。
2. `peri-agent` 仍构造旧版事件，部分包入新版载荷，执行助手参与协议映射和旧事件发布。
3. 执行归入 Agent 合法，不代表出口协议也可归入业务，映射应留在 `peri-acp`。
4. 属现行结构违规，尚未测试错序或丢失；证据含用户改动快照，不归因于该补丁。
5. 应建立新版提醒、重试和终态载荷，移除旧事件包裹通道，由 `peri-acp` 兼容映射。

**用户裁决（2026-10-05）**

1. 确认本项问题成立，定为 P1，需要清理。
2. 清理 `peri-agent` 中的 v1 事件构造、旧事件包裹与协议映射路径；Agent 发出领域事件，ACP 负责出口协议适配。
3. 必要的 wire 兼容仅保留在协议边界，不在 Agent 内新增兼容层；本次只记录裁决，尚未实施清理。

### 22. Agent / 持久终态提交与处理责任入队不是同一接纳事务

分类：批准目标缺口；用户裁决归入会话恢复设计，目前缺失该场景的完整恢复设计与实现。

治理归组：会话恢复设计；与 10 的存储能力缺口、20 的 Workflow 生产缺口关联，统一明确待处理输入的恢复责任，不拆成三套独立方案。本次未指定级别。

依据：[RCRA 消息权威](../../docs/design/rcra-message-activation.md) §4.3、§5；[Session 异步任务](../../docs/design/session-async-tasks.md) §3；现行[消息存储](../../docs/design/message-transcript.md)的易失 Queue 不被倒推为持久保证。
证据：`peri-agent/src/agent/async_tasks/delivery.rs:63`，`SessionTerminalDelivery::deliver`；`peri-agent/src/agent/async_tasks/registry.rs:168`；`peri-agent/src/agent/async_tasks/settlement.rs:105`。

说明：

1. 用户等后台任务结束，期待重开会话后结果能被处理，而不只在历史中看到正文。
2. `peri-agent` 先保存提醒正文，再放入 RCRA MQ；待重试结果也只留在内存。
3. 两步之间崩溃，正文可能还在，会话恢复时不知道哪些结果尚未被处理。
4. 归入会话恢复设计缺口：恢复后如何找回待处理输入、继续处理并避免重复，目前仍缺失。
5. 应在会话恢复设计中明确持久化记录与 RCRA MQ 的恢复关系，关联 10、20，不另建输入队列。

**用户裁决（2026-10-05）**

1. 本项属于会话恢复设计中的问题，目前该场景的恢复设计缺失，需纳入统一设计。
2. 需明确重开会话后如何找回已保存但未处理的输入、恢复 RCRA MQ 的待处理责任，以及避免重复处理。
3. 现有 RCRA 与异步任务设计已提供目标要求，但不代表本场景的恢复闭环已明确或落地；持久化历史不等于处理责任可恢复。
4. 本次仅记录设计缺口，不新增另一套队列，不提前认定具体接纳事务或恢复扫描方案已获批准。

## 后续与验收边界

优先推进 13 的 P0 程序退出语义权威契约，将关联关闭问题统一治理。按已记录裁决安排 P1 修复与 Workflow 整体重构；其余批准目标缺口归入配置、RCRA、Session 异步任务及 builtin 组合任务，不另造平行业务权威。01 按已裁决职责实施；剩余待验证项先补定向复现。

本次未运行 Rust/TS 构建或行为测试，未完成异机、真实 Turso、跨进程崩溃、Windows 或托管 WASM 验收。报告交付仅验证新增文档格式、内部引用和正文长度；不将旧报告或旧测试结果冒充本次运行证据。
