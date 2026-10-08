# RCRA 消息、执行与历史会话加载

- 状态：**执行恢复已剥离，当前进程执行与历史加载保留**；验证范围见 active spec。
- 2026-10-07 用户裁决：撤销持久 Inbox/处理义务、WorkState、阶段检查点、执行恢复及其 journal/control 回执；保留历史会话加载、冻结上下文、普通续聊和当前进程执行。
- 实施契约：[执行恢复剥离计划](../../spec/issues/2026-10-07-remove-execution-recovery-plan.md)。此前的可靠执行恢复目标不再作为验收要求。

## 1. 数据与生命周期

会话 ID、历史消息、配置、frozen/inherited context 持久保存。消息队列、处理批次、阶段、调用关联、执行预算和待发送输入是当前进程运行态，不以完整 WorkState 或替代执行账本持久化。

RCRA 主路径为 `MessageQueue → Receive → Compact → Reason → Act → MessageQueue`。Receive 是唯一退出判断点；普通 Transcript 写入、压缩和消息 flags/projection 保持。进程退出终止该进程的执行责任，不承诺重启恢复模型请求、工具调用、子 Agent、审批或待发队列。

历史中的 started、未结束工具或子任务只表示历史事实，不是当前执行证据。未知外部副作用不改写为成功、失败或已取消；删除追踪不会自动停止外部任务。新输入不因已撤销的旧检查点被冻结。

## 2. 消息与当前执行

每个主/子/Workflow Agent 的队列归属明确，生产者根据可信会话地址路由，不以 root fallback 吞掉其他会话的结果。类型化消息策略、受众和当前 run 关联继续决定模型消费、展示和唤醒；不从正文或严重程度猜测消费语义。

用户输入和当前异步结果通过内存队列进入 Receive，再追加 Transcript。queue 接收不是磁盘持久接纳，不再使用 durable delivery/obligation/ACK 证明跨重启处理。当前进程可为重复通知去重，但不建立永久执行 journal。

当前执行的调用身份与模型 tool-call 身份保持可区分；子 Agent 展示归属使用模型 tool-call ID，不把临时调用身份当作历史恢复凭证。任务取消绑定当前调用/执行，不误取消后续独立调用。

## 3. 执行入口与控制

普通 ACP prompt 和当前输入队列能直接驱动 Agent，不要求 SDK durable admission ticket、entered evidence 或 settlement proof。Peri 保留进程内的并发/取消/关闭协调，不重建 Store 执行租约、owner CAS、dirty reset 或 sidecar 锁。

SDK 可在部署层管理跨进程执行，但其 alpha 协调器本轮不修改；Peri 不再提供 Work query/resolve、持久 control 或恢复结算协议，也不保留兼容 shim。旧客户端应得到明确不支持错误，而不是伪成功。

Pause/Stop 和输入取回依据当前 runtime/队列裁决；取消与关闭仍须诚实报告是否结清。执行预算按当前运行管理，不承诺跨重启累计。显式会话关闭意图与一般存储写入事务不属于撤销的阶段恢复机制。

## 4. History 加载

`session/list`、`session/load`、`session/resume`、历史 replay、fork 和手动续聊保留。按 ID 读取历史与元数据，按保存的环境身份验证可执行装配，保留冻结输入及继承快照；不能把当前启动环境覆盖保存环境。

加载创建新的 live runtime，只回放 canonical 内容，不恢复旧执行、owner invocation、Work 阶段或 cold child。History 读取和普通续聊不查询已删除的 Work/control 表。环境缺失时独立历史读取仍可用，不能伪造执行准入。

模型请求使用独立派生视图校验工具调用与结果配对：已有真实结果保持正文与身份，错位的独立结果在请求视图中移到对应调用之后；缺失结果补充明确说明“历史结果缺失，执行状态与外部副作用未知”的协议错误结果。该占位不代表工具执行失败、取消或未执行，不触发重跑，不写入 canonical 历史或数据库。紧随调用的用户消息若以内嵌内容块承载完整真实结果，保留内容；若结果块位于文本等内容之后，仅在请求视图中将结果块前移。其结果不完整或错位时明确报错，避免重复补位。重复调用身份、重复结果、无对应调用的结果以及结果早于调用同属无法安全修补的完整性错误，发送前明确报错并记录。`Raw` 内容在模型桥接处报错，不会形成 Anthropic 请求。普通 Reason、Full 摘要与预算估算共用这一视图，覆盖加载、fork、rewind 和当前进程中断后的续聊。

## 5. 存储与故障

删除 `session_work_state/events/receipts/commands` 和 `session_control_state/receipts`。保留会话、消息、环境绑定、OAuth、普通显式关闭及远端存储身份/普通 operation ledger。表结构与版本迁移以 active spec 和 canonical DDL 为准，不复建 main 的旧 dirty/lease 表。

消息写入/压缩使用真实事务；失败返回错误并记录，不能把未知提交报告为成功。一般存储完整性、只读访问、环境检查和当前写入排空不因移除执行恢复而取消。

旧恢复状态只在停止写入者后的版本迁移中移除；不将未完成 work 标成已完成，不修改历史正文/顺序，不通过 TTL 删除普通历史或扩展对象。

## 6. 验收

验证正常 prompt 的 Receive/Reason/Act 与工具结果入库、取消、活跃子任务与同名子 Agent 归属。验证历史列表/加载/切换/重放、frozen/inherited、fork 和续聊。

重启后旧阶段/工具/子 Agent 不自动执行，历史缺少终态不显示为当前 active；旧账本不阻塞新输入。迁移保留历史 rowid、flags、绑定、OAuth、关闭意图及未知扩展对象。本地和远端采用相同业务表语义。

实现状态、命令和结果仅记录 active spec，不把本文目标宣称为已实现。
