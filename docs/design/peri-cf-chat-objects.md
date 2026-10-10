# peri-cf 聊天对象契约与前端消费面目标设计

> 状态：**已批准目标设计；对象契约、view/消费面与「零持久化 + 按需 ACP 重放复原」架构基线已落地（本地验证），生产切换与部署端验收未完成。**
> 用户已批准三项裁决：(1) 升级策略：chat schema 2→3 / session schema 1→2，消费端拒绝旧 schema、版本化应用入口 + 短维护窗口、不长期双写；(2) **展示权威与持久化（2026-10-10 转向）**：无 Durable Object、零表、零持久化，Yjs 展示态不持久化，历史由 ACP `session/load` 重放按需复原，Turso/libSQL 的 Rust Store 是唯一跨 isolate 持久权威，会话命令走 WS 文本控制帧；(3) Entry 双身份：`entryId` 为对象 key、新增稳定展示 `id`，新数据两者默认同值。
> **已作废（superseded）**：本文原「DO 展示 checkpoint（`recordVersion:2` 单键索引 + `payload:<id>:<n>` 1 MiB 分段 + `record:v1-backup` 7 天回滚窗口）」「5.3.4 平台结构裁决」「四张表草案」及其容量数值，全部由裁决 (2) 取代，不再具有设计效力。它们只作为决策历史保留在 [P3 批准包](../../spec/issues/2026-10-10-peri-cf-chat-objects-p3-approval.md)，取代关系与证据见 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md)；不得按旧基线创建 DO、表、bucket 或绑定。
> 实施进度、验收边界与遗留项只写 `spec/issues/`；本文件不记录实施叙事。原文「本次交付仅文档」等历史表述已随实施落地作废。
>
> Scope：`@peri-code/sdk` 实时 Yjs 对象面（Entry / ContentBlock / ToolCall / Turn / Task / Plan / Interaction）与 `peri-cf` 的展示消费面、工具载荷可达性、无持久化解构下的复原与传输边界。
> 边界：不对齐 rcs 的同步机制、generation 换代算法与 selector 缓存层；聚合权威在 SDK 状态层，peri-cf 不建立第二套对象规则。
> 参考（结构对照，不引为本仓库事实源）：`/Users/konghayao/code/pazhou/remote-control-server/packages/chat-channel/src/`（该仓库不在本仓库内，路径保留绝对前缀）。
> 相邻设计：TUI 侧同类展示目标见 [TUI Chat 与 Tool Activity Workbench](tui-chat-workbench.md)；Session 与执行归属见 [Session 异步任务统一入口](session-async-tasks.md)、[RCRA 消息、任务归属与激活](rcra-message-activation.md)；代码定位以 [TS SDK 索引](../code-index/peri-ts-sdk.md) 与 [Peri CF 索引](../code-index/peri-cf.md) 为准。

本文只定义稳定对象契约与消费面边界，不记录实施批次、勾选清单或运行结果。当前实现、入口与动态 inventory 以代码索引和源码为准：`npm-packages/@peri-sdk/src/state/`、`src/view/`、`src/sync/`、`npm-packages/peri-cf/worker/chat/`、`shared/`、`web/chat/`。

## 1. 前提与已定基线

1. 对齐范围 = **前端消费面齐全 + 对象契约完整性**；不对齐 rcs 同步机制（generation 换代、selector 缓存层）。
2. 聚合权威落 SDK 状态层；peri-cf 不再实现第二套对象聚合规则。
3. 工具大载荷保持外置（`{id,bytes,preview}`，只在 isolate 内存）+ 浏览器可达（WS 控制帧 `op:"payload"` + 注入 reader），不持久化。
4. Entry 的展示身份、时间与公开错误收回 SDK 对象契约；应用保留真正属于应用的生命周期、导航和 UI 状态。
5. 已批准三项裁决（见文首状态块）为本设计的决策基线，本文件不得与它们相抵；其余设计内容为已对齐目标，未决小项集中登记于第 9 节。

## 2. 已确认事实与差距（基线：2026-10-10 核查快照）

下表为设计变更前的现状核查（只读源码；未运行构建/测试/迁移）。行号对应核查时快照；**可变定位权威在代码索引**（见文末「落位与维护」），不要求长期同步。

**历史标注（superseded）**：下表的对象契约差距（F1–F8、F11–F17）仍是本轮设计的有效输入；但 **F9/F10 描述的「DO 记录保存展示历史 / 重启由 DO 重放纯文本」路径已被裁决 (2) 取代**——新基线删除 DO 记录适配器，改为按需 ACP 重放复原（见 §3、§5.3）。保留原文只作历史对照，不得据此实现持久化。

