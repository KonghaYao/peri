# v4 第二阶段：单机服务化评估报告

状态：阶段未完成；本文是现状评估与验收建议，不替代已批准设计或实施契约。

日期：2026-10-01。核对基线为 `c703082b` 及读取时工作树；根指引、两份相关审计及其他改动已有未提交内容，本文不把它们当成已发布验收。此次只读核对代码、标准、code-index 与 active spec，未运行测试、启动独立服务或做强杀实验。

## 结论与阶段边界

根 [CLAUDE.md](../../CLAUDE.md) 将「单机本地」标为已完成，「单机服务化」标为未完成。现有 TUI/stdio 共享 ACP host 与 Agent 执行路径，SQLite/远端会话数据后端、Session ID 恢复和 MCP 工具包提供了服务化基础。但当前 stdio 入口仍随客户端连接建立和关闭宿主；后台任务、Workflow 在途状态及部分工具恢复信息仍依赖进程内存或终态文件。**已有可服务化的核心，不等于已有可独立驻留、重启后安全接续的单机服务。**

本报告按「同一台机器上，客户端、计算进程、持久状态和工具环境可以分开管理生命周期」评估第二阶段。它不要求多实例同时接管、分布式锁、跨机器工作区、云端传输或 Serverless。是否采用常驻 daemon、Unix socket/Named Pipe、独立工具进程，以及客户端断连后继续运行还是取消，尚无已批准的具体契约；下文将这些列为待裁决的服务形态问题，不假定某种部署已经获批。

## 进度估算

**单机本地：100%（按已确认的阶段状态）；单机服务化：约 35%。** 服务化数字是基于下列五项等权的工程估算，不是测试通过率或已批准的里程碑计量。各项的事实与缺口见下一节。

| 单机服务化能力 | 估算完成度 | 判断依据 |
| --- | ---: | --- |
| 服务入口与连接生命周期 | 35% | 共用 ACP host 和传输抽象已存在；独立服务入口、断连与重连语义未落地 |
| 会话持久化与冷恢复 | 70% | 存储后端、历史和按 ID 恢复已有实现；跨层及进程重启验收未闭合 |
| Run 与工具安全恢复 | 20% | 有局部恢复机制；运行身份、逐项证据与副作用未知处理未形成系统保证 |
| 工具环境归属与关闭 | 50% | MCP 边界及部分关闭机制已实现；独立生命周期与关闭反例仍待处理 |
| 真实服务端到端验收 | 0% | 尚无独立服务的断连、强杀、重启、恢复纵向实测 |

等权平均为 `(35 + 70 + 20 + 50 + 0) / 5 = 35%`。如果后续阶段契约把「服务」限定为更窄或更宽的形态，应重定权重和验收项；不能把单机本地已完成的 100% 混入第二阶段分母。

## 能力现状

