# WorkState 重设计：按责任持久化，按本次工作读写

状态：**推荐设计已获用户批准，实施中，完整验收未完成**。日期：2026-10-06。原 Astra 设计交付与基线保留下文；实施入口为 [独立 worktree 并行任务](2026-10-06-workstate-redesign-implementation.md)。

本次唯一写入是本文件。没有修改生产代码、数据库、schema、lock、现行设计或标准，没有启动 Peri/模型、构建、测试、性能探针或提交 Git。文中的类型、表和接口均为下一轮评审草图，不是当前实现。

## 1. 决策摘要与证据边界

**推荐一个方案：以 Mailbox、Processing、Effect 三个职责模块替代会话级 WorkState 聚合；复用现有 Control、Transcript、命令 journal 和回执；不可变大载荷独立寻址。** Processing 合并 batch、阶段游标和稳定预算；Effect 合并 invocation intent、task binding 和精确委托绑定。不是把每个 BTreeMap 搬成一张表，也不是建立泛用事件溯源框架。

保留 RCRA 已批准的可靠接纳、精确 Reason/Act 检查点、响应提交屏障、责任原子移交、Unknown 原命令对账、撤回与领取裁决、子会话隔离。删除的是重复内容、重复索引权威和全会话聚合读写，不删除保证。历史 load 不复活执行；服务化显式恢复、当前可靠消息激活和用户新发送分别准入。SDK 继续唯一管理实例和 attempt 执行权。

### 1.1 读取基线

- 初读及本轮核实 HEAD：`b7c6770d2c0a85a4e7bd5ca3fd24e28e630a6ee8`。这是**非冻结工作树**，不是以该 commit 单独构成的源码快照；开始时暂存统计为空，存在大量并行 dirty 和未跟踪文件。
- 直接核实 `work/processing.rs::trim_terminal_request`、`work/reducer.rs::reduce_work`、resources 本地/远端 `write_work`、`work/effects.rs::mutation_effects`、`work/availability.rs::READ_AVAILABILITY` 和 ACP `publish_inbox_work`。移除整份 reducer clone、复用旧 JSON、窄读、进入终态时清请求等已经出现在读取到的源码中；全部属于外部工作，不计作本 proposal 实施。
- 当前 `canonical::CURRENT_SCHEMA_VERSION` 为 **17**。部分历史设计仍提 schema 14，不能据此选择迁移起点；实施时重新核实最终版本、符号及 dirty 内容，本文不预占下一 schema 数字。
- 依据 [RCRA 权威](../../docs/design/rcra-message-activation.md) §5.3、§8.2、§8.3、§8.5、§8.6；并读 [Transcript](../../docs/design/message-transcript.md)、[异步任务](../../docs/design/session-async-tasks.md)、[用户待发队列](../../docs/design/user-input-queue.md)、[会话身份与环境](../../docs/design/session-id-environment.md)。Transcript 的旧 staging/易失 MQ 叙述不是撤销新可靠性目标的依据。
- 实施状态参考 [RCRA active spec](2026-10-05-rcra-message-activation.md)、[历史加载 P0](2026-10-06-p0-remove-session-runtime-state-restoration.md)、[资源占用 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md)。[TUI Astra 审计](2026-10-06-tui-perf-astra-audit.md) 只提供 backend 采样口径：旧映像采到 encode/decode 不等于当前构建收益，不能与 TUI 绘制重复归因。
- 读取 standards index、architecture-contracts、rust、testing、documentation、git，Agent/ACP 指引及 agent/resources/acp 索引；使用本地 `codebase-design/SKILL.md` 的深模块原则。研究限仓库与本地文件，没有网络查询或重新读取用户数据库。

### 1.2 当前为何失衡

| 当前可核实机制 | 失衡原因与设计后果 |
| --- | --- |
| `WorkState` 同时持有投递、阶段、请求、响应、调用、预算、草稿、委托和历史回执 | 独立生命周期被绑定到同一序列化和 revision 单元；已结算历史持续参与普通操作 |
| `write_work → read_snapshot/read_work_snapshot → reduce_work → mutation_effects` | 本地/远端仍读取完整 state；接受 mutation 仍编码完整新 state；`GUARD_STATE` 比较完整旧 JSON |
| `READ_DELIVERY` 用 `json_each`；`READ_AVAILABILITY` 从 blob 构造多组轻量 facts | 已减少返回 Rust/网络的正文，不等于数据库不解析大 blob，也不等于候选查询有界 |
| `WorkQuery.limit` 在 `WorkSnapshot::from_state` 后筛选 | API 的 limit 不限制前置解码、集合构造和历史扫描；不能把分表后重组 WorkState 当完成 |
| `WorkPayload.serialized` 出现在 event、delivery/projection、response、outcome、原 WorkCommand 中 | 同一内容被嵌套转义和重复持有；请求还包含实际 provider 上下文，不能简单由 messages 还原 |
| `successor` 为每个阶段创建 WorkRecord，复制 response/委托关联 | 阶段责任确有必要；每阶段一份完整可变 work、独立 budget map 和重复 binding 并非必要 |
| `REFRESH_COUNTS` 对 messages 做 `COUNT(*)`；部分规则遍历所有 work/task binding | 即使外置 payload，也须删除隐藏的全历史计数和唯一性扫描，改为事务增量与唯一索引 |

源码支持“历史证据进入高频整体处理”的机制判断；不据此宣称 RSS 泄漏、优化必然达到某倍数或某毫秒。旧 P0 中的测量数字未在本轮复测，尤其不能把已经变更的 clone/终态保留描述当现状。

## 2. 用例、保证与生命周期

| 用户/系统用例 | 必须保留的保证 | 责任主体与结束条件 |
| --- | --- | --- |
| 编辑、排队、指定发送、取回 | 原稿及附件完整；input ID 与 publication generation 分离；命令幂等 | Mailbox 保存未发布输入；成功撤回/投递不等于允许删除所有原稿证据 |
| 可靠结果投递、重复/乱序发现 | event 身份冲突可见；每收件人独立 Accepted；required 不降级 | 生产者持有责任至 Inbox Accepted；之后 Delivery 持有处理义务 |
| Receive、Compact、Reason | 精确批次、投影版本、未履行输入不被 compact 吞掉；精确实际请求可恢复 | Processing 持有责任；响应与下一阶段责任提交后才履行输入义务 |
| 并发工具、响应/结果丢失 | 响应和稳定 invocation 先落盘；未知副作用对账，不盲重发 | Effect 保存调用及结果责任；外部 owner 才是执行/资源事实权威 |
| 子 Agent、嵌套、Workflow | 子会话独立 Inbox/Transcript；父只拥有精确委托；迟到取消不伤新委托 | 父 Effect 记录 delegation；子 Processing 引用该关系；终态 Accepted 前保留发送责任 |
| Stop/Pause/Resume/Close | 精确 attempt、控制代际、命令回执；接纳控制不等于资源静止 | 复用 Control；关闭逐 owner 对账，未确认保持 Incomplete |
| load、断连、进程替换 | 历史/冻结上下文可恢复；不从非终态事实推断 live；不因重启重置预算 | Transcript 管内容，SDK 管执行权；显式恢复读取指定责任，原 owner 管外部任务 |
| Unknown mutation | 原身份、原参数、原回执；无回执不是 NotApplied | 命令 journal 保存恢复材料；服务端最终封存或取得 Applied 才解除屏障 |
| clear/fork/rewind/删除 | 不复制处理 ACK、不重跑历史副作用、不丢未知责任 | 历史操作与责任处置分开；未决关联须保留、阻塞或经授权放弃 |

保留至少一次交付与幂等接纳，不承诺外部副作用 exactly-once。best-effort owner 仍可使用，但必须诚实报告 OutcomeUnknown/Incomplete，不能借降低存储成本扩大其保证。

## 3. 推荐领域结构与深模块 Interface

### 3.1 少量实体与关系

`SessionResources` 保持消费 seam；`SessionDataPort` 的 SQLite/Turso 两个 Adapter 是实际可替换点。共同领域规则继续位于 `peri-acp-types`，Resources 封装选行、事务、引用和回执，不把 SQL 更新顺序暴露给 Agent。不新增 crate、独立存储服务或公开通用 CRUD 框架。

```text
Session / Lifecycle ─ Control + SessionHead
     ├─ Mailbox: Draft ─publication→ Delivery ─claim→ Processing
     ├─ Transcript: MessageId ─→ immutable ContentRef
     └─ Processing: batch + phase cursor + budget ─→ Effect(invocation)
                                                   └─ TaskBinding / Delegation
Journal + Receipt ─→ 原命令、精确阶段证据、终态待投递
Payload storage ─→ 不可变内容与检查点字节；不拥有调度语义
```