| 编号 | 已确认事实 | 证据（仓库相对路径；rcs 为绝对路径） |
|---|---|---|
| F1 | WASM transport 接收 native JSON-RPC 帧并交给 transport，不在此聚合聊天对象。 | `npm-packages/@peri-sdk/src/transport/wasm-transport.ts:13-24,42-51` |
| F2 | peri-cf 过滤 sessionId/子 Agent 聊天块；512 KiB 限额只统计 assistant text，不覆盖全部 reasoning/tool。 | `npm-packages/peri-cf/worker/chat/session.ts:228-257` |
| F3 | SessionDocs 分派已有 projections；done 映射 completed/cancelled/error；completeTurn 另外 abort interactions。 | `npm-packages/@peri-sdk/src/state/session-docs.ts:45-55,91-140` |
| F4 | chat schema=2、session schema=1；Entry 没有时间/error 字段；已有 text/reasoning/tool_call 保序，reasoning 默认 summary。 | `npm-packages/@peri-sdk/src/state/doc-model.ts:14-29,66-111` |
| F5 | 工具参数/结果字段替换；结果含 contentText/rawOutput；终态工具拒绝不同状态覆盖；工具打断后的文本已会新建块。 | `npm-packages/@peri-sdk/src/state/chat-projection.ts:93-130`；`.../doc-model.ts:82-111` |
| F6 | TurnMachine.exit 会将该 turn 所有 assistant Entry 改为退出状态，未完成工具置 cancelled；cancelling 不再 writable。 | `npm-packages/@peri-sdk/src/state/turn-machine.ts:14-30,52-67` |
| F7 | >4096 UTF-8 bytes 的工具载荷外置，preview 最多 512 字符；全文只在内存，替换即删除旧版本；SessionDocs 有 readPayload，/view 不导出读取能力。 | `.../state/tool-payloads.ts:3-40`；`.../state/session-docs.ts:28-29`；`.../view/index.ts:1-8` |
| F8 | SessionView 已有 blocks/tool、tasks、plans、pendingInteractions、inputQueue；schemaVersion 只读取 chat；已有 ReadCache/SessionViewStore。 | `npm-packages/@peri-sdk/src/view/session-view.ts:5-87,135-178,272-300,304-362` |
| F9 | 应用在 peri-cf.state 保存 chat/running/executionBlocked/error/entryMetadata；应用生成展示 id/time；物化仅拼 text，flush 又将结果写回 record.messages（写回路径已随裁决 (2) 作废；应用运行元数据仍属应用）。 | `npm-packages/peri-cf/worker/chat/projection.ts:86-121`；`.../shared/sync-state.ts:5-23`；`.../shared/sync.ts:9-14` |
| F10 | 〔superseded，见本节历史标注〕DO 保存 SessionRecord 而非 Y.Doc 快照；restore 重放文本消息；重启遇 running 会阻断执行、标记错误，不恢复旧执行。 | `npm-packages/peri-cf/worker/types.ts:12-17`；`.../worker/chat/session.ts:75-90,317-319`；`.../worker/chat/projection.ts:23-57` |
| F11 | replica 校验 protocol/generation/sequence，snapshot 新建文档替换，delta/update 只 apply；无业务 schema 校验或迁移器；producer 已支持 generation 不同则全量 snapshot。 | `npm-packages/@peri-sdk/src/sync/session-doc-replica.ts:18-50,60-63`；`.../sync/session-doc-sync.ts:27,55-73` |
| F12 | 浏览器 ChatSync 经 replica/readSyncState 发布；ChatSession 消费面主要是 messages/phase/error；MessageList 渲染 message.content。 | `npm-packages/peri-cf/web/api/sync.ts:21-44`；`.../web/chat/session.ts:6-11,46-61`；`.../web/chat/MessageList.tsx:7-48` |
| F13 | SDK interaction 来自反向 ACP 请求，pending 无 turnId，完成后删除并保存 lastInteraction；peri-cf 直接取消权限请求、拒绝其他反向请求；声明能力仅 userInputQueue。 | `npm-packages/@peri-sdk/src/state/interaction-projection.ts:13-21,81-88,101-140`；`npm-packages/peri-cf/worker/chat/session.ts:215-217`；`.../worker/chat/bootstrap.ts:6-12` |
| F14 | SDK 同时写当前 info.plan 和按 turn 的 plansByTurn；tasks 已有轻量投影、数量限制和快照修复；usage 已写 Entry。 | `npm-packages/@peri-sdk/src/state/session-projection.ts:58-76,114-125`；`.../task-projection.ts:5-7,19-35,43-65`；`.../session-docs.ts:70-84` |
| F15 | rcs Entry/Block/Tool 有 kind、时间、error、visibility、resource、permissionId；聚合器在 Yjs transaction 内分派事件；退出统一收敛 Entry/权限/工具。 | `/Users/konghayao/code/pazhou/remote-control-server/packages/chat-channel/src/schema.ts:32-81`；`.../src/state/aggregator.ts:750-788`；`.../src/state/turn-machine.ts:40-80` |
| F16 | rcs 前端按工具块切段保序，错误挂本 Entry 末段；此为展示参考，不是必须复制的对象结构。 | `/Users/konghayao/code/pazhou/remote-control-server/packages/web-runtime/web/chat/chat-doc-to-structured.ts:158-243` |
| F17 | peri-cf 是共享 Bearer token 信任域，不是多租户；HTTP 复用鉴权；现有 publicError 只是拼接诊断并截断，不能视为脱敏。 | `npm-packages/peri-cf/CLAUDE.md:9-21`；`.../worker/api/app.ts:9-33`；`.../worker/api/http.ts:5-37` |

**差距判断：**不是 SDK 缺少聚合，而是字段/恢复不完整（F4/F7/F9/F10）、应用有损物化（F9）、载荷不可达（F7）、交互未接线（F13）。F4/F5 已有的保序机制保留；F9/F10 必须一起治理，否则只修当前页面、重启后仍丢对象。

**取代说明（superseded）：**上述「重启后仍丢对象」的治理路径已按裁决 (2) 改为**按需 ACP 重放复原**，不再依赖任何应用侧展示持久化（见 §3、§5.3）；F9 的 record 写回与 F10 的 DO 重放均不再作为目标形态。

## 3. 目标数据流

`*` 标记本轮新增或修改；未标记的同步链路沿用（不新增 generation 算法与缓存协议）。