| 能力 | 已有基础 | 单机服务化差距 |
| --- | --- | --- |
| ACP 出口与执行核心 | `run_acp_server` 对 MPSC/TUI 和 stdio 共用请求处理、事件映射与 Agent 执行；`AcpTransport` 已抽象传输。[ACP 入口](../../peri-acp/src/host/mod.rs)、[transport](../../peri-acp/src/transport/mod.rs) | 生产实现目前是进程内 MPSC 与 stdio；尚无独立服务的连接、重连和客户端归属契约。stdio EOF 会进入 host 关闭流程并取消会话任务。[stdio 入口](../../peri-acp/src/host/stdio/mod.rs)、[关闭流程](../../peri-acp/src/host/shutdown.rs) |
| 会话与持久化 | `ThreadStore`、本机 SQLite 和远端数据 adapter 已存在；frozen snapshot、历史和按 Session ID 恢复已有实现。[Resources 索引](../../docs/code-index/peri-resources.md)、[迁移记录](2026-09-30-session-id-environment-core-change.md) | 历史可读不能证明未完成的 Run 可继续。Session ID/env 任务仍待热/冷恢复、父子树、关闭及真实远端等验收；本阶段至少需要本机进程重启实验。 |
| 计算与后台任务 | RCRA、Subagent、Workflow、后台 Bash 和取消/排空机制已有实现。[Agent 索引](../../docs/code-index/peri-agent.md)、[TaskManager](../../peri-agent/src/agent/async_tasks/manager.rs) | TaskManager 随 session 存活，registry 是易失状态；原执行身份、结果和恢复动作没有统一的跨进程闭环。[工具可恢复性扫描](2026-09-30-agent-tool-recoverability-p0.md) |
| 工具环境 | Workspace/Web/Artifact/Cron/LSP 等能力经 MCP 包与宿主桥接；Workspace 资源提供技能、Agent 定义和项目指令。[MCP packages 指引](../../mcp-packages/CLAUDE.md) | 包与协议边界已形成，但不能据此认定工具实例可独立于 ACP/Agent 宿主重建或驻留。Full Compact 仍直接读取计算宿主本机文件；MCP-over-ACP 关闭/撤销有已记录的隔离和排空反例。[边界审计](2026-09-30-architecture-boundary-compliance.md) |
| Workflow 恢复 | journal 可保存已完成调用，终态 `RunState` 保存参数并供 resume 使用。[Workflow 索引](../../docs/code-index/peri-workflow.md) | `init_run` 只写脚本，`list_runs` 只枚举已有 `state.json` 的目录；终态前崩溃的 run 可能不可发现，也缺完整启动输入。[journal](../../peri-workflow/src/journal.rs) |
| 配置与服务生命周期 | 配置权威、scoped snapshot 与来源 MCP 已实现；ACP host 有保留关闭上下文、重试 Incomplete 的机制。[配置评估](2026-10-01-configuration-authority.md)、[host 生命周期](../../peri-acp/src/host/lifecycle.rs) | 部分 consumer 接线、独立 TUI pool 和跨进程 CAS 仍待验收；服务入口的启动、关闭、重连及错误反馈尚未形成完整产品路径。 |

## 阻断项

1. **明确服务和连接的生命周期。** 需要决定服务是否常驻、客户端断连对活跃 Run 的影响、重连如何认领会话与事件、交互审批在无客户端时如何结算。当前 stdio EOF 的「关闭宿主并取消」是已实现行为，不能直接充当持久服务的断连语义。服务入口应继续复用 ACP host 与业务分发，避免形成第二套 Agent 路径。
2. **持久化运行事实，而不只保存对话。** 至少能在同机重启后按稳定身份区分已完成、未开始和结果未知的操作，找回输入、完成证据及下一步。当前工具批次先收集后提交 transcript，快工具已产生副作用但慢工具未完成时，进程退出可能留下证据空窗。细项见[工具可恢复性扫描](2026-09-30-agent-tool-recoverability-p0.md)。
3. **收口工具成果和后台任务。** Write 草稿只在工具实例内存；Edit 失败后的完整结果未保存；Subagent 陈旧 active、Workflow 未终态 run、后台 Bash 超时/取消后的原 ID 查询都有缺口。恢复不能把远端 MCP 超时当成确定未执行，也不能盲目重放有副作用的调用。上述 P0 扫描目前标记待审批，本文只引用其发现，不视作已批准的修复方案。
4. **厘清工具环境所有权。** 会话绑定的 Workspace、MCP/LSP、shell/Node 进程需要明确由哪个服务单元创建、查询、取消和关闭。当前 host 的关闭报告有局部可靠性机制，但 ACP-over-MCP 撤销隔离、取消后的重试与桥接任务排空已有反例；Full Compact 的宿主磁盘回读也违反资源来源边界。先在单机完成这些边界，不要求本阶段跨机器迁移工具环境。
5. **做服务级故障验收。** 当前多项记录是定向单测、静态分析或历史运行结果；尚缺一个真实独立进程的「启动 → 运行 → 客户端断连/服务强杀 → 重启 → 查询/恢复 → 关闭」纵向证据。失败后的恢复信息与 ACP 事件也需在此路径核对。

## 建议的交付顺序与验收