三处领域职责：Mailbox 管发送/接纳/领取，Processing 管阶段责任，Effect 管副作用与委托关联。Control 是已有生命周期权威，journal/receipt 是共同提交机制，payload 是存储机制；不为它们制造另一套调度模块。

类型草图只表达身份和责任，ID/Rev 均为 newtype；未展示的授权、错误与字段见后面的 schema 和流程，不作为可编译实现：

```rust
struct SessionHead {
    session: SessionId,
    lifecycle: Lifecycle,
    change_seq: ChangeSeq,
    next_delivery_seq: DeliverySeq,
    limits: WorkLimits,
    usage: AdmissionUsage,
    recovery: RecoveryDescriptorRef,
}

struct Draft {
    input: InputId,
    lifecycle: Lifecycle,
    revision: DraftRev,
    generation: PublicationGeneration,
    original: PayloadRef,
    status: DraftStatus,
}

struct Delivery {
    id: DeliveryId,
    event: EventKey,
    recipient: SessionId,
    lifecycle: Lifecycle,
    policy: MessagePolicy,
    sequence: DeliverySeq,
    content: ContentRef,
    projection: Option<MessageId>,
    processing: Option<ProcessingId>,
    obligation: ObligationDisposition,
    revision: DeliveryRev,
}

struct Processing {
    id: ProcessingId,
    session: SessionId,
    lifecycle: Lifecycle,
    revision: ProcessingRev,
    execution: ExecutionAssociation,
    phase: Phase,
    phase_sequence: PhaseSequence,
    budget: WorkBudget,
    checkpoint: Option<MutationId>,
    delegation: Option<DelegationRef>,
}

enum Phase {
    ReasonReady,
    ReasonInFlight { request: RequestId },
    ActReady { remaining: InvocationCount },
    Blocked { resume: ResumePhase, evidence: EvidenceRef },
    Settled,
    Abandoned { evidence: EvidenceRef },
}

struct Effect {
    invocation: InvocationId,
    session: SessionId,
    lifecycle: Lifecycle,
    processing: Option<ProcessingId>,
    phase_sequence: PhaseSequence,
    revision: EffectRev,
    intent: IntentRef,
    binding: Option<TaskBinding>,
    status: InvocationStatus,
    outcome: Option<OutcomeRef>,
    delegation: Option<DelegationIdentity>,
}

struct PayloadRef {
    storage_scope: StorageScope,
    id: PayloadId,
    encoding: EncodingVersion,
    byte_length: u64,
    sha256: Digest,
}
```

- Processing 的身份是一批处理责任，不是一次 attempt，也不是会话一生。阶段推进更新游标；旧阶段的精确请求、响应、动作集合及回执留在对应不可变命令证据中，不靠重放 journal 重建当前状态。
- `ExecutionAssociation` 保留 SDK admission ID、精确 turn/attempt、instance/generation 关联及进入/退出证据。它描述 SDK 已做的准入，不能申请、续租、抢占或证明 SDK 执行权。热运行是否存在另由当前 SDK/运行句柄证据回答。
- 批次集合以 Delivery 的 `processing_id + batch_ordinal + projection_version + participates_in_reason` 为单一权威；Processing 不再复制三份 delivery ID 集合。BeginReason 的不可变命令保留该批次引用和投影身份。
- 同一个 Effect 保存 invocation 与后续 owner task 的唯一绑定。`invocation_id != tool_call_id`：前者持久执行身份，后者来自模型、用于消息配对与 UI。原模型参数和审批后有效参数都保留，不能合并成一个参数值。
- 父委托是父会话的一条 Effect；子 Processing 存 `(parent_session, parent_lifecycle, delegation_id)` 精确引用。独立工具结果只投子 Inbox；一个子会话可以先后承接多个委托。同一执行若无法隔离多个委托的取消，拒绝合并，不能扩大取消范围。
- SessionHead 是定长标量和少量引用，不含 map/历史列表。历史 lifecycle 各有一行恢复描述，Control 指向当前 lifecycle；owner 连接目录、子恢复 metadata 只按需读取引用，不能带进每次 mutation。

### 3.2 Transcript 与载荷的权威

**Transcript 是唯一 canonical 内容权威。** `messages` 保存消息身份、顺序、角色、flags/projection 与 ContentRef；Work/Effect 不再另存一份可变 response/result。未投影的 Delivery 保存不可变来源引用，不因此成为第二份 Transcript。

内容编码将消息身份 envelope 与不可变 body 分开，同一来源投给两个会话可引用同一授权存储域内的 body，但各有独立 MessageId/Delivery/ACK。Receive 创建 canonical envelope；不为改 message ID 再编码整份正文。不同语义的原稿、准备后的模型输入、owner 原结果和有界摘要可以分别保存，必须有来源关系，不能因字节相似随意混成一个事实。

Reason checkpoint 是精确实际 provider 请求证据，不是第二份可编辑会话内容。保留动态 system、tools、model、endpoint、credential grant 引用和精确请求体；不能只存 Transcript cutoff/digest 后用当前配置重建。每轮写入这一个请求的成本仍然存在。已批准契约不允许为了性能静默改成“失败后重新 Reason 即可”。

## 4. 现有字段与 actions 的完整去向

### 4.1 WorkState 字段

| 字段 | 去向与删除依据 |
| --- | --- |
| `revision` | SessionHead.change_seq 用于变化通知/退出交接；普通 Effect 更新不用全会话 expected revision |
| `next_admission_sequence` | SessionHead.next_delivery_seq，事务分配；不按 UUID/墙钟排序 Inbox |
| `limits` | SessionHead 限额；预算默认沿当前已批准 policy，定制 limits 保留 |
| `deliveries` | Delivery 行；正文改 ContentRef，投影指向 messages |
| `obligations` | 并入 Delivery 的 required 状态/原因/processing 关联；一条投递的义务不再跨两个 map |
| `batches` | Processing 身份 + Delivery 关联列；精确成员/参与 Reason/投影版本不丢 |
| `works` | 合并为 Processing 阶段游标；旧 work/阶段身份保存在 checkpoint 命令证据及迁移映射 |
| `budgets` | 并入 Processing 的稳定累计；旧共享 budget 的连续阶段必须归同一 Processing，不能迁移时拆开重置 |
| `invocations` | Effect，单调用修订及不可变 intent/outcome 引用 |
| `task_bindings` | 并入同一 Effect 的 write-once owner task 绑定；唯一约束替代全 map 搜索 |
| `legacy_unknown` | journal 中 typed LegacyEvidence 记录，按关联身份可查；不解释为 Pending/Satisfied |
| `admissions` | Control 保留当前精确观察，进入/退出原命令与回执由现有 journal/receipt 保存；删 map 内重复回执 |
| `resource_owners` | 对应 lifecycle 的 RecoveryDescriptor 引用；保持可信 owner 路由、权限上限和凭据引用，不是执行 owner lease |
| `child_resume_metadata` | 同一 RecoveryDescriptor 内有版本的 child 部分；不复制到每阶段 |
| `terminal_obligations` | journal 内 typed outbound terminal command，保存 origin、recipient、delegation/admission 和发送责任状态 |
| `terminal_acknowledgements` | 指向收件人 durable receipt 的引用/校验信息；原回执仍在 receipt 表，不再完整复制进会话状态 |
| `work_delegations` | Processing 上的稳定 DelegationRef，跨 phase 不复制 TaskBinding |
| `staged_user_inputs` | Draft 行，含原稿引用、顺序、发布代际、撤回关联及状态 |
| `user_input_publications` | 原 PublishSelected 命令 journal 是权威；Draft/Delivery 只存 publication mutation ID，删重复 WorkCommand map |

嵌套信息也不隐式裁剪：`WorkRecord.request_id/reason_request/response` 归 checkpoint 命令及 MessageRef；`stage/resume_stage/reason/recovery_condition/revision` 归 Processing；`invocation_ids` 由 Effect 的 processing/phase 索引取得。`ProcessingBatch.execution/recipient_lifecycle` 归 Processing，其他成员信息归 Delivery。`InvocationIntent` 的 tool names、原/有效参数 digest、owner、scope/epoch、authorization、recovery_locator 全部保留在 intent/binding。`StagedUserInput.command_id/fingerprint/publication_id/withdrawal` 保留对应命令关联，fingerprint 仅作已有输入校验，不能替代完整命令摘要。`WorkSnapshot.state` 整体删除，换窄 DTO；`pending_commands` 独立分页查询。

### 4.2 WorkAction 映射

以下覆盖当前枚举的全部变体；“并入”表示同一领域意图的事务化重组，不表示删除幂等或校验。