```text
[Turso/libSQL Rust Store]  ← 唯一跨 isolate 持久权威（会话、消息、执行数据）
    ↑ ACP session/load：在响应之前以 session/update 重放持久历史
[Peri WASM / ACP 后端]（驻留在接受连接/请求的 isolate 内，随 isolate 回收丢失）
  ACP JSON-RPC notification / reverse request / response
    ↓
[SDK transport]
  解码、request/notification 分流，不聚合 UI 对象
    ↓
[peri-cf 应用：isolate 内 runtime 注册表 + worker/chat/session]
  会话绑定、sessionId/sourceAgent 过滤、预算、在途执行登记
  * 无持久化：先建 runtime 并订阅，再触发 session/load 重放（重放不丢）
  * 接好任务快照/config；reverse request 交 SDK 交互入口
    ↓
[peri-cf 应用：worker/chat/projection]
  * 删除 presentation/entryMetadata 聚合；只传生命周期元数据
  * 展示态只在内存；不写 record、不写 checkpoint、不落表
    ↓
[SDK 状态层：SessionDocs → projections / TurnMachine → DocModel]
  * 身份/时间/error、resource、permission 关联、终态收敛
  chat Y.Doc：entries + blockOrder/blocks + toolCalls
  session Y.Doc：session + turns + tasks/plans + interactions/queue
  外置 payload：{id,bytes,preview}；全文只在 isolate 内存，不进 Yjs
    ↓
[SDK SessionDocSync] → [peri-cf 无状态 WS 连接（仅 isolate 内存）]
  沿用 snapshot/delta/update、generation/sequence、交付信用
  二进制帧 = 文档同步；文本帧 = 应用控制帧
    ↓
[浏览器：SDK SessionDocReplica]
  apply；* 发布 view 前校验 schema pair，无浏览器迁移
    ↓
[SDK /view：readSessionView / SessionViewStore]
  * 完整对象 + 纯选择器 + readPayload(ref, reader, options)
    ↓
[peri-cf 应用：shared/sync-state]
  * 纯展示适配，不 flatten，不补造对象身份
    ↓
[浏览器：web/chat]
  Entry → Text / Reasoning / Tool / Resource（按顺序）
  会话/任务/计划/队列/交互面板

会话命令通道（WS 文本控制帧；HTTP 无会话亲和性，命令不能走 HTTP）：
  浏览器 → prompt / cancel / interaction 回答 / payload 读取（同一 WS 连接）
    → isolate 内 runtime → SDK live responder / SessionDocs.readPayload
    → command_result{ok,...} 回执；断线/超时即未确认，不自动重放

载荷读取：
  浏览器 SDK reader → WS 控制帧 op:"payload"
    → 校验 chat/tool/field/ref → isolate 内存全文（重放重建）
  不通过 Yjs 传全文，不经 HTTP 端点，不持久化。

交互命令：
  用户回答 → WS 控制帧 → SDK live responder
    → 原 ACP reverse request response
  浏览器不可写 Y.Doc，不经副本授权工具执行。
```

## 4. 对象契约

SDK 定义类型、校验与状态规则；应用 DTO 不成为另一份领域权威。

### 4.1 Entry

| 字段 | 契约 |
|---|---|
| `entryId` | SDK 对象身份及 Map key；投影生命周期内稳定。compact/rewind 是权威历史替换，不伪装为同对象增量。 |
| `id` | **已批准的双身份**：稳定展示身份，吸收旧 entryMetadata.id。新对象默认等于 entryId；导入保留旧 id；迟到 messageId 不改变 id。 |
| `messageId?` | ACP/历史源身份，可晚到，不替代稳定展示 id。 |
| `turnId` | message/tool 关联 turn；独立公开 system 事件可 null。 |
| `kind` | `message | tool | system`。常规 assistant 中的工具继续为 block，不复制独立 tool Entry；tool kind 仅用于真实独立工具记录。 |
| `role` / `status` | role ∈ user/assistant/system；status ∈ pending/streaming/completed/cancelled/error；组合须校验。 |
| `createdAt` / `createdAtSource` | 新增 `string|null` 与 source/observed/unknown。可信源时间优先；live 无源则 SDK 时钟只赋值一次；旧历史未知用 null，不伪造当前时间。 |
| `completedAt` | 新增 `string|null`；首次终态写入，重复退出不刷新时间。 |
| `error` | 新增 `PublicError|null` = `{code,message,diagnosticId?}`；仅公开错误，禁止 stack/cause/raw response/secrets。 |
| `tokenUsage?` | 保留，收紧为已支持的数值与请求元数据，不透传 provider 私有对象。 |
| `blockOrder` / `blocks` | Y.Array + Y.Map；view 输出有序数组，禁止按类型重排。 |
| `planEntries` | 只在 view 按 turnId 从 plansByTurn 派生，不在 Entry 重复持久化。 |

system Entry 只表示明确公开事件，**不是公开模型 system prompt**。历史 decoder 会跳过 system 且只识别 text/reasoning/tool_use（`npm-packages/@peri-sdk/src/state/protocol.ts:43-85`），扩展时保留该安全边界。

### 4.2 ContentBlock：保序与安全

| 类型 | 契约 |
|---|---|
| text | blockId、type、text:Y.Text；仅向末尾相邻同类型块追加。 |
| reasoning | 同上，加 `visibility = summary | hidden`。summary 可折叠展示；hidden 不进展示/复制/搜索。敏感正文必须在聚合前不写入，UI 隐藏不是访问控制。 |
| tool_call | blockId、type、toolCallId；同一调用只附着一次，后续更新只改独立 ToolCall。 |
| resource | 新增 resourceId/mediaType/name；只保存公开描述，不内联二进制、不自动访问任意 URI；不可读取时显示明确原因。 |

验收序列：`text:A → reasoning:R → tool:T1 → tool:T2 → text:B` 必须保序展示；T1/T2 结果晚到不重排；B 不追加进 A；实时与重放同规则。resource 解码须先核实真实 ACP 内容类型及安全标识，未知 provider block 不允许原样 JSON 透传。

