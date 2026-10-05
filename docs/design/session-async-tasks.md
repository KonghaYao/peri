# Session 异步任务架构

> 状态：已批准目标设计，**待重构**（作为代码重构基线）。本文负责 Subagent、Workflow 与 MCP 后台任务的注册、观察、取消和恢复；消息可靠接纳、分级和激活统一服从 [RCRA 消息权威](rcra-message-activation.md)。现行实现入口见 `docs/code-index/`，不把目标描述为已经实现。
>
> Scope：每个一等公民 Agent 会话的异步任务控制面，以及独立驻留的任务执行面。RCRA 循环借用这些领域，不拥有跨 turn 状态。实施见 [消息与激活任务](../../spec/issues/2026-10-05-rcra-message-activation.md)。

## 1. 领域与所有权

**执行 owner** 持有实际执行、终态结果和取消能力。Workspace MCP 实例拥有后台 Bash，Workflow owner 拥有 Workflow run，子 Agent 的执行属于其自身会话 runtime 和 SDK 执行管理。任务只有一个执行 owner；TaskManager 不通过 UI 条目删除证明执行已停止。

**Session TaskManager** 是本会话直接发起任务的目录、操作入口与运行时投影。每个主/子/嵌套/Workflow Agent 都有独立的 Task 领域、Inbox、Transcript 和生命周期。父会话登记“委托子会话”的任务，不登记子会话内部的工具任务；共享 MCP 连接不共享调用身份或回调。父子树聚合是另一个只读视图，不是父会话的直接任务目录。

**持久绑定与 owner 事实分离**：调用意图、发起者、owner 路由、任务关联和投递责任必须可恢复；MCP task record 仍是外部执行状态和结果的权威。内存目录可以重建，不要求把外部 task 状态复制成另一份执行权威。Workflow journal、Subagent Transcript 各守自己的成果与执行恢复语义。

任务身份至少包含 `initiator_session_id + trusted owner identity + owner task_id`，对外 opaque task ID 由稳定复合身份派生。owner identity 必须重连后仍指向同一任务空间，不能用工具名、临时连接 ID 或模型传入字符串。调用绑定必须在创建任务前可靠登记，响应丢失时能按调用身份对账；不能确认创建结果时进入 OutcomeUnknown，不盲目重发非幂等调用。

## 2. 单一操作入口

发起者通过本会话 TaskManager 登记、查询、对账和取消任务；可信上下文决定归属，不由 callback 捕获的父身份决定。创建失败不能留下虚假 running；创建结果不确定不能当作失败重试。快速终态不得被迟到 started 回写。

Manager 根据稳定 task 身份定位 owner，转发查询、Workflow kill、委托关联执行的取消或 MCP `tasks/cancel`。取消委托按 delegation ID 和执行关联精确定位，不能等同于取消子会话当前工作；终态委托的迟到取消不得影响后续委托。取消请求只表示 owner 接纳，终态和实际清理证据分别确认。未知 task、错误 session、无权限、失联 owner 和超时使用明确结果，不伪装成功。

直接发起者按授权控制自己的任务。父/祖先只有具备显式级联授权时才能控制后代，遵守 Cascade/Independent 策略；观察权限不隐含取消权限。自然结束一次委托不等于关闭子会话，更不等于关闭执行树。

ACP/TUI 获取本会话直接任务快照，授权树级聚合另行标明来源会话、委托关联与 scope。快照和增量必须通过 revision/cursor 衔接；缺口重取快照，切换会话不复用旧投影。聚合不产生第二个业务终态或模型结果投递。

## 3. 完成结果与消息

### 3.1 结算责任

执行终态、资源清理、交付接纳和模型处理分别记录。任务执行完成不必等待模型处理，但需要通知的终态在取得 Durable Inbox Accepted 前必须保留可重试交付责任；不能仅因 active execution count 归零而丢结果。UI 可以显示“执行完成、交付待确认”，不得以显示成功替代交付证据。

拒绝、panic、ACK 丢失和进程重启均按稳定事件身份重试交付，不重跑任务。终态发布由 owner 的不可变 terminal transition ID 确定；状态描述、清理进度的 revision 变化不产生第二次终态。取消与结算按 owner 状态机裁决，清理结果不制造重复 completed。

### 3.2 交付与处理

终态事件进入直接发起会话的可靠 Inbox，由 Receive 幂等投影到该会话 Transcript；任务终态是否需要模型处理由类型化激活策略决定。Accepted、Projected、处理检查点分别确认，具体事务、去重、重试和激活只由 [RCRA 消息权威](rcra-message-activation.md) 定义。

不允许任务模块通过直接写 Transcript 加可选 queue push 形成第二条可靠交付协议；即使物理实现允许接纳时预写 canonical 内容，也必须原子保留投递记录和处理义务。旧 MessageQueue/InboxHandle 可作为内存投影，但不能独立确认可靠接纳。

外部 owner 保留原始结果，Agent 将其投影为有界摘要和安全产物引用。Workspace Bash 使用结果文件引用；可靠引用须满足保留和访问契约，不能以易失文件冒充可恢复内容。展示发送失败不丢业务结果，交付失败也不被展示成功掩盖。

## 4. 外部任务发现与恢复