| 当前 action | 新领域操作与责任 |
| --- | --- |
| `StageUserInput` | Mailbox.Stage：只保存 Draft 与原命令回执，不生成义务 |
| `WithdrawStagedUserInput` | Mailbox.TakeBackDraft：Draft 条件更新与完整原稿回执 |
| `PublishStagedUserInputs` | Mailbox.PublishSelected：固定选中集合、发布代际；必要时与精确中断/显式 Resume/放弃旧 processing 原子裁决 |
| `RegisterAdmission` | Processing.Enter：核对 SDK 关联和控制许可，写进入证据/当前执行观察，不授予执行 ownership |
| `FinishAdmission` | Processing.Leave：精确退出证据与观察清除同事务；终态交付责任独立保留 |
| `BindResourceOwners`、`BindChildResumeMetadata` | BindRecoveryDescriptor：同 lifecycle 的可信恢复描述分项 write-once/冲突校验 |
| `BindTerminalObligation`、`AcknowledgeTerminalObligation` | Effect.PublishTerminal/ConfirmDelivery：journal outbox 与接收方 receipt；不是模型处理 ACK |
| `BindWorkDelegation` | Processing.BindDelegation：核验父 Effect/父绑定回执；固定关联，不把父 binding 拷入每个阶段 |
| `PublishDelivery`、`PublishTaskSettlement` | Mailbox.Accept：共用接纳事务；后者额外核验 Effect/owner terminal 身份 |
| `ClaimBatch` | Mailbox.Receive：SQL 选择有限集合，投影/成员/Processing 原子提交 |
| `BeginReason` | Processing.BeginReason：精确请求引用、预算与 InFlight 同事务 |
| `CommitReasonResponseAndDispatchIntent` | Processing.CommitResponse：canonical response、Effect intents、输入 Satisfied 与 Act/后继责任同事务 |
| `BeginDispatch` | Effect.AcceptDispatch：单 invocation 的副作用发送屏障、预算和控制资格 |
| `OutcomeUnknown` | Effect.RecordUnknown：保留原 invocation、证据；阻塞对应推进，不误记 Failed |
| `CommitAct` | Effect.CommitResults：逐项持久结果；最后一项原子使 Processing 转 ReasonReady 或显式终结 |
| `BlockWork`、`ResumeWork` | Processing.Block/Resume：保留 resume phase、证据与恢复预算；不能隐式重复 InFlight Reason |
| `AbandonWork` | Processing.Abandon：显式授权；仅放弃可执行 processing，不删除调用、结果或交付证据 |
| `SettleWork` | 并入阶段终结事务或显式 FinishProcessing；必须说明无后继理由，删“先结算再补后继”接口 |
| `PrepareInvocation` | Effect.Prepare：非模型入口的可信独立调用仍可用；模型调用必须随 CommitResponse 同事务，不保留第二次重复准备 |
| `ReconcileTaskBinding` | Effect.BindOwnerTask：单 Effect write-once 与唯一索引，旧观察不覆盖可信关联 |
| `WithdrawDelivery` | Mailbox.WithdrawPublication：与 Receive 竞争同一 Delivery；成功后才允许重发新代际 |
| `AbandonDelivery` | Mailbox.Dispose：显式原因/证据，不伪造 Satisfied |
| `ResetBudget` | Processing.ResetBudget：授权命令及回执；不随 attempt/重启自动重置 |
| `QuarantineLegacy` | RecordLegacyEvidence：保留原始身份/证据引用与阻塞范围，等待对账或明确裁决 |

## 5. SQLite / Turso 共同逻辑 schema

### 5.1 表数与复用

保留既有消息、会话、binding/frozen、Control、event、command、receipt 表；替换 `session_work_state` 为固定 head；只增加 **inputs、deliveries、processing、effects、payloads 五类表**。不另建 batches、budgets、admissions、task_bindings、delegations、outbox 各一张表。Control 小记录可以保留现有版本化编码，调度用标量列只能是同事务维护的投影，不再增加另一份业务状态。

下表为逻辑 DDL，具体列名/约束语法在获批后按两 Adapter 实际能力定稿；所有身份列非空，revision/sequence 使用检查溢出的非负整数。状态枚举要有 CHECK，未知格式/枚举 fail closed，不以默认值掩盖损坏。

| 表 / 主键 | 承载数据及必要约束/索引 |
| --- | --- |
| `session_work_head` / `(session_id,lifecycle)` | format_version、change_seq、next_delivery_seq、limits、配额用量标量、recovery_ref。当前 lifecycle 来自 Control；无正文、集合或历史 map。复用旧 work 表迁移位置，不与 state_json 并存运行 |
| `session_inputs` / `(session_id,lifecycle,input_id)` | input_revision、fifo_seq、publication_generation、status、original_ref、stage/withdraw/publication mutation 关联。`UNIQUE(session_id,lifecycle,fifo_seq)`；`(session_id,lifecycle,status,fifo_seq,input_id)` 支持待发分页 |
| `session_work_events` / `(producer_namespace,event_id)` | event_kind、causation、body_ref、完整内容摘要、format_version；替代旧 event_json 内嵌正文。同 key 异内容/身份属性冲突；producer namespace 来自可信调用方 |
| `session_deliveries` / `delivery_id` | recipient/lifecycle、event key、受控 purpose、policy/activation/绑定 attempt、sequence、delivery_revision、projection_message_id/version、processing_id、batch_ordinal、participates_in_reason、义务状态/处置证据、publication 关联、计费字节。`UNIQUE(producer,event,recipient,lifecycle,purpose)`；`UNIQUE(recipient,lifecycle,sequence)`；用户发布代际唯一映射 event；`UNIQUE(processing_id,batch_ordinal)`（已领取行） |
| `session_processing` / `processing_id` | session/lifecycle、revision、phase_sequence、phase/resume phase、SDK execution association、reason request_id、checkpoint mutation、预算累计/策略版本、Act remaining、阻塞/放弃证据、delegation ref。`(session,lifecycle,phase,processing_id)`；`(parent_session,delegation_id,processing_id)` 查精确委托关联 |
| `session_effects` / `invocation_id` | 发起 session/lifecycle、processing/phase、effect_revision、tool_call_id、intent_ref、status、outcome_ref、owner/scope/epoch、owner_task_id、授权/恢复引用、可选 child/delegation、terminal transition/清理证据。非空 task binding 上 `UNIQUE(initiator_session,owner_identity,owner_task_id)`；`UNIQUE(processing_id,phase_sequence,tool_call_id)`；`UNIQUE(parent_session,delegation_id)`（委托行）；按 `(session,lifecycle,status,invocation_id)`、`(processing_id,phase_sequence,invocation_id)`、`(child_session,delegation_id)` 建索引 |
| `session_work_commands` / `mutation_id` | 复用原 journal：origin_session、recipient_session/lifecycle、kind、subject_id、phase_sequence、digest、command_ref、小型 guard envelope、journal_state、delivery_state、admission_id、parent binding/receipt 引用。请求 ID 在 BeginReason 命令上唯一；admission 进入/退出各按 `(kind,admission_id)` 唯一；publication 选择快照在不可变 command_ref 中。pending/terminal outbox/legacy 隔离分别有带 session/lifecycle 的部分索引 |
| `session_work_receipts` / `mutation_id` | digest、recipient/session、持久 Applied(含 Accepted/Rejected) 或 FinalNotApplied、原小回执与相关实体 revisions；原结果不可覆盖。ACK 重试读本行；absence 不代表 NotApplied |
| `session_payloads` / `(storage_scope,payload_id)` | kind、codec/version、byte_length、sha256、immutable bytes、retention_class、creation/retire metadata。`UNIQUE(storage_scope,kind,codec,version,sha256,byte_length)` 用于域内内容去重；命中需验证已有对象元数据和字节一致，异常冲突拒绝，不把 hash 当授权 |
| `messages` / 既有 `message_id` | 继续唯一 canonical envelope；`thread_id,role,content_ref,flags,projection`，替换旧 content 内嵌存储，不运行双份内容列。增加可稳定分页的 transcript_seq 与 `(thread_id,transcript_seq,message_id)` 唯一/排序索引；projection 的版本化内容可引用 payload |

`threads`、`session_bindings`、frozen/inherited_context 的归属契约保持；本轮目标不再拆这些低频字段。`session_control_state`/`session_control_receipts` 保留现有控制权威；当前观察指向 admission 原记录，终结历史留 journal。`session_close_intents` 在切换时消除与 Control 的独立关闭状态写入，统一以 Control Closing/Closed 为权威，历史关闭证据保留；不是两处长期双写。删除 thread 后保留小型 Control Closed/tombstone、必要去重与责任证据，迟到消息不能重建同 ID 会话。