### 4.3 ToolCall

1. Yjs 字段：`toolCallId, entryId, turnId, name, kind?, status, createdAt, completedAt, error, permissionId?`。
2. `status` ∈ pending/awaiting_permission/running/completed/error/cancelled；`permissionId` 关联 Interaction 的稳定字符串 id，不使用跨进程会重置的裸计数器作为浏览器命令身份。
3. `arguments|argumentsRef`、`result|resultRef` 分别互斥；未收到可都无；JSON null ≠ 未收到。
4. 参数/结果字段级 JSON 替换（不逐键 CRDT 合并）；保留外置 `{id,bytes,preview}`；result 保留 contentText/rawOutput，资源结果扩展须显式 schema。
5. 缺失 status 的更新不得默认推进为 running；终态不可回退；同终态可幂等补元数据，关闭 turn 后拒绝无关联晚到更新。
6. 检测同一 session 内 toolCallId 复用与跨 turn 错绑；若上游不保证唯一，建立显式 source ID → projection ID 映射，不覆盖历史工具。
7. 工具失败结果与公开 error 分离；不把结果正文或原始异常直接充当安全错误摘要。

### 4.4 Turn 与终态收敛

session.root 新增 `turns: Y.Map<TurnProjection>`；`session` info 中 activeTurnId/activeTurnStatus 保留为 SDK 同事务维护的当前索引，应用不得独立写入。

```ts
TurnProjection = {
  turnId, requestId?, status,
  startedAt: string | null, completedAt: string | null,
  error: PublicError | null, stopReason?: string,
  interactionStatus: "none" | "awaiting_permission" | "awaiting_input"
}
// status: accepting/running/cancelling/completed/cancelled/error
```

1. 不机械复制 rcs 的 failed/interrupted 枚举；宿主中断用 `status=error + error.code=execution_interrupted`；interactionStatus 与运行状态正交。
2. TurnMachine 是退出唯一权威；SessionDocs 协调指定 turn 的 interactions/responder 失效；所有入口绑定 turnId/requestId，不影响新 turn。
3. **只收敛未终态 assistant Entry**（契约变更，需先补测试）：已完成前序 Entry 保持 completed；退出错误归 Turn 与当前未完成/占位 assistant，不污染同 turn 已完成消息。
4. 未终态工具在任何 turn 退出时 cancelled；已 completed/error 保持；正常完成却残留工具应记录安全诊断，不伪造成功。
5. 未完成 interactions 置 expired 并撤销 responder；新 turn/取消/超时/断宿主复用同一入口；晚到回答不得授予权限。
6. 请求取消不等于已取消：沿用宿主关闭确认与执行阻断；取消请求失败可恢复活动状态，但已失效 interaction 不自动重活。
7. 终态幂等不回退；模型已完成后的保存/清理失败体现为应用 executionBlocked/error，不改写模型生成结果。
8. 两份 Y.Doc 无跨文档原子提交（`npm-packages/@peri-sdk/src/state/doc-model.ts:48-50`）；沿用批处理，在完整 frame apply 后发布 view；短暂缺引用显示 pending。

### 4.5 Session / Agent / Task / Plan / Interaction

1. Session 的模型/模式、goal/compaction、usage、inputQueue 暴露只读消费面；本轮不新增模型切换等命令。
2. tasks/plans **本轮消费**：任务状态/摘要/时间；计划按 turn 展示；沿用背景任务 revision/snapshot 修复规则。计划维持“当前 info.plan + plansByTurn”由同一 SDK projection 维护，不再复制到各 Entry。
3. Agent 做语义映射而非复制 rcs：子 Agent 活动由 TaskView 展示；当前会话运行状态由 TurnView 展示；WASM 实例资源/宿主可用性仍属 peri-cf。需要独立 Agent 对象时须先确定真实来源，禁止由 running 布尔值虚构“Agent 在线”。
4. **interactions 纳入目标消费面**：session doc 新增 `interactions` Map——稳定 id、kind、turnId、sessionId、toolCallId?、status `pending|resolved|expired`、createdAt/resolvedAt、公开问题/选项与安全表单 schema。
5. `pendingInteractions` 改为 view 从 interactions 派生；工具 permissionId 与之关联。数字 callback ID 可留 SDK live 内部；网络契约使用不碰撞的稳定 ID。回答正文、批准令牌、resolver/AbortController 不入 Yjs；结束只保留 outcome 摘要。
6. 表单契约保留必要 type/required/enum 约束；首版明确有限支持类型；不支持时提示无法回答，不误发 accept。interaction 记录须有获批的数量/保留预算，不建立无限历史。
7. 重放/历史只恢复展示，pending 失效；不恢复可回答性（isolate 回收后无 live responder）。完成 reverse request → UI → 回答 → ACP response → 取消/超时闭环后再声明 capability，不绕过当前默认拒绝。

## 5. DocModel diff、schemaVersion 与兼容

### 5.1 字段级变更

`+` 新增，`~` 修改，`-` 删除；未列出现有字段保留。