| 切片 | 可核对结果 |
| --- | --- |
| 1. 服务形态契约 | 明确单机服务入口、连接和会话/Run 的不同寿命；断连、取消、关闭、重连与无客户端交互有确定语义。先用现有 ACP host 实现一个可运行纵向切片。 |
| 2. 同机冷恢复 | 在真实进程退出后以原 Session ID 读取历史、frozen 数据和父子关系；执行不可用时仍可读，恢复准入不依赖旧文件锁或 dirty reset。 |
| 3. Run 与工具恢复 | 启动前保存恢复必要输入；逐项记录完成证据，保存可查成果；未知副作用显式要求核对。覆盖 Write/Edit、Subagent、Workflow、Bash 与远端 MCP 的代表性故障。 |
| 4. 资源与服务关闭 | 工具环境按会话/部署归属；取消和重复关闭不丢 owner；确认任务、MCP/LSP/Node/shell 与存储排空后再报告服务关闭成功。 |
| 5. 端到端验收 | 在同一台机器用真实独立进程测试正常运行、断连、强杀、重启、恢复及再次关闭；分别记录通过项、未验证项和平台限制。 |

上述顺序是评估建议，不预先选择 daemon、IPC、表结构或统一恢复框架。第二阶段完成的判据应是用户能通过 ACP 找到原会话与运行、核对已产生的成果，并安全决定继续或停止；只通过编译、历史重放或内存中的恢复测试不能关闭阶段。

## 证据局限与关联工作

- 本报告没有运行新测试，也没有验证单机服务部署；「未完成」是现有入口和 active spec 所支持的判断，不等同于所有路径都已复现故障。
- [Session ID/env 任务](2026-09-30-session-id-environment-core-change.md)已完成核心实现，仍留跨层与故障验收；不要把其剩余项误写成多实例要求。
- [工具可恢复性 P0](2026-09-30-agent-tool-recoverability-p0.md)是待审批扫描；本报告未改变其授权状态。
- [架构边界审计](2026-09-30-architecture-boundary-compliance.md)记录了 MCP 生命周期与取消链路反例；该文件目前在工作树中未跟踪，后续实施须按届时源码重新核对。
- [v4 云架构审计](2026-09-27-v4-cloud-architecture-audit.md)已明确总体方向，但没有形成第二阶段服务接口或部署方案；本报告也不把云端/多实例工作提前计入单机服务化验收。

## 2026-10-01 后续方向与首个切片

用户进一步明确：先做 TS SDK；单机服务的服务器部分由 TS 管理，Peri 承担 Agent 后端。根指引已同步职责：TS 管服务入口、进程与跨主 Agent 管理，Peri 管 ACP 后端、Agent 执行、Store 直连和 MCP Client。此裁决不把进程内 Subagent/Workflow 移交 TS 重写，也不让 TS 代理 Session 快照。

首个 Bun SDK 切片位于 [`npm-packages/@peri-sdk`](../../npm-packages/@peri-sdk/README.md)：通过现有 ACP stdio 启动 Peri、创建/加载 Session、持续接收事件与用户输入、发送取消通知、响应 ACP 反向请求；不存在回合级 `wait()`。共享 KV 的原子占位使同一 Session 只能由一个 Agent 实例持有，Sandbox 直接从 Session Store 列举历史会话。协议和真实 Peri 测试覆盖输入交付、事件流、进程退出、占位冲突与关闭后按原 Session ID 冷加载；类型检查与构建通过。测试未覆盖真实外部模型或工具调用；生产级服务认证、持久 Run 和强杀后的安全续跑仍未落地，因此本报告的 **35% 仍是服务化阶段的粗略基线**，不因 SDK 切片自动上调。

可运行的 [Bun HTTP demo](../../npm-packages/@peri-sdk/examples/demo/demo.ts) 现已补齐：POST `/api/session/list` 纯查询数据库，POST `/api/session/create` 创建或加载 Session 并持续返回 SSE 事件，POST `/api/session/send` 向指定 Session 投递输入。demo 使用本地 libSQL HTTP Store，按 Session ID 复用进程内 Agent，支持事件游标回放；隔离的真实 HTTP 冒烟验证了创建重试复用、发送、列表和事件回放。它仍不提供生产服务的认证、持久 Run 或强杀后的安全续跑；上文「独立服务入口未落地」指生产级入口，**35% 估算不因 demo 通过而修改**。