`peri_op_ledger` 保留为 Turso 传输提交/最终封存机制，WorkReceipt 是领域决定，两者层次不同，不能为了“单一权威”删掉远端最终性证明。新格式引用领域 mutation/digest，避免在传输 receipt 重复大 command。不存在同功能的第二套事件回放数据库。

journal 的两种责任必须显式区分：`origin_session` 拥有 outbound 的保留/发送责任，`recipient_session` 拥有入站业务提交与原回执。仅 OutboundReady 不构成收件人的 Unknown mutation；只有开始收件提交后才进入对应 mutation 屏障。同库共享一条稳定命令身份及回执，跨库各自保存同身份的发送/接收事实；命令正文不可被这些状态更新改写。pending 索引必须包含责任方向和状态，不能让历史未 ACK outbox 冒充当前执行锁。

### 5.2 有界选行与完整性

- Delivery 的未领取队列索引为 `(recipient,lifecycle,queue_state,sequence,delivery_id)`，其中 queue_state 由该行状态唯一推导并在同事务更新；required 与 optional 各有部分索引，ContinueCurrentRun 另按绑定 attempt 索引。队列选择先各取 SQL `LIMIT` 的候选再有限合并，明确给 required 预留位置；不能让海量 Passive 排在前面使驱动消息永久饥饿。保持各组接纳顺序及显式因果约束。
- 已领取 batch 通过 `(processing_id,batch_ordinal)` 直接查询；candidate/Unknown/outbox 查询使用部分索引的状态谓词，不从全部历史遍历后 `take(limit)`。跨会话扫描先分页 session，再分页直接责任，不每两秒递归重建整棵父子树。
- 单 delivery 去重读 identity/digest/status/receipt 引用，不读取正文。只有发生实际同身份内容核验/首次投影时按引用取必要正文。哈希校验不需要每次读取全部历史。
- 外键可作为本地安全网，**不能依赖远端级联或未验证的 FK enforcement**。共同事务计划显式验证每个被引用的 session/lifecycle、payload、parent binding、canonical message 并执行删除/更新；guard 失败使整个事务回滚。不允许一条 UPDATE 影响 0 行却继续写 Accepted 回执。
- `threads.message_count` 只按本次真正插入/删除的 canonical 行数增减；幂等重放增量为 0。完整重算只允许显式离线一致性检查，不放入 mutation 热路径。

### 5.3 大载荷：先复用，缺失能力才补

现有 `messages` 可承载 canonical 记录，Work event/command/receipt 表可承载身份与证据；未发现 Resources 中已有满足“任意检查点、不可变引用、版本/hash、授权域、跨重启保留”的通用 payload 存储。Workspace `store_output` 是 MCP 输出能力，不证明其产物有 Inbox 所需的 durable pin/退休契约，不能把临时文件路径直接当 request checkpoint。

因此首版推荐 `session_payloads` 放在**同一个选定数据库**：SQLite 存本地 DB，Turso 全存远端 DB，不建本地 sidecar 文件或借 mcp-packages 之上的磁盘 fallback。这是一条不可变消息/请求一个对象，不是一个会话一个巨 blob；单对象、单命令和单事务都有字节准入上限。超过上限在 Accepted/BeginReason 之前明确拒绝/Blocked，原稿和原 owner 责任保留，不能静默截断。初版不另造分块对象服务；必须支持更大单对象时再以真实用例设计分块协议。

- canonical body 使用带 kind/version 的 UTF-8 编码；精确 provider checkpoint 按冻结字节保存，不嵌入另一层 JSON 字符串。首版不压缩，避免新增 codec/解压预算和跨版本不确定性。命令摘要以稳定版本化 envelope + 有序引用摘要计算，重试不得重序列化为另一份语义近似命令。
- Blob 创建与引用的责任确认同事务。必要的预上传是不可见 Prepared 对象，只有完整 hash/长度校验和引用提交后才可 Accepted；孤立 Prepared 可以按安全水位回收，绝不能把“上传开始”当成责任接纳。
- message、event、command、Effect 共享不可变引用；正文变更产生新内容/消息身份。模型请求天然含已渲染历史，即使消息已经保存也可能必须写一份完整新请求；方案消除后续反复读写旧请求，不声称消除这一契约成本。
- Payload ID/hash/session ID 均不是能力凭证。读入口以认证主体、存储域、session 权限和恢复授权检查；工具恢复重验原权限上限与 owner 身份。只保存 credential/grant 引用，不把旧 bearer token 当可复用权限。
- owner 大结果若已有稳定取回、完整性、授权、pin/release 和故障域保证，可原样复用 owner 产物引用；Accepted 前固定 owner namespace、artifact/version、长度/hash、grant 和 pin receipt。缺一项则转存同库或拒绝可靠接纳；不假设所有 MCP 都可靠。
- GC 以 message/event/未决 command/receipt、processing checkpoint、Effect/outbox、原稿及备份退休证据作为 roots。首版保守保留已被引用的证据；未被引用的 Prepared 对象可分批候选扫描，删除事务再次逐类索引检查无引用且未被新 pin。外部引用释放在本地解除责任确认后执行、失败重试；不引入易漂移的引用计数作为唯一删除依据。

## 6. 精简读写 Interface 与事务流程

### 6.1 消费者只学习这些行为

```rust
trait SessionWork {
    async fn inspect(&self, query: WorkInspection) -> Result<WorkPage>;
    async fn execute(&self, command: WorkCommand) -> Result<WorkReceipt>;
    async fn resolve(&self, identity: MutationIdentity) -> Result<WorkResolution>;
    async fn read_evidence(&self, query: EvidenceQuery) -> Result<EvidencePage>;
}
```

这是 SessionResources 的工作能力面，不是新增上层 pass-through。`inspect` 只允许 Availability、DeliveryHeader、DraftPage、ProcessingSlice、PendingMutationPage 等窄、强类型查询；不提供 “include_all” 或动态任意 join。`execute` 接收 Mailbox/Processing/Effect 的 typed intent，隐藏需要触及的行、投影和回执。`resolve` 从服务端恢复原命令；调用方不能再提交重造的 expected revision。`read_evidence` 按授权和明确 ID/页读取正文，通常只有执行/历史展示/人工对账使用。

小 Interface 不等于一个 `Value` 万能方法：每种 intent 在领域层规定读取集合、guard、写入集合、结果与预算，Adapter 不重新实现业务规则。领域决策输入是本次有界事实切片，输出是 typed transition；不接受完整 WorkState。组装事实与提交的中间 seam 仅限 Resources 内部。

### 6.2 普通提交与 Unknown

1. 在 ingress 校验身份、权限、格式、单次字节/条数预算；读取命令/回执唯一键。相同 mutation/digest 返回原结果，异参冲突。需要提前保存原命令时，journal 先记录完整 immutable command_ref；这只是持有命令，不是业务 Accepted。
2. 业务事务重读当前 Control 与本次目标行，校验 revision/lifecycle/关联；应用所有转移、canonical 投影、配额差额、change_seq、receipt **同生共死**。Rejected 同样有持久回执，但没有部分领域效果。
3. 业务提交确认后发布内存 projection、通知和 UI 回执。journal 的 reconciled/热态同步确认可独立幂等完成；失败回读原 receipt，不能重做副作用。
4. 传输超时、commit ACK 丢失、连接取消或读不到 receipt 均为 Unknown。冻结该命令涉及的热态、处理批次和后继 dispatch；初版保留同会话未决 mutation 屏障的保守语义，控制结算只走显式可恢复入口。不通过“窄表”擅自放宽未知提交限制。
5. `resolve` 只能取得原 Applied，或在同一唯一命令身份空间写入 FinalNotApplied 封存。迟到 apply 必须与该封存竞争并被拒绝；查询无记录不是最终未应用。Turso 的 qualify/closure 与领域 receipt 关联原 mutation，明确内部失败尝试与领域命令最终封存的区别。
6. Unknown 只查询/重试同 ID、同 envelope 和同引用。若原命令已经确定 Rejected/FinalNotApplied，下一次经重新决策的命令才可用新 ID；原工具 intent 不因此变成新副作用。不得在 Unknown 后补造模型响应、重新 Reason 或反向补偿删除消息。

本地使用同连接写事务（现有 `BEGIN IMMEDIATE` 可复用）；远端若先读再规划，托管原子批必须重验完整 read-set 和唯一约束，guard 未命中令整批失败。可沿现有 qualified mutation/guard 机制实现，但改比较小 revision 与必要不可变身份，不传完整 state_json。不是靠进程 mutex 保证远端一致性。