| 位置 | diff |
|---|---|
| chat.root | `~ schemaVersion: 2→3`；entries/entryOrder/toolCalls 容器保留。 |
| entries[*] | `+ id, kind, createdAt, createdAtSource, completedAt, error`；`~ role/status` 封闭类型；`~ turnId` 允许公开 system 的 null；保留 messageId/tokenUsage。 |
| blocks[*] | `+ resource`；`~ reasoning.visibility` 严格枚举与公开正文约束；text/tool_call 物理结构不变。 |
| toolCalls[*] | `+ createdAt, completedAt, error, permissionId`；entryId 从隐式写入提升为契约；`~ status` 精确映射；arguments/result 与 ref 互斥保留。 |
| session.root | `~ schemaVersion: 1→2`；`+ turns, interactions` 两个 Y.Map；tasks/taskOrder/plansByTurn 保留。 |
| session.root.session | 保留 activeTurnId/activeTurnStatus；`~ activeTurnInteractionStatus` 由未决 interactions 派生；`- pendingInteractions, lastInteraction` 持久字段（view 提供同名派生查询）。 |
| 应用 peri-cf.state | `- entryMetadata`；保留 chat/running/executionBlocked/error 的应用语义（见第 8 节），且只在 isolate 内存，不持久化。 |
| SDK view | `+ schemaVersions:{chat,session}`，移除含混单值 schemaVersion；新增完整 tools/turns/interactions 读面；公开 API 破坏性变更，SDK 与应用协调发布。 |
| 应用展示持久层 | **〔superseded，见文首状态块〕** 原「DO SessionRecord + `recordVersion` + display checkpoint」基线作废：**无持久化**，无 record/表/分段，展示态按需由 ACP 重放重建（见 §5.3）。Yjs schemaVersion 与同步 protocol 仍分别管理。 |

全文载荷、浏览器展开/复制状态、认证信息、网络请求、宿主执行恢复状态均不进 Yjs。

### 5.2 升级策略（已批准；应用侧持久化迁移部分已作废）

依据 F11：replica 无迁移。不能只改版本数字，也不能让浏览器补 Y.Map 字段充当迁移。

1. 固定新 pair `{chat:3, session:2}`；SDK view 入口提供纯 schema 校验：空副本 = not-ready；完整但不支持的 pair = UnsupportedSchema，不伪装为空历史。
2. **〔superseded，见文首状态块〕** 原「旧 record 由应用解析交 SDK import 构造全新文档、保留旧 id/time、标记 legacy/incomplete」不再适用：新基线**不持久化展示态**，没有应用侧旧记录可转换；重放直接来自 Rust Store 的 canonical 历史。
3. **〔superseded〕** 原「import 重建索引、不得把 update apply 到已初始化 root」属 SDK import 语义，仅在显示材料确需外部导入时保留（当前生产路径无调用方）；重放路径不经过它。
4. **〔superseded〕** 原「新文档由新 SessionDocSync 承载、不在活跃 doc 原地改写 schema」中的文档换代语义保留为 SDK 能力；应用侧不再有"记录转换后换 doc"的步骤。
5. 浏览器先 apply、发布 view 前校验；不支持即废弃副本、停用命令、提示刷新；不循环 resume 不支持的版本。
6. 新浏览器校验不能保护旧 JS；采用**版本化应用 HTTP/WS 入口**隔离新版消费面，旧入口在切换后明确升级错误/关闭连接，避免旧客户端静默丢块。SDK 同步 wire 不变；具体路由名属实施设计。
7. 发布先停止接纳新 turn、等待在途结算；未确认关闭的会话保持 blocked，不为升级强制重跑。**〔superseded 半句〕** 原「完成 DO 记录转换」步骤删除；旧 socket 关闭后直接开放新版入口。
8. 默认不长期双写/双 producer；若要求零维护窗口，须另审有限期双版本服务与清理条件。

### 5.3 展示复原与载荷边界（新基线：零持久化）

**取代声明（superseded）**：本节的旧版本（SDK display checkpoint 取代 DO 纯文本、DO 记录结构未决、分段/R2 选型、容量与 GC 预算、写入顺序、回滚备份）**全部作废**，由 2026-10-10 用户裁决取代；对应的 P3 批准决定（`recordVersion:2`、`payload:<id>:<n>` 1 MiB 分段、`record:v1-backup` 7 天窗口、四张表草案）只作决策历史，见 [P3 批准包](../../spec/issues/2026-10-10-peri-cf-chat-objects-p3-approval.md) 与 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md)。不得据旧版本创建 DO、表、bucket、绑定或分段存储。

1. **无 DO、零表、零持久化**：不注册 Durable Object、不配置 `durable_objects`/`migrations`，不使用 DO 存储、socket attachment 或 Hibernation 事件入口；不新增 R2/KV/D1 等替代持久层。展示态（Yjs 文档、运行元数据、连接状态、载荷全文）只存在于 isolate 内存。
2. **持久权威唯一**：Turso/libSQL 的 Rust Store 保存会话、消息与执行数据；ACP `session/load` 在响应**之前**以 `session/update` 重放持久历史（`peri-acp/src/host/requests/session_restore.rs`），应用必须先建立订阅/runtime 再触发重放，重放不得被丢弃。
3. **按需复原**：`GET /api/v2/chats/:id` 与 WS 连接鉴权成功后触发复原（重放重建投影后返回同一 SyncState DTO）；`GET /api/v2/chats/:id/resources` 与 `GET /api/instances` 是 peek，不触发复原、不启动 Host。
4. **执行归属与丢失语义**：执行驻留在接受连接/请求的 isolate；isolate 回收即在途执行丢失且**不复原**，不得由展示历史重造 Rust 工作义务。长时保活只依赖平台尽力而为的机制（WS 消息 await 与升级请求的 `waitUntil`），没有平台保证，部署端才能验证。
5. **载荷全文**：工具参数/结果全文只在 isolate 内存，经 WS 文本控制帧（`op:"payload"`）按需读取，不通过 Yjs、不经 HTTP 端点、不持久化；重放后由历史重建。若上游历史不含完整载荷，则读取不可用必须显式失败，不得伪装成功（重建覆盖范围待验证）。
6. **命令通道**：prompt/cancel/interaction 回答/payload 读取一律走 WS 文本控制帧（HTTP 无会话亲和性）；旧 HTTP 命令/载荷端点返回 426。命令结果只在服务端明确答否时为确定失败；断线、超时、未同步视为未确认，调用方必须阻断重跑。
7. **端口化边界**：平台能力经显式端口注入，不在业务内直接触达 CF 原语——`SocketHost`（WS 升级、保活、时钟）在连接层，`RuntimeDeps`（`startTransport` / repository）在运行期注册表，SDK 宿主经 `moduleFactory` / `ports` / `onLifecycle` 注入。测试以内存实现替换这些端口，不依赖 workerd 专有行为。
8. **不做的事**：不恢复旧 Agent 执行、不做跨 isolate 接管、不把观测当存活探测、不为回滚保留任何展示持久层（转向时未部署，无旧数据可迁移）。