MCP Tasks 按 ID 查询、取消和订阅不能找回响应丢失的未知 ID。可靠 owner 必须支持可信发起会话 scope/调用身份的发现，返回 task 身份、initiator、revision、terminal transition ID 以及恢复结果所需信息。调用者认证授权与 scope 隔离分别校验，scope 本身不是凭证。

每个会话可以独立发现自己的任务，不要求 root 活跃。执行树 scope 可用于授权批量扫描，但每个结果按不可变 initiator 归属重建，禁止 root fallback。未知归属隔离为 Unroutable；发现与响应登记冲突不能覆盖已知身份或回调。

owner 提供快照 cursor 与续接变更流；cursor 过期、lag 或空洞重新取快照。断连/查询失败表现为失联或待对账并有界重试，不能把本地任务直接判完成或无限留 running。owner 记录保留至责任可靠移交或明确过期处置；不具备发现/保留能力的外部 server 明确为 best-effort，不声称冷恢复。

单个 loop/session runtime 消失不自动取消任务。Builtin Workspace 与 Agent 同进程时，进程退出可能同时终止二者，该部署不提供进程级执行高可用。独立 MCP owner 存活时可重新发现；owner 自身退出后的恢复由其持久化与执行环境契约保证，Inbox 不能恢复已消失的外部进程。

子 Agent 与 Workflow 同样按稳定会话/运行身份恢复。已有 Transcript 或 journal 只是恢复证据，不证明旧执行仍活跃或任务已完成；SDK 决定执行唯一性，未知副作用按检查点和 owner 对账，不由 TaskManager 自动重放。

## 5. 关闭与清理

用户显式关闭会话时，领域准入进入 PreparingClose，阻止新任务，并可靠提交关闭意图。提交失败撤销准备态；提交不确定先回读确认，不能先返回成功。关闭意图不是执行 owner lease。

owner 的会话 scope closing gate 必须与任务创建线性化，创建与关闭请求均绑定生命周期 epoch，返回 barrier cursor；关闭前接纳的在途创建及尚不确定的调用意图纳入后续发现，旧创建不得穿过 Reopen。缺少发现/barrier 能力的 owner 不能证明强排空，只能按 RCRA 权威 §8.6 返回 OutcomeUnknown/Incomplete，不以本地目录为空当成功。逐项结算本会话任务，不关闭共享 MCP 实例、不误取消其他会话任务。关闭子会话时级联终止其持有的后台执行资源（如 bg shell），资源终止与结算后才向发起者返回消息。若请求显式要求级联，则对授权范围内的每个子会话分别执行关闭协议并尊重 Independent 契约；树级 empty 不替代逐会话关闭证据。

关闭排空可以接纳既有任务的终态和清理证据，但不启动模型。未履行处理义务必须显式放弃或交接并留记录；交接通知不等于原会话已处理。父会话关闭涉及 Independent 委托时，显式等待或持久 detach/abandon：脱离后不再阻塞父关闭，也不取消子执行；迟到结果保留在 owner/委托结算记录，按关闭收件人政策处置，不重新打开父会话。脱离不是任务正常完成。超时或失联返回 Incomplete，保留可重试上下文。删除记录必须等待任务/投递责任明确处置，并保留去重与关闭所需 tombstone。

关闭已完成的会话保持 Closed；重开必须是显式操作，scope epoch 防止迟到 close 请求影响新生命周期。自然结束、父会话不活跃、transport 断开、宿主退出都不是 Close。显式 Stop 的暂停与后续激活语义统一见消息权威。

会话唯一执行者、替换和跨实例接管由 `peri-sdk` 管理。SDK 在开放新执行者前停止或隔离旧实例；Peri 不保存执行租约、不认领 Store owner、不向工具传 Store fencing token。scope epoch 只保护任务生命周期，不承担执行所有权。

## 6. 边界与验收契约

所有 Agent 复用同一套任务登记、恢复和交付契约测试。至少覆盖：调用响应丢失、快速完成、发现先到、重复终态、持久接纳失败、owner 失联、父 runtime 消失、子会话独立恢复、树级聚合不夺取归属、取消权限和关闭创建 barrier。

可靠 Inbox 不提高外部 owner 的实际持久性；持久任务绑定不证明副作用已完成；任务终态不证明模型处理完成。消息激活与崩溃恢复矩阵见 RCRA 消息权威，实际执行结果只记 active issue。

## 7. 归属、投递与可见性

本节保留归属主题入口，统一规则如下：

| 关系 | 权威与用途 |
| --- | --- |
| 直接任务归属 | 发起会话自己的 Task 领域，跨主/子 Agent 完全同构 |
| 消息收件人 | 调用时可靠绑定的直接发起会话；恢复不得隐式改投 |
| 执行 owner | 实际执行、终态、取消与清理证据 |
| 发现/控制 scope | 会话隔离及授权批量操作，不改变任务或消息归属 |
| 父子委托 | 父目录中的委托任务与子会话执行关系，不包含子会话内部任务所有权 |
| 可见性 | 授权只读聚合，保留 initiator 和因果身份，不产生第二份结果权威 |

子会话自然结束后仍可独立接纳和处理 EnsureProcessing 消息；委托已完成则不得重复发布同一委托终态。需要向父会话补充报告时使用显式后续订阅或新委托事件。任何有界等待后的交接都不能把子会话 Inbox 改成父会话 Inbox。