### 6.3 正常工作与责任交接

| 路径 | 原子步骤及可观察结果 |
| --- | --- |
| Stage/普通提交 | 写 Draft/原稿引用/命令回执；loading 时不生成 delivery。不把成功排队当 Delivered |
| 指定发送 / idle FIFO | 同一命令固定选择集合及 input revisions；整批校验后生成 publication generations、events、deliveries。指定集合超单事务上限明确拒绝，不能悄悄分批导致部分发送；自动 idle 每次只选一条 |
| Publish / TaskSettlement | 校验 producer、收件人、lifecycle、policy、容量和可信 task binding；event+delivery+required obligation+receipt 同事务。重复返回原接受/撤回/处置事实，不再创建义务；Notify 失败不撤销 Accepted |
| Receive | 在事务内按 SQL 取有限候选，或重验指定精确集合；生成 canonical envelopes、Delivery 领取/投影版本/成员和 Processing ReasonReady。只有本批实际 required 纳入处理；晚到消息不得被 ACK。Passive 不单独 Reason |
| 输入准备 / Compact | 本批输入准备只处理本批 ID；原稿不改。Compact flags/摘要提交引用精确投影版本，未履行输入必须保留可处理投影；版本变动与 BeginReason 在事务中判定，不能读旧视图后使用新 flags 误履行 |
| BeginReason | 生成 PreparedModelCall 后保存实际完整 request_ref、request_id、model/授权、batch/projection 身份，累计预算，转 InFlight；同 mutation 重试不再扣预算。确认 Applied 后才 `start`，发送使用同一冻结请求 |
| 模型响应 | response canonical ref、所有有效 intent（含原/审批后参数及授权）、本批输入 Satisfied 和下一 phase 同事务。无工具时明确下一 Reason 或结束；有工具必有 ActReady。未可靠提交绝不能 dispatch；已经提交的响应不能因取消从历史删除 |
| BeginDispatch | 校验精确 effect/phase、control generation、attempt、授权与额度；Prepared→DispatchAccepted 持久确认后才调用 owner。该名称只表示 Peri 发送屏障，不声称 owner 已执行 |
| 并发工具返回 | 按 invocation 保存独立 result_ref、canonical ToolResult 和状态；重复同结果不重复扣 remaining，异结果冲突。最后一项完成时同事务把 Act 责任转下一 ReasonReady；停止/放弃态只结算证据，不偷偷创建后继 |
| 工具结果不确定 | DispatchAccepted 后进程退出或响应丢失 → OutcomeUnknown；先按 invocation/owner task 发现。证明完成则提交旧调用结果；证明未执行且 owner 支持原幂等身份时才按授权继续；无法证明保持 Blocked/Incomplete |
| 自然退出 | 同事务核对相关未决责任、head change_seq 和精确 execution observation，提交退出证据并清对应观察；并发 publish 要么使检查失败/继续 Receive，要么留作下一次候选。通知丢失由 SDK 扫描同一事实补偿 |

模型发送后的进程退出与 Store Unknown 是两种未知：BeginReason 已提交但无模型响应时保留原 request identity/checkpoint，不能从“没有 response”推断模型未调用。provider 不提供对账时显式阻塞或由新用户发送授权放弃旧 processing；不自动生成替代响应。可接受重复执行的产品裁决不等于允许忽略已批准 mutation 屏障。

### 6.4 子任务、委托与终态交付

1. 父在发起前持久化 Effect intent、发起会话/epoch、可信 owner/scope、delegation ID 和恢复授权；创建响应丢失按 invocation 发现，不能重新 spawn 新子会话。发现先到与响应后到都 bind 同一 Effect。
2. 子有自己的 session/lifecycle、Mailbox、Processing、工具 Effect。SDK 准入后，子 Processing 绑定父委托；同库在事务内核对父 Effect 及原绑定 receipt，跨库须验证由可信入口获得的持久绑定凭证，无法证明则拒绝执行，不信任模型传来的 JSON receipt。
3. 子执行终态、资源清理结果和给父的 terminal command 在同一次本地结算中落盘；命令稳定 event key 派生于 delegation/terminal transition。journal 承载 outbound Pending→Accepted/明确处置，不新建通用 outbox 框架。
4. 同库可以一次事务提交父 Inbox Accepted 与子交付确认；跨库严格先提交子 outbox，再调用父接纳，取得父 receipt 后确认子交付。ACK 丢失重发原命令，父去重；子不能提前释放产物。没有父 runtime 也可以存父 Inbox，路由不可达保持待投，不 fallback root。
5. `cancel(D)` 查询 D 固定绑定的 child/execution/task，已终态 D 返回原终态；不能取消 C 后续 D2。复用同一个 child session 不复用 delegation ID。父 Close 对 Independent 明确等待或持久 detach/abandon；子不受隐式取消，原结果留 owner/委托记录，父 Closed 不被回调重开。
6. 已放弃且不可执行的旧 Processing，其未 ACK 终态 outbox 是交付责任，不是新输入必然被阻断的理由；但未确定 mutation、缺失委托绑定或当前执行屏障不能被忽略。FinishAdmission 保留精确退出/交付关联证据，不能仅凭 attempt=None 推断完成。

### 6.5 撤回、clear/load/new、取消、断连与重启

| 操作 | 推荐语义与原子点 |
| --- | --- |
| 撤回发布 | 事务检查 Delivery 未领取且相同 publication generation；写不可执行处置、Draft 回待发及原稿回执。Receive 先赢则拒绝撤回；撤回先赢则 Receive 不可领取。Unknown 时 UI 保留未确认原稿，不重新发新代际 |
| 重发 | 显式新命令增 publication generation，event key 由 `(input_id,generation)` 产生。旧命令仍返回旧撤回回执，不复活旧 obligation |
| new | 新 Session ID、binding/frozen/head/Control 的建立遵循现有原子创建/失败补偿；旧会话草稿和责任不自动迁移，UI 当前 session 切换后不得重投旧命令 |
| load / replay | 分页恢复 canonical 历史与冻结上下文；可读历史责任状态但不登记 Running、不发模型/工具、不默认恢复旧阶段。可执行环境加载仍验证保存 binding/env/frozen，历史读取与执行准入分开 |
| 显式新用户发送 | 无当前 attempt 时，以 lifecycle/control generation、旧 processing 的精确 revisions 做已批准的放弃裁决，再建立新批；有当前 attempt 时只中断精确执行。旧请求、结果、unknown Effect 和交付证据保留。不能声称永久冻结的旧 mutation 已失效 |
| clear | 当前 `ClearCommand` 返回空消息，这是历史操作而非取消授权。推荐只在无受影响执行/Unknown、且待处理内容有保留或明确处置时提交历史清空；引用内容可退为证据，不能级联删除 obligations、草稿或 owner 绑定。否则明确拒绝并给冲突身份。clear 不隐式 new/reopen/resume；若产品要“一键放弃旧任务并清空”，须新增明确裁决，见 §11 |
| fork / rewind | fork 只复制历史/冻结上下文，不复制 Inbox、budget、Effect/ACK 或自动执行权限；按已批准完整工具边界准入。rewind 影响未履行内容时先显式保留/阻塞/放弃，不能借消息删除重置已发生副作用 |
| Stop / Pause / Resume | 复用 typed Control 命令；command ID + lifecycle/control revision + 精确 attempt 裁决。Stop 原子记录取消与自动激活暂停；迟到 Stop/Resume 不改变后来的执行。恢复预算需要单独授权 |
| Close / Reopen | 关闭新准入后逐 owner barrier 对账，在途创建也属于关闭集；Unknown/失联保持 Incomplete。仅既有结算进入 Closed 路径、不启动模型。Reopen 新 lifecycle，不改标旧 Effect/Delivery；旧命令只结算旧生命周期 |
| transport 断开 | 终止本连接请求与 host-owned 资源按现有关闭契约 drain；不是所有持久会话 Close，不把外部任务标 Cancelled。待投递和 unknown evidence 留存，重连只恢复观察/对账 |
| 服务进程重启 | SDK 建立执行唯一性并先对账原 mutations/owners；仅显式恢复授权指定的 Processing 可继续阶段。新的合法 EnsureProcessing 可激活；历史 load/observer 登记本身没有旧阶段恢复授权。ContinueCurrentRun 过期只 Suppressed，不跨 attempt |

激活查询显式带 `ActivationCause`：当前可靠投递、用户发送或服务恢复命令及其工作关联。SDK 与领域共享同一候选/控制判定，ACP 无第二份规则。load 不生成 ActivationCause；服务部署若配置自动恢复策略，该策略本身须是明确授权、持久且可审计的 SDK 恢复请求，不借重连或打开历史代行。旧未处理 required 始终可枚举；未获恢复许可时标明等待条件，不通过 observer 的临时 floor 把责任永久隐藏。新消息到达也不能顺便重放旧副作用。