## 6. SDK view 与安全 readPayload

### 6.1 SessionView 扩展

1. 暴露 `schemaVersions`、完整 entries、独立 toolCalls、turns、activeTurn；Entry 带 id/time/error/kind/ordered blocks。
2. tool block 可保留已有 tool 快照关联，但只是独立 ToolCall 的只读派生，不成为写源。
3. 暴露 interactions 与派生 pendingInteractions；保留 tasks/plan/plansByTurn/inputQueue/session。历史缺失、不支持、暂不可观测不得全部伪装为空数组。
4. 纯选择器：`selectEntry`、`selectToolCall`、`selectTurn`、`selectPlanForTurn`、`selectPendingInteractions`；只做查询，不新增 selector 缓存/订阅机制。
5. 孤立工具引用/未知对象显示诊断或 unavailable，不静默隐藏整块；只补现有 ReadCache/SessionViewStore 的必要失效依赖，不改缓存设计。

### 6.2 readPayload 契约与安全边界

`/view` 导出浏览器安全类型与异步 helper：

```ts
readPayload(ref, reader, { signal, maxBytes })
  → Promise<{ id: string; bytes: number; json: string }>
```

1. `reader` 为注入的 transport-neutral `ToolPayloadReader`；服务端 adapter 可使用 SessionDocs.readPayload，浏览器 adapter 经 peri-cf 的 **WS 控制帧**（`op:"payload"`，结果帧带 `{id,bytes,json}`）。**不 re-export SessionDocs，不假设浏览器能读服务端 Map。**
2. 请求绑定 `chatId + toolCallId + field + ref.id`；服务器验证归属，随机 ref id 不是授权。SDK helper 校验返回 id、UTF-8 bytes 与合法 JSON；应用拒绝跨会话或已过期 ref 的迟到响应（旧响应不覆盖新选择）。
3. 控制帧经已鉴权的同步连接（首帧 Bearer），token 不入 URL；全文与 preview 不记录日志、不入持久浏览器缓存；JSON 转义显示，不当 HTML 执行；resource URI 默认不自动访问。
4. 默认按需展开读取，不全量预取。WS 控制帧是浏览器唯一读取路径；服务端只读 isolate 内存当前版本（重放重建），不含持久 repository；不得以任意 URL、文件路径或 provider endpoint 兜底。
5. 明确失败码：unauthorized、unavailable/stale、too_large、integrity_error、aborted、transport_error；未知/越界 ref 不泄露其他会话存在性；stale 不替换成最新版本内容。
6. 保留 4096-byte inline 分界与投影接纳时的 inline+外置总量、reasoning、资源与累计版本预算；预算只约束 isolate 内存与单次传输，不是持久容量（F2/F7）。
7. 建议评审起点（提案，非实测/平台上限）：浏览器单次内存展示 ≤1 MiB、并发读取 ≤2；更大结果需另行设计流式路径（当前未实现，超限显式拒绝）。单 payload 与 isolate 总量上限须与 64 MB WASM 预算一并压测（未决小项）。
8. 不允许“已成功接纳但永久无法读全文”：接纳上限必须与可读路径一致；重放后无法重建的载荷必须显式 unavailable，不伪装成功。
9. preview 也可能含秘密：公开数据过滤须发生在投影之前，过滤后 bytes 对应实际公开 JSON；hidden reasoning 不写浏览器可见正文；UI 折叠不代替安全边界。

## 7. 前端消费路径

### 7.1 shared/sync-state → web/chat

1. `shared/chat.ts`：消息 DTO 带 id/entryId/kind/status/time/error/blocks；SyncState 带 tools/turns/session/tasks/plans/interactions/queue；SDK 领域类型与应用 DTO 显式边界转换。
2. `shared/sync-state.ts`：删除 flatten 与 entryMetadata 回填；复制文本仅点击时派生，不做第二份同步/持久化正文。
3. `web/chat/session.ts`、`useChat.ts`：传完整状态；HTTP 首屏（触发复原）与 WS 实时同 DTO、同身份，避免接通后换消息；命令只经 WS 控制帧端口（`ChatCommandPort`），HTTP 客户端只读。
4. `MessageList.tsx`：Entry 卡片内按 blockOrder 逐块渲染 Text/Reasoning/Tool/Resource；text 沿用现有 Markdown 安全设置；summary reasoning 可折叠；tool 显示状态/preview/全文入口；resource 显示安全描述与可用性。
5. React key 用 Entry.id/blockId/toolCallId，不用数组位置；工具晚到更新原卡片不移动；纯工具/纯 reasoning/仅错误 Entry 不提示“没有文本”。
6. Entry 错误显示一次，Turn 错误独立表达；复制默认仅可见 text，不含 hidden reasoning、工具秘密或原始错误。
7. 展开/loading/重试/复制反馈/滚动/表单草稿留浏览器；切会话或登出取消旧请求、关闭旧连接并作废在途命令（未确认不重放），拒绝旧响应写入新会话。