## 7. 原子边界、revision 与竞态

### 7.1 各 revision 只守自己的事实

| revision / 身份 | 守护对象 | 不承担的职责 |
| --- | --- | --- |
| Control revision、lifecycle、control_generation | Pause/Resume/Close 与执行关联的有效性 | 不签发 SDK claim、租约或接管证明 |
| Head change_seq | 通知 cursor、无工作退出交接；事务内配额/序号增量 | 不是每个工具结果必须携带的全局 CAS |
| Draft/Delivery revision | 撤回、发送代际、Receive 领取及处置 | 不代表模型已经处理 |
| Processing revision + phase_sequence | BeginReason/CommitResponse/阶段后继、稳定预算与委托引用 | 不决定实例 ownership |
| Effect revision + invocation_id | dispatch、结果提交、owner binding/取消对账 | 不证明外部副作用尚未/已经发生 |
| mutation ID + digest + receipt | 原命令幂等、Applied/最终封存 | 不替代领域资格及授权 |

两个工具结果无需在调用方争抢同一个 Processing expected revision：各自以 Effect revision/phase identity 提交，事务内部按“本次首次 Settled”更新 remaining；最后一项条件转移阶段，重复结果不能再次减计数或生出第二个 Reason。远端预读到的 remaining 仅供计划，提交必须条件校验或直接 SQL 原子减量；冲突只重读当前处理关联，不读取会话历史。dispatch 预算也原子条件增量，不能由多个调用者先查后各自放行。

同会话当前 journal 屏障意味着短存储提交可以串行，外部工具仍并发运行；首版不以放宽 Unknown 为代价追求多个未确认写同时在途。过时**预读**可在同一命令 envelope 内重新规划 SQL；已最终 Rejected 的 expected qualifiers 不能悄悄替换再以同 ID 重新执行。

### 7.2 必须在同一事务验证的反例

- **撤回 vs Receive**：竞争同 Delivery 的未领取状态；不能先从内存移走后再写 DB。
- **响应 vs Pause→Resume**：提交核对请求开始时的 control generation，不借较新 Resume 放行旧响应；拒绝响应仍保留可对账证据，不能发工具。
- **响应 ACK 丢失**：canonical response/输入履行/intent/ActReady 要么全有，要么全无；receipt Unknown 时全部按原 mutation 查询。
- **两工具同时结束**：单调用唯一结果 + remaining 条件更新 + phase 转移 + receipt 原子；最后者唯一，前者崩溃也不丢其结果。
- **publish vs exit**：退出事务读取工作存在性和 change_seq，清精确观察；publish 在前则退出失败/继续，publish 在后则保留新的可枚举工作。Notify 丢失不改变结果。
- **父绑定/终态 ACK**：同库核验父 Effect 和已持久 receipt 与子状态同事务；不能相信事务前查到的、可变或错误生命周期父记录。跨库用 outbox/inbox，不宣称跨库事务。
- **控制 vs dispatch**：Control 更新与本地 DispatchAccepted 有 DB 排序点；owner 接纳端另校验其 scope epoch/closing gate。已越过发送屏障的在途调用仍可能发生，暂停回执只能先称 Accepted/Pending，不能宣称瞬时静止。
- **清理 vs 引用新增**：GC 删除与新引用竞争同一存储事务，payload 不存在须使业务接纳失败；不能先回收再异步修引用。

这些是存储事务 guard、业务状态版本及生命周期屏障。即使某记录有 `instance_id`，Peri 也不比较它来认领执行权，不新增 lease/owner CAS/fencing。SDK 失去唯一性时本方案不能神奇阻止所有重复外部调用。

## 8. 成本预算、增长与保留

定义：历史字节 `H`、历史记录数 `N`、本次批次条数 `B`、当前 phase 关联工具数 `I`、本次新载荷字节 `P`、尚未结算责任数 `U`。索引查找有 `log N` 成本；有界返回不等于无索引页 I/O。以下是**验收目标**，不是实测结果。

| 操作 | 允许的成本 | 确定性禁止项 |
| --- | --- | --- |
| availability / 单 delivery / pending 存在性 | 固定次数索引点查/范围查，或明确 SQL LIMIT 的轻量页 | 历史 payload 读取/解码 0；不 `json_each(state_json)`、不构造 WorkSnapshot |
| 普通状态 mutation | 本次目标行与小回执，`O(log N)` 点查/有限行更新 | 重新编码历史 payload 0；不 COUNT 全历史，不带完整 JSON guard |
| Accept / Receive | `O(B log N + P)`，返回和分配随本批 | 不扫描/复制其他 batch、旧 requests、其他 session |
| BeginReason / CommitResponse | `O(P + B + I)` 的本次证据与关联写入 | 不重新编码之前所有 Reason requests；保留完整本次请求不等于常数字节成本 |
| 单工具结果 / owner 对账 | 单 Effect、当前 Processing、小回执及本次结果；phase 完整展示可 `O(I)` | 不因无关工具/历史工作增长而复制全 session；不得每次查询全部历史 invocation |
| 原命令恢复 / terminal outbox | 按 mutation/receipt 点查；待对账列表 keyset 分页 | 不通过恢复一次工具重放全 journal，不递归拿全执行树正文 |
| 历史加载 / 导出 / 显式迁移 | 明确例外 `O(N + H)` 总量，SQL cursor 分页并限制页字节 | 不把全历史 load 藏在 polling 或普通状态提交里；分页后全量收集仍须如实计峰值 |

正常推理仍需构造实际模型上下文，可能随当前可见历史增长；History UI 全量导出也有不可消除的内容成本。本 proposal 的边界是把它们从状态查询和每次状态更新中移出。降 poll 频率、压缩、Arc/cache、终态裁剪只改变部分常数或保留语义，不是替代方案。

### 8.1 活动记录并非天然 O(1)

未结算 required、Blocked/OutcomeUnknown、长期 Independent 委托、尚未 ACK 的 outbox 和原稿均可增长；合法大批次/大工具响应也会增加工作集。仅把终态移出索引不等于内存/数据库有界。

- 延用当前 required/optional 分开计数和字节配额、Receive `max_batch_size`、推理/dispatch/recovery 稳定预算；草稿使用自己的现行准入上限。不得在迁移时偷偷缩小定制 limits，亦不照搬 issue 中已过期默认数值。
- 增加明确的单 command/单 payload/单事务字节上限、当前 phase invocation 上限、每会话未结算 Effect/outbox 数量与字节预算、进程级在途读取/上传预算。具体数值需测量后定，不在无证据时编造“安全默认”。未配置必要部署上限不能宣称 bounded。
- 创建任务前预留其最小结算/回执空间；required 与控制/结算保留容量不被 Passive 占满。容量不足在生产者仍持有责任时返回可重试拒绝，不以丢 required 或清原稿腾空间。
- 新大结果超预留空间时保留 owner 责任或使用已满足保留契约的产物引用；磁盘永久耗尽仍可导致 ResourceBlocked，不承诺无限输入下持续接纳。
- 查询均以条数和字节双预算分页；单项超过页预算返回可单独读取的引用/显式超限，不能因“第一项必须返回”无界分配。原子 PublishSelected 超预算整批拒绝，Receive 可在既有序和 required 进展规则下形成更小批次。
- 结束/放弃只是退出活跃调度索引，不自动删除持久证据。队列积压、历史保留、allocator 驻留、请求组装分别计量，不混成“泄漏”。

### 8.2 保留与退休

| 数据 | 默认处置 / 删除门槛 |
| --- | --- |
| 未履行 Delivery、Unknown mutation/Effect、待 ACK terminal | 保留内容/可靠引用及精确身份，直至完成或明确授权处置；TTL 不静默删除 |
| canonical 历史与用户原稿 | 按现有用户可见语义保留；clear/撤回/切换不是通用 GC 授权；未确认原稿不能被当缓存清掉 |
| 精确 Reason checkpoint、响应/Act 证据 | 新写入离开热路径后保留；终态不默认裁剪。现有已裁剪的请求不能补造，迁移如实标记缺失来源 |
| event/命令去重与回执 | ACK 后可不再加载大对象，但去重依据保留；只有 producer/owner 不可重放退休水位覆盖断连发现/备份重现窗口才可删除或压缩成持久拒绝证据 |
| Closed/tombstone/Detached 委托 | 覆盖迟到消息与控制的重现期；删除 thread 不使同 ID 自动重建 |
| 无引用 Prepared blob | 过安全候选期、无在途原命令、事务内重验无 roots 后分页 GC；不得仅按墙钟 TTL 判断 |

首版允许小型历史证据长期增长，明确呈现容量压力；未形成退休协议之前不承诺数据库总量有界。归档必须是可核对搬迁：引用可读、授权不扩大、原回执可查，成功后才回收旧载荷。它是保留治理，不是让查询正确的前提。

故障域沿当前真实能力声明：本地 SQLite WAL + NORMAL 的既有合同覆盖 DB/WAL 完整的进程退出恢复，不扩展为掉电/磁盘损坏 RPO=0；Turso ACK 不自行证明远端 fsync/副本/RPO。备份恢复须对齐去重退休水位；缺失已接纳责任要报告 DataLoss，而非把“Blocked”当作数据仍在。

## 9. 迁移、切换与删除旧调用链

**推荐停写迁移，不做在线双写。** 先在隔离 fixture 完成新实现与一次性 importer，生产仍保持旧格式；批准上线窗口后由 SDK 停止全部旧执行/写入入口，核实工具 owner 及 pending 的处置。只停 TUI 不足以覆盖远端多实例、Cron 和独立子会话。

1. 记录冻结的构建/协议版本、数据版本与备份标识；独立保存数据库/WAL 一致备份或远端一致快照。保留用户数据与未知责任，不以“迁移前清空 work”换容易导入。
2. 未决 journal 先按原身份对账；仍 Unknown 的命令保留完整旧格式证据且阻止受影响责任自动推进。importer 不能把它判断为 NotApplied。外部 owner 不可停时必须保持投递重试责任，关闭接纳窗口明确返回未接纳，不能 ACK 后丢消息。
3. 一次性导入按 session/记录分页。旧 state_json 必须至少读取其本体，这是迁移成本例外；大单 session 使用流式解析/受控 spill 的资源层实现，不能要求整个库同时物化。工具代码不得新增上层磁盘依赖。
4. 将准确的 batch→work successor→budget→invocation/delegation 关系归并为 Processing。不能证明唯一关系、投影版本或处理状态的记录隔离为 LegacyUnknown，保存原 blob/相关原命令引用；不从 messages 是否存在推断 Satisfied，也不自动重跑。旧 request 为空区分已终态裁剪与缺失活动证据。
5. 对每个会话核验正文 hash、canonical IDs/flags/frozen、input 原稿/附件、逻辑 delivery 唯一键、全部未结算义务、request/response/invocation 身份、预算值、父子委托及回执链。旧 work IDs 在迁移证据内映射到 Processing/phase，旧命令保持原字节和 digest，不能先翻译再冒充原命令。
6. SQLite 在停写事务中提交新布局、格式门槛和权威切换；Turso 数据量超过托管事务限额时可分批填充不可服务的 staging 表，迁移 cursor/校验清单可重启，最后原子切换版本/准入标记。只有一个可服务权威；staging 是一次性迁移产物，不是兼容双写层。
7. 切换时移除旧 `state_json` 业务表/读写入口；保留的原始 LegacyEvidence 只供人工/迁移对账，不提供旧运行 API。更新所有 schema shape 检查、显式子行删除、只读历史解码和 Native/Turso 路由；不把迁移失败降级为另建空库。
8. 新执行协议/格式准入与 SDK、ACP 同步发布；旧二进制写打开必须拒绝。旧二进制不保证能读引用格式，新版只读历史入口继续工作。schema 版本无法隔离不遵守协议的旧 writer，部署必须实际停止/撤销其访问，不在 Peri 增加执行租约。

回退窗口明确分两段：**切换前、且无新格式接纳/副作用**，可恢复一致备份并恢复旧构建；**切换后产生任何新责任**，禁止只退二进制或恢复旧备份。必须停写，把新接纳、原稿、Unknown、副作用/去重水位完整迁移到目标版本并验收，或前向修复。反向迁移工具未实现/未验证时只允许前向修复。远端多 writer 部署同样适用，没有“本地可退所以云端也可退”的推论。

### 9.1 需要删除或改接的调用链

| 当前入口 | 切换目标 |
| --- | --- |
| `work.rs::WorkState/WorkSnapshot`、`work/query.rs::from_state`、`work/availability.rs` 全 map 投影 | 移除聚合运行接口；以 SQL 有界事实切片驱动同一领域规则 |
| `work/reducer.rs::reduce_work` 的整份接收/输出 | 拆为三个职责内的本次 transition；不是把所有 map 装入临时 struct 再调用旧 reducer |
| `sessions/work.rs::{READ_STATE,GUARD_STATE,UPDATE_STATE}`、`work/effects.rs::mutation_effects` | 小行 guard、typed write-set、事务 receipt；删除 JSON 全量比较/重写 |
| SQLite `session_data/work.rs`、remote `session_work.rs`/journal | 共同事实选择和事务计划，两 Adapter 保留自身真实失败/最终性处理，不各写一套业务规则 |
| `resources/gate_work.rs`、`resources/work.rs` | 窄 pending/resolve；不扩大 Unknown 的可写范围，不恢复旧聚合读取 |
| Agent `stages/work_ledger.rs`、`work_pipeline.rs`、`work_receive/reason/dispatch/recovery/boundary.rs` | 只拿当前 ProcessingSlice/Effect 和明确 evidence；屏障由领域行为端口封装，不让调用方自行拼 guard/多写事务 |
| `session/user_input_mailbox/{durable,staging}.rs` | Draft/Publication 查询与原命令回执；删从 WorkSnapshot 恢复整个会话的路径 |
| ACP `host/execution*.rs`、`continuation.rs`、资源 owner/child 恢复入口 | 新 protocol DTO、窄候选、显式 ActivationCause；删除 load→旧执行自动恢复耦合 |
| 内存 MQ、TaskManager、Transcript writer | 保留运行投影/通知；不重复确认 durable 接纳。已由事务写入的 canonical 行仅 mirror，writer 不再双写 |

实施时按符号重新搜索全部消费者及测试，不能仅改表就保留隐藏的 `load_session_work`。不留 deprecated shim、双格式长期运行或旧写函数兜底。按 DOC-UPDATE-001 在获批实施中更新受影响的 RCRA/Transcript/输入队列设计、schema/code-index 和测试路由；本轮未改变权威文件。

## 10. Tracer-bullet 实施与验收计划

以下全部为获批后工作，**本轮未执行**。每个 slice 必须贯通真实领域 Interface、SQLite 与 Turso Adapter、消费入口和失败回执；实验性新实现使用隔离数据，不开生产双写。若某 slice 无法独立生产切换，则先作为同一待发布变更的可验证垂直切片，完成全部门槛后一次切换。

| 阶段 | 可核对交付与完成门槛 |
| --- | --- |
| 1. 接纳→撤回/Receive | 固定 ID fixture，经真实 SessionResources 完成 Draft/Publish/Receive/TakeBack、message 引用和 Unknown；证明逐行查询/新 payload 写入，不生成 WorkState |
| 2. Reason→并发 Act→后继 | 精确 checkpoint、响应屏障、单调用结果、原子阶段交接与持续预算；调用方只获必要切片。既有生产行为完整运行，不删除失败语义以求通过 |
| 3. 子委托→终态投递→精确取消 | 父不存在、子独立恢复、乱序绑定、同名子会话、D1 迟到取消不影响 D2、Independent detach/Close、owner Unknown/barrier |
| 4. 控制/加载/重启→SDK 交接 | load 零模型/工具调用；显式新输入放弃旧 processing；服务恢复指定责任；新可靠消息合法激活；Pause/退出竞争、通知丢失、journal 冷恢复与 UI 原稿保全 |
| 5. 停写迁移→单权威切换 | 旧库/新库、本地/远端、失败中断/重启、旧二进制拒绝、回退窗口验证；删旧链和兼容 shim，完整行为矩阵通过后才安排真实迁移 |
| 6. 同构建性能验收 | 冻结最终源码/dirty 摘要、依赖、编译选项和二进制标识；计数先过再比较 CPU/heap/IO，不用旧 sample 数字充当前基线 |

### 10.1 完整行为、生命周期与故障矩阵