### 7.2 本轮消费取舍

| 对象 | 本轮目标 | 不在本轮 |
|---|---|---|
| Entry/Block/Tool | 保序展示、状态/error/usage、全文读取 | 工具执行重放、自动执行资源 URI |
| Session/Turn | 状态、模型/模式等只读信息、等待/失败说明 | 执行恢复、配置编辑 |
| Task/子 Agent | 状态/摘要/时间及不可观测提示 | 子 Agent 控制台、任务重启 |
| Plan | 当前与按 turn 计划 | 浏览器直接修改计划 Y.Map |
| Interaction | 权限/问题展示与安全回答闭环、超时/取消/重复回答 | 自动批准、猜测回答不支持表单 |
| InputQueue | 队列/执行请求只读关联 | 新增队列调度规则 |
| Resource | 公开描述、支持类型的安全读取/不可读说明 | 任意远端抓取、附件托管平台 |

完整性验收口径：每类对象都有“源→投影→view→DTO→组件→历史/终态”检查项；上游未启用能力显示 unsupported，不以空数组冒充“没有事件”。interactions 可晚一个实施阶段，但未闭环前只能称“聊天对象对齐”，不能称本目标全部完成。

## 8. 应用层 `peri-cf` map 收敛去向

| 当前字段 | 去向与理由 |
|---|---|
| entryMetadata.id | SDK Entry.id；应用不再生成对象展示身份。 |
| entryMetadata.createdAt | SDK Entry.createdAt + source；导入保留旧值。 |
| entryMetadata.error | SDK Entry/Turn PublicError；旧字符串经安全转换，不直接信任。 |
| chat | 暂留应用 map：来自应用会话目录（Turso）的 Chat DTO；明确其与 ACP session info 的来源，不混为一份实时元数据。 |
| running | 暂留应用**内存**：宿主执行/结算流程仍占用，不等于 activeTurnStatus；随 isolate 回收消失，不持久化、不跨 isolate 一致。 |
| executionBlocked | 留应用**内存**：宿主停止/执行资格判断，不由 SDK 展示投影推断；isolate 回收后不保留。 |
| 顶层 error | 留应用**内存**：仅启动、清理、连接等公开运行错误；生成失败归 Entry/Turn。 |

收敛采用**单写新权威**：展示身份/时间/错误由 SDK Entry 提供，应用不再发布 entryMetadata；旧 record/metadata 的 import 输入路径已随裁决 (2) 作废（无持久化可迁）。版本切换期旧客户端由版本化入口拒绝。

## 9. 实施里程碑（依赖顺序、各阶段验收判据）

以下为实施拆分范围与各阶段验收判据；进度事实源为对应 `spec/issues/`，本设计不记录勾选状态与实施叙事。依赖：P0 → P1 → P2/P3；P4 依赖 P1/P3；P5 依赖全部。P3 的存储判据已按裁决 (2) 改为「零持久化 + 重放复原」（见 §5.3），原 DO 记录验收判据作废。

| 阶段 | 范围 | 独立验收判据 / 须先补的测试 |
|---|---|---|
| P0 契约冻结 | 批准身份规则、版本 pair、公开数据政策、interaction 支持类型与预算；建立 ACP fixtures。 | **先补失败样例**：text→reasoning→tool→text 交错、同 turn 多 Entry 退出、晚到更新、重放缺事件、丢失 payload；测试先揭示现缺口；**不得引入任何展示持久化或 CF 专有原语**。 |
| P1 SDK 对象权威 | schema/projections/TurnMachine 扩展；显式错误与时钟注入；统一关联身份；重放路径（`SessionDocs` 消费 ACP 历史通知）。 | **先补 SDK 契约测试**；实时与历史生成相同有序对象；重复终态不改时间；取消不污染已完成 Entry；无 system prompt/secret 泄漏；不依赖 peri-cf metadata。 |
| P2 view + 展示消费 | 新 SessionView/纯选择器；shared DTO；块组件与任务/计划/会话/队列面板。 | **先补 DTO/组件测试**；纯工具、hidden reasoning、仅错误、工具更新、未知对象均有明确结果；HTTP/WS 同身份；现有缓存回归通过，不新增缓存架构。 |
| P3 复原与载荷通道 | `/view` reader 接 WS 控制帧、按需 ACP 重放复原、版本门禁；**零持久化**（原 DO 记录/分段/回滚方案已作废）。 | **先补边界测试**：越权/跨 chat ref、字节不符、超限、取消、并发；重放重建后既有全文可达（或显式 unavailable）；旧客户端明确升级；不恢复旧执行；不得出现任何持久化写入。 |
| P4 interactions 接线 | reverse request → SDK projection/responder → DTO/表单 → WS 控制帧命令 → ACP response；接好 tasks snapshot/config 能力源。 | **先补往返测试**：允许/拒绝、问题提交、timeout、turn 退出、重复/迟到/跨 session 回答；重连/重启后全部失效；无 callback/不支持表单默认拒绝；能力声明最后开启。 |
| P5 真实闭环与清理 | workerd/WASM + 无状态 WS + 浏览器验收；部署端验收（免费档 CPU、长时保活、远程 Turso）；移除应用旧 metadata/flatten 写回。 | 真实交错工具全文、刷新/重连、断宿主、isolate 回收、容量与安全用例通过；记录未覆盖的 provider/resource 类型，不宣称全量支持。 |

## 10. 未决问题与风险

### 10.1 未决问题（本设计不裁定，保留待批）