| 类别 | 确定性场景 | 断言与证据 |
| --- | --- | --- |
| 发布/幂等 | 同 event 不同 delivery ID、异内容、ACK 丢失、重复发现、不同 recipient | 每逻辑收件人一义务；冲突不覆盖；Accepted/Projected/Satisfied 分开 |
| 输入 | queued/selected/FIFO、大附件、撤回先赢/领取先赢、重发代际、整批发送中失败 | 原稿不丢、不重复发送，整批原子；Unknown 不生成新 publication |
| 投影/批次 | 已有 canonical 但 Pending；Receive 后崩溃；晚到消息；Passive 满队列 | 不抹除义务、不越界 ACK、required 有进展且不越配额 |
| Reason | Begin 提交前后 kill、发送前后故障、模型完成但响应未提交、旧 generation 响应 | 请求精确字节与身份不变；未持久 intent 零工具调用；不替代 Reason |
| Act | 两个以上并发工具反序返回、最后结果重复、取消前后返回、owner 响应丢失 | 每 invocation 结算一次，后继最多一次；Unknown 不重派；停止只收证据 |
| 原 mutation | journal/业务/ACK 三个窗口失败；延迟原提交与 FinalNotApplied 竞争 | 原 ID/digest/参数可恢复；最终封存后迟到提交无效果；拒绝无部分写 |
| 生命周期 | 真子进程写入后退出/新进程恢复、Pause/Resume/Close/Reopen、通知丢失与退出竞争 | 跨进程保留责任；旧 lifecycle 不激活新；SDK 唯一 attempt，无 Peri ownership |
| 子任务 | 父 runtime 消失、嵌套/Workflow、错误归属、同名不同 invocation、D1/D2、Independent | 子结果只进子 Inbox；父只收委托终态；精确取消、迟到不串会话 |
| 历史操作 | load/重复 load/clear/fork/rewind，旧非终态与真实 live 对照 | 历史加载模型/工具计数为 0；真实执行仍可用；没有无执行 loading；不丢原稿/未知证据 |
| 授权/引用 | grant 撤销、未知 codec/hash 错误、缺 blob、GC 竞态、owner 引用过期 | fail closed/可见 Blocked；不借父或新宿主权限；Accepted 前验证保留 |
| 容量/预算 | 大 batch、持续 Passive、未结算任务上限、重启后预算、外部结果超额 | 显式拒绝/保留生产者责任；不重置预算、不伪称 O(1) |
| 存储/迁移 | SQLite 锁冲突、Turso 托管批回滚/ACK 丢失、只读/未知 schema、旧库歧义、备份回退 | 相同领域结果和完整回执；不退本地库；LegacyUnknown/DataLoss 不伪造处理完成 |

沿现有 `peri-resources/tests/durable_work_contract.rs`、`tests/durable_work/journal_contract.rs`、remote work 合同以及 Agent `work_*_test.rs`、ACP execution/continuation、SDK 会话测试扩展公开行为验证；测试通过与否另记实施 issue。使用 patched cargo 定向执行并核对非零用例数；涉及 public Rust doc example 时跑 doc tests。真实 HTTP/进程生命周期使用受控本地 transport/模型 fixture，不把纯 reducer 测试冒充跨进程证据；真实 Turso/WASM 部署能力另列门槛，未运行就标未验证。

### 10.2 确定性复制 / IO 计数与性能门槛

构造固定 seed/ID/时间的合成会话，独立改变历史条数与旧 payload 总字节、当前 batch 大小、phase invocation 数、未结算数。相同当前操作在历史扩大时必须满足：

1. gate/availability/去重查询的历史 payload 读取、解析和复制字节均为 **0**；SQL 直接范围切片，检查查询计划及实际 visited/returned rows，防止 `LIMIT` 外观掩盖全扫描。
2. 普通状态提交的历史 payload 重编码/写入字节均为 **0**；命令 envelope、read-set/write-set、SQL 参数和返回字节只随本次目标变化。新 payload 的 hash/encode/write 字节单独计量，禁止与历史搬运混算。
3. 单工具结果不会因为同 session 的已结算 work 数增加而复制旧请求；事务剩余计数和后继数准确。Receive 的行数/字节受 B/P 上限，历史预览/完整 load 明确记录分页总量与是否最终全量物化。
4. 计数覆盖 Rust 编解码/clone、远端请求/响应、SQLite logical rows/bytes、WAL/page IO、in-flight buffers；B-tree 页分裂和日志写放大不承诺恒定物理字节，需记录后评估。
5. 同 toolchain、依赖、profile/features、输入数据、日志配置和硬件下比较前后构建；记录每个 binary hash/UUID、源码与 dirty 摘要。先在冻结基线上复现机制，再比较 CPU 时间差分、状态操作延迟分布、分配量/峰值、live heap、RSS/footprint 各自口径、DB/WAL 和远端往返。debug/release 不混比，旧样本不替代新基线。
6. 强制行为契约与复杂度门槛；收益百分比、绝对延迟预算由基线测量后与用户约定，不能先填“提升 N 倍”。没有 heap 生命周期和同构建对照，不宣称修复泄漏或保证内存下降。

## 11. 替代方案与最少待裁决事项

| 方案 | 优点 | 不能满足本次目标的原因 / 裁决 |
| --- | --- | --- |
| 继续窄读、缓存、降 poll、终态裁剪 | 改动小，可作现场止血 | 仍整体编码、生命周期混合；裁剪还改变证据保留。不是推荐终态 |
| 只外置 request blob，保留 WorkState | 能移走主要大正文，迁移较小 | 历史元数据仍全聚合、全局 revision 冲突和业务重复仍在；只能作为内部过渡，不能长期兼容双轨 |
| 每个 map 各一张表 / 泛用 event sourcing | 容易机械对应现结构 | 接口仍暴露全部复杂度；重建状态可能继续全历史成本，表/回放框架增加维护负担；不推荐 |
| 删除精确 Reason/Act 持久化，仅存消息与任务 | 存储/实现进一步简化 | 取消已批准阶段恢复与响应屏障保证；必须用户单独裁决，不能混进本次性能重构 |
| **本文方案** | 三职责模块、小行事务、复用现有存储和回执；保持全部可靠责任 | 需要一次存储/协议迁移和全生命周期验证；单次完整 provider 请求成本仍存在。**唯一推荐** |

### 11.1 实现简化与产品取舍分开

可以在**重构获批后直接实施**：合并重复字段/map 权威、消息/大载荷引用、记录级 revision、SQL 有界查询、消除全历史 COUNT/encode、以 Processing 游标代替阶段 successor 复制。它们不需要降低 RCRA 契约，但存储迁移和 wire 版本仍要整体批准。

以下仅保留 **三个关键决策**，给出推荐默认；没有批准前不执行生产变更：

1. **是否批准本文保留全部阶段保证的方案及其停写迁移/协议升级范围？** 推荐批准设计方向，实施另以冻结基线细化窗口；首版不做在线双写，切换后新责任存在时只允许前向修复或完整责任反迁移。该批准不等于现在授权操作真实数据库。
2. **证据保留与容量策略是否采用“引用保留优先，超额显式拒绝”的首版？** 推荐是：不为根治继续裁剪精确请求/未知义务/原稿；容量数字据测量定。若要求删终态请求、有限回执 TTL 或去掉精确 Reason/Act 检查点，须独立批准保留/恢复契约变化。安全替代至少保留 canonical response、dispatch intent、原 mutation/调用与授权、unknown 对账、明确 Blocked/Abandoned；即使只放弃精确模型重试，也不能删副作用屏障。本文不推荐该降级。
3. **clear 遇未决责任是否采用明确拒绝并保留证据？** 推荐是：普通 clear 只处理历史，不获得取消/放弃授权。若需要一键清空并释放所有旧处理责任，必须定义显式取消/放弃与 Independent/Unknown 的用户可见结果，不能通过清数据实现“已停止”。

load 不复活旧执行、显式新输入可放弃旧 processing 而保留证据、SDK 唯一执行权均已获批准，本 proposal 不重新提交这些为未决事项。服务恢复的触发者和工作关联按这些既定边界实现。

## 12. 本轮交付检查与未验证局限

本轮只进行了文档/源码读取与 proposal 写入；schema 为逻辑设计，类型为草图，故障矩阵和计量门槛均未运行。未证明两 Adapter 的新查询计划、Turso 批大小/真实网络与版本迁移可行性、WASM 闭包、heap 释放曲线或任何新构建性能收益；没有复用他人测试数字作为本轮 PASS。

交付前静态检查：当前 WorkState 的 19 个字段、WorkAction 的 28 个变体均有映射，9 个本地 Markdown 链接目标存在，无行尾空白，代码围栏配对；检查脚本 exit 0。它只证明文档覆盖/格式，不证明设计正确或实现通过。重新核对 HEAD 仍为上述值、暂存统计为空；共享工作树出现更多外部改动，包括 Cargo.lock，均非本任务写入。

并行 dirty 持续变化，后续实现须按符号重核；外部生产改动、止血、测试与提交不属于本任务。按 DOC-UPDATE-001，本次没有现行架构/接口变更，因此不修改 standards、design 或 code-index。