1. **interaction 表单支持范围**：首版支持的字段类型/校验/多步形态，及不支持的降级交互（仅提示 vs 阻断）。
2. **resource 类型范围**：可读资源的 ACP 来源、安全标识、允许的读取路径与不可读展示契约。
3. **payload 字节/内存预算**：单 payload、isolate 内总量、浏览器内存展示、并发读取的具体数值；本设计的 1 MiB/并发 2 仅为评审起点，且不再是持久保留期/GC 问题。
4. **公开数据过滤政策**：工具 arguments/result/preview 的公开脱敏规则、错误白名单 PublicError 的字段边界与日志安全码。
5. **Agent 语义映射**：TaskView/TurnView 与 rcs 独立 Agent 对象的最终映射；若需独立 Agent 对象，其数据源与生命周期须另行审定。
6. **SDK checkpoint API 去留**：`SessionDocs.fromCheckpoint/exportCheckpoint` 已无生产消费方（零持久化后不再需要展示材料导入导出），删除与否待用户裁决；未裁决前不得为它保留应用侧调用点。

### 10.2 风险

| 风险 | 影响与缓解方向 |
|---|---|
| **免费档 10 ms/请求 CPU vs WASM 冷启动** | 本机实测编译/实例化 10–66.6 ms 量级，免费档单请求预算 10 ms → 部署到免费档很可能 1102。用户已知情并要求按文档结论推进；部署端第一项验收，必要时改用付费档或把 Host 启动移出请求路径（本设计不预留持久原语）。 |
| **无 DO 后长时执行无平台保活保证** | WS 消息处理里的 await 与升级请求 `waitUntil` 只是尽力而为；isolate 何时回收由平台决定。部署端验证长时 turn 的存活；失败时表现为连接中断 + 执行丢失（不复原），不得假装成功。 |
| **HTTP 无会话亲和性 / 多 isolate 内存态分叉** | 命令必须走 WS；同一会话可能在不同 isolate 各有一份内存态，客户端以自己的连接为准。不得把某一 isolate 的状态当作全局事实。 |
| **每 isolate 一份 64 MB WASM 预算** | 同一 isolate 内并发执行被容量预留拒绝（`shared/runtime-limits.ts`、`worker/wasm/budget.ts` 的 `WasmCapacityError`），经 WS 命令失败码显式回执并置本 isolate 的执行阻断；用户可见的并发上限必须显式，不静默排队。 |
| **重放覆盖范围** | ACP 重放是否携带完整工具参数/结果与 reasoning 未验证；不可重建的内容必须显式 unavailable，不得伪装空历史。历史缺可信时间时保持 null（timestamp 行见下）。 |
| timestamp 来源 | 核实 ACP/历史是否含可靠时间；否则 observed/unknown，不承诺重放推回原事件时间。 |
| 工具名称/permissionId/资源来源 | 以真实 ACP fixtures 验证 ID 唯一性与事件先后；无源字段用 unknown/null，不猜关联。 |
| interaction 扩面的权限风险 | 有限表单类型、服务端答案校验、会话/turn/live identity 绑定、幂等回答、失败默认拒绝；UI 完成不代替能力授权。 |
| payload 生命周期与过期 ref | 重放后旧 ref 不得返回新内容；不可读必须显式失败；仅内存不等于内存有界（预算见 §10.1.3）。 |
| 秘密经 preview/error 泄漏 | 现有 publicError 只是拼接截断（F17）；本轮触及的错误出口改为白名单 PublicError，日志只留安全 code/id；明确工具公开载荷过滤政策。 |
| 无持久层后的写入失败面 | 零持久化消除了 checkpoint/分段写入的一致性问题；新失败面是 isolate 回收导致的内存态丢失与命令未确认（见上两行），必须按"未确认即阻断重跑"处理，不伪装成功。 |
| 任务与 Agent 语义差异 | 前端消费 SDK 实际拥有的 Task/Turn/宿主状态；独立 Agent 同构需求另审数据源，不能从 running 布尔值造对象。 |
| “齐全”的验收范围 | resource 可读类型、interaction 表单类型与 Agent 语义映射确认前，不支持能力必须可观察，不计作已完成。 |
| **部署端未验收** | 本地 workerd/测试不能证明平台 CPU、保活、真实 Turso、真实浏览器；状态块必须如实标注「已落地（本地验证）/未验证（部署端）」。 |
| **未决配置与孤儿 API** | 远程 Turso DB 地址与令牌尚未提供（部署前必须补齐）；SDK checkpoint API（`SessionDocs.fromCheckpoint/exportCheckpoint`）已无生产消费方，去留待用户裁决（§10.1.6）。 |
| **事实核查纪律** | 向用户报告时区分「已核对（官方文档/仓库代码/本机实测）」与「推断」；未核实的内容不得写成既定事实，无法与最终代码对齐的表述单列待核实清单（见 [无 DO 转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md) §5）。 |

## 落位与维护

1. 本文正式位置为 `docs/design/peri-cf-chat-objects.md`，并按 `docs/design/README.md`「已批准目标设计」登记（进度事实源指向对应 active issue：对象契约见 [实施 issue](../../spec/issues/2026-10-10-peri-cf-chat-objects-implementation.md)，无 DO 转向见 [转向 issue](../../spec/issues/2026-10-10-peri-cf-no-durable-object.md)）。
2. 实施完成后只保留稳定结果；动态 inventory、固定源码行号与命令输出不入本文，定位信息进 `docs/code-index/peri-ts-sdk.md` 与 `docs/code-index/peri-cf.md`。
3. 文件改名/合并/删除按 DOC-LINK-001 同步根与模块路由、standards、code-index 与 active spec，并运行本地 Markdown 链接检查。
