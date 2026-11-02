# peri-cf 聊天对象契约与前端消费面目标设计

> 状态：**已批准目标设计；对象契约与展示消费面仍有效，传输与控制实现已随服务端执行更新（2026-10-26）。**
> 现行实现以 [Peri SDK Chat](peri-sdk-chat.md)、[服务端 Agent](peri-cf-server-agent.md) 与
> [Chat 前端](peri-cf-chat-ui.md) 为准：Chat 控制面由 SDK `ChatManager`/`ChatService`/`PeriInstance`
> 承担，浏览器经 `ChatReplica` 消费 WebSocket + Yjs 只读副本，Action 经 HTTP 提交，载荷经 HTTP 读取。
> Worker/DO、`SessionDocs`/`SessionViewStore`、WS 文本控制帧与 `/api/v2/*` 均为已删除的历史实现，
> 完整记录由 Git 保留。
>
> Scope：`@peri-code/sdk` 实时 Yjs 对象面（Entry / ContentBlock / ToolCall / Turn / Task / Plan / Interaction）与 `peri-cf` 的展示消费面、工具载荷可达性、无持久化解构下的复原与传输边界。
> 边界：不对齐 rcs 的同步机制、generation 换代算法与 selector 缓存层；聚合权威在 SDK 状态层，peri-cf 不建立第二套对象规则。
> 参考（结构对照，不引为本仓库事实源）：`/Users/konghayao/code/pazhou/remote-control-server/packages/chat-channel/src/`（该仓库不在本仓库内，路径保留绝对前缀）。
> 相邻设计：TUI 侧同类展示目标见 [TUI Chat 与 Tool Activity Workbench](tui-chat-workbench.md)；Session 与执行归属见 [Session 异步任务统一入口](session-async-tasks.md)、[RCRA 消息、任务归属与激活](rcra-message-activation.md)；代码定位以 [TS SDK 索引](../code-index/peri-ts-sdk.md) 与 [Peri CF 索引](../code-index/peri-cf.md) 为准。

本文只定义稳定对象契约与消费面边界。代码入口：`npm-packages/@peri-sdk/src/{state,view,sync}/` 与 `npm-packages/peri-cf/frontend/remote/`。

## 1. 前提与已定基线

1. 对齐范围 = **前端消费面齐全 + 对象契约完整性**；不对齐 rcs 同步机制（generation 换代、selector 缓存层）。
2. 聚合权威落 SDK 状态层；peri-cf 不再实现第二套对象聚合规则。
3. 工具大载荷保持外置（`{id,bytes,preview}`）+ 浏览器可达（注入 `ToolPayloadReader`，经 HTTP 读取全文），只在运行侧内存，不持久化。
4. Entry 的展示身份、时间与公开错误收回 SDK 对象契约；应用保留真正属于应用的生命周期、导航和 UI 状态。
5. 已批准裁决为本设计的决策基线，本文件不得与它们相抵；其余设计内容为已对齐目标，未决小项集中登记于第 9 节。

## 2. 目标数据流

现行实现为「服务端执行 + 浏览器只读副本」；以下是当前结构的对象流。

```text
[Turso/libSQL Rust Store]  ← 唯一持久权威（会话、消息、执行数据）
    ↑ ACP session/load：在响应之前以 session/update 重放持久历史
[peri-wasm ACP Host]（Bun 服务进程内，每次激活一个；进程退出不恢复旧执行）
  ACP JSON-RPC notification / reverse request / response
    ↓
[SDK PeriInstance / Session]
  绑定、输入队列、取消、关闭、历史分页与交互；不聚合 UI 对象
    ↓
[SDK ChatService]
  按绑定代次发布 ChatFact：seed-config / history-* / notice / input-* /
  task-snapshot / interaction-* / turn-* / run-state / closed
  外置 payload：{id,bytes,preview}；全文只在运行侧内存，不进 Yjs
    ↓
[SDK ChatView] → ChatDocument（一对 Y.Doc：chat/session）+ SessionDocSync
  二进制 HELLO/SNAPSHOT/UPDATE/ACK；交付信用与有界背压
    ↓
[WebSocket /api/chats/:chatId/sync] → [浏览器 ChatReplica]
  只读 apply；校验 protocol/generation/schema，断线用 HELLO resume 补齐
    ↓
[SDK /view：readSessionView / readPayload]
    ↓
[peri-cf ChatProjection → ChatDisplayState（Zod）]
  纯展示适配，不 flatten，不补造对象身份或运行态
    ↓
[展示组件（frontend/chat/timeline、composer 与交互卡片）]
  Entry → Text / Reasoning / Tool / Resource（按顺序）
  会话/任务/计划/队列/交互面板

Action（HTTP，独立于同步通道）：
  浏览器 → POST /api/chats/:chatId/actions（ChatUserAction + target）
    → ChatService → PeriInstance → ACP
    → 202 回执携带 requestId / inputId / status / applied 等纯数据
  失败以 disposition 表达可重试性；断线/未知结果不自动重放

载荷读取（HTTP）：
  浏览器 SDK reader → GET /api/chats/:chatId/payload/:refId?bytes=
    → 校验当前绑定与引用 → 运行侧按需读取全文
  不通过 Yjs 传全文，不持久化；旧会话/丢失引用的 ID 明确失败。

交互回答（HTTP Action）：
  用户回答 → answer Action → ChatService → PeriInstance → 原 ACP reverse request response
  浏览器不可写 Y.Doc，不经副本授权工具执行。
```

## 3. 对象契约

SDK 定义类型、校验与状态规则；应用 DTO 不成为另一份领域权威。

### 3.1 Entry

| 字段 | 契约 |
|---|---|
| `entryId` | SDK 对象身份及 Map key；投影生命周期内稳定。compact/rewind 是权威历史替换，不伪装为同对象增量。 |
| `id` | **已批准的双身份**：稳定展示身份，吸收旧 entryMetadata.id。新对象默认等于 entryId；导入保留旧 id；迟到 messageId 不改变 id。 |
| `messageId?` | ACP/历史源身份，可晚到，不替代稳定展示 id。 |
| `turnId` | message/tool 关联 turn；独立公开 system 事件可 null。 |
| `kind` | `message | tool | system`。常规 assistant 中的工具继续为 block，不复制独立 tool Entry；tool kind 仅用于真实独立工具记录。 |
| `role` / `status` | role ∈ user/assistant/system；status ∈ pending/streaming/completed/cancelled/error；组合须校验。 |
| `createdAt` / `createdAtSource` | `string|null` 与 source/observed/unknown。可信源时间优先；live 无源则 SDK 时钟只赋值一次；旧历史未知用 null，不伪造当前时间。 |
| `completedAt` | `string|null`；首次终态写入，重复退出不刷新时间。 |
| `error` | `PublicError|null` = `{code,message}`；仅公开错误，禁止 stack/cause/raw response/secrets。 |
| `tokenUsage?` | 保留，收紧为已支持的数值与请求元数据，不透传 provider 私有对象。 |
| `blockOrder` / `blocks` | Y.Array + Y.Map；view 输出有序数组，禁止按类型重排。 |
| `planEntries` | 只在 view 按 turnId 从 plansByTurn 派生，不在 Entry 重复持久化。 |

system Entry 只表示明确公开事件，**不是公开模型 system prompt**。历史 decoder（`npm-packages/@peri-sdk/src/state/protocol.ts`）会跳过 system 且只识别 text/reasoning/tool_use，扩展时保留该安全边界。

### 3.2 ContentBlock：保序与安全

| 类型 | 契约 |
|---|---|
| text | blockId、type、text:Y.Text；仅向末尾相邻同类型块追加。 |
| reasoning | 同上，加 `visibility = summary | hidden`。summary 可折叠展示；hidden 不进展示/复制/搜索。敏感正文必须在聚合前不写入，UI 隐藏不是访问控制。 |
| tool_call | blockId、type、toolCallId；同一调用只附着一次，后续更新只改独立 ToolCall。 |
| resource | resourceId/mediaType/name；只保存公开描述，不内联二进制、不自动访问任意 URI；不可读取时显示明确原因。 |

验收序列：`text:A → reasoning:R → tool:T1 → tool:T2 → text:B` 必须保序展示；T1/T2 结果晚到不重排；B 不追加进 A；实时与重放同规则。resource 解码须先核实真实 ACP 内容类型及安全标识，未知 provider block 不允许原样 JSON 透传。

### 3.3 ToolCall

1. Yjs 字段：`toolCallId, entryId, turnId, name, kind?, status, createdAt, completedAt, error, permissionId?`。
2. `status` ∈ pending/awaiting_permission/running/completed/error/cancelled；`permissionId` 关联 Interaction 的稳定字符串 id，不使用跨进程会重置的裸计数器作为浏览器命令身份。
3. `arguments|argumentsRef`、`result|resultRef` 分别互斥；未收到可都无；JSON null ≠ 未收到。
4. 参数/结果字段级 JSON 替换（不逐键 CRDT 合并）；保留外置 `{id,bytes,preview}`；result 保留 contentText/rawOutput，资源结果扩展须显式 schema。
5. 缺失 status 的更新不得默认推进为 running；终态不可回退；同终态可幂等补元数据，关闭 turn 后拒绝无关联晚到更新。
6. 检测同一 session 内 toolCallId 复用与跨 turn 错绑；若上游不保证唯一，建立显式 source ID → projection ID 映射，不覆盖历史工具。
7. 工具失败结果与公开 error 分离；不把结果正文或原始异常直接充当安全错误摘要。

### 3.4 Turn 与终态收敛

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

1. 不机械复制 rcs 的 failed/interrupted 枚举；运行后端中断用 `status=error + error.code=execution_interrupted`；interactionStatus 与运行状态正交。
2. TurnMachine 是退出唯一权威；interaction 收敛由 `InteractionProjection` 在同一事务内完成；所有入口绑定 turnId/requestId，不影响新 turn。
3. **只收敛未终态 assistant Entry**：已完成前序 Entry 保持 completed；退出错误归 Turn 与当前未完成/占位 assistant，不污染同 turn 已完成消息。
4. 未终态工具在任何 turn 退出时 cancelled；已 completed/error 保持；正常完成却残留工具应记录安全诊断，不伪造成功。
5. 未完成 interactions 置 expired 并撤销 responder；新 turn/取消/超时/断运行后端复用同一入口；晚到回答不得授予权限。
6. 请求取消不等于已取消：沿用运行后端关闭确认与执行阻断；取消请求失败可恢复活动状态，但已失效 interaction 不自动重活。
7. 终态幂等不回退；模型已完成后的保存/清理失败体现为应用 executionBlocked/error，不改写模型生成结果。
8. 两份 Y.Doc 无跨文档原子提交（`DocModel` 在 `npm-packages/@peri-sdk/src/state/doc-model.ts` 中分别持有 chat/session 文档）；沿用批处理，在完整 frame apply 后发布 view；短暂缺引用显示 pending。

### 3.5 Session / Agent / Task / Plan / Interaction

1. Session 的模型/模式、goal/compaction、usage、inputQueue 暴露只读消费面；展示面不新增模型切换等命令。
2. tasks/plans 消费：任务状态/摘要/时间；计划按 turn 展示；沿用背景任务 revision/snapshot 修复规则。计划维持“当前 info.plan + plansByTurn”由同一 SDK projection 维护，不再复制到各 Entry。
3. Agent 做语义映射而非复制 rcs：子 Agent 活动由 TaskView 展示；当前会话运行状态由 TurnView 展示；WASM 实例资源/运行后端可用性仍属 peri-cf。需要独立 Agent 对象时须先确定真实来源，禁止由 running 布尔值虚构“Agent 在线”。
4. **interactions 纳入目标消费面**：session doc 新增 `interactions` Map——稳定 id、kind、turnId、sessionId、toolCallId?、status `pending|resolved|expired`、createdAt/resolvedAt、公开问题/选项与安全表单 schema。
5. `pendingInteractions` 改为 view 从 interactions 派生，session info 不再持久化 `pendingInteractions`/`lastInteraction`；工具 permissionId 与之关联。数字 callback ID 可留 SDK live 内部；网络契约使用不碰撞的稳定 ID。回答正文、批准令牌、resolver/AbortController 不入 Yjs；结束只保留 outcome 摘要。
6. 表单契约保留必要 type/required/enum 约束；首版明确有限支持类型；不支持时提示无法回答，不误发 accept。interaction 记录须有获批的数量/保留预算，不建立无限历史。
7. 重放/历史只恢复展示，pending 失效；不恢复可回答性（运行后端回收后无 live responder）。完成 reverse request → UI → 回答 → ACP response → 取消/超时闭环后再声明 capability，不绕过当前默认拒绝。

## 4. DocModel schema 与兼容

当前 chat/session 文档版本由 `npm-packages/@peri-sdk/src/state/doc-model.ts` 的 `CHAT_SCHEMA_VERSION` / `SESSION_SCHEMA_VERSION` 定义；`SessionView.schemaVersions` 分别暴露两者，字段契约见第 3 节。

1. 固定成对版本；空副本 = `not-ready`；完整但不支持的 pair = `unsupported`，不伪装为空历史。
2. replica 无迁移器；不能只改版本数字，也不能让浏览器补 Y.Map 字段充当迁移。
3. 应用侧没有旧展示记录可转换：展示态不持久化，复原只来自 Rust Store 的 canonical 历史（见第 5 节）。
4. 新浏览器校验不能保护旧 JS；采用**版本化应用 HTTP/WS 入口**隔离新版消费面，旧入口在切换后明确升级错误/关闭连接，避免旧客户端静默丢块。SDK 同步 wire 不变。
5. 发布先停止接纳新 turn、等待在途结算；未确认关闭的会话保持 blocked，不为升级强制重跑。
6. 默认不长期双写/双 producer；若要求零维护窗口，须另审有限期双版本服务与清理条件。

## 5. 复原与载荷边界

1. **零 DO、零表、零持久化展示态**：不注册 Durable Object，不使用其存储、socket attachment 或休眠入口，也不新增 R2/KV/D1 等替代持久层。Yjs 文档、运行元数据、连接状态与载荷全文只存在于服务端进程内存。
2. **持久权威唯一**：Turso/libSQL 的 Rust Store 保存会话、消息与执行数据；ACP `session/load` 在响应**之前**以 `session/update` 重放持久历史（`peri-acp/src/host/requests/session_restore.rs`），应用必须先建立订阅/runtime 再触发重放，重放不得被丢弃。
3. **按需复原**：打开历史在 `change_session` 回执后由 `ChatView` 重放事实重建投影；目录与监控读取不触发复原、不启动 Host。
4. **执行丢失语义**：执行驻留服务端进程；进程退出即在途执行丢失且**不复原**，不得由展示历史重造 Rust 工作义务。刷新/关闭页面不结束执行。
5. **载荷全文**：工具参数/结果全文只在运行侧内存（`PayloadStore`），经 `GET /api/chats/:chatId/payload/:refId?bytes=` 按需读取，不通过 Yjs、不持久化；重放后由历史重建。若上游历史不含完整载荷，读取必须显式失败，不得伪装成功。
6. **权限面**：浏览器不可写 Y.Doc，也不能经副本授权工具执行；交互回答与工具执行授权只走 Action + ACP。
7. **不进 Yjs 的数据**：全文载荷、浏览器展开/复制状态、认证信息、网络请求与运行后端执行恢复状态。

## 6. SDK view 与安全 readPayload

### 6.1 SessionView 扩展

1. 暴露 `schemaVersions`、完整 entries、独立 toolCalls、turns、activeTurn；Entry 带 id/time/error/kind/ordered blocks。
2. tool block 可保留已有 tool 快照关联，但只是独立 ToolCall 的只读派生，不成为写源。
3. 暴露 interactions 与派生 pendingInteractions；保留 tasks/plan/plansByTurn/inputQueue/session。历史缺失、不支持、暂不可观测不得全部伪装为空数组。
4. 消费方直接读取 `SessionView` 的 `entries`、`toolCalls`、`turns`、`plansByTurn`、`pendingInteractions` 等数组，并按 id 自建局部索引；SDK 不导出 `selectEntry` 等按身份选择器，也不提供 selector 缓存/订阅机制。
5. 孤立工具引用/未知对象显示诊断或 unavailable，不静默隐藏整块；只补现有 ReadCache 的必要失效依赖，不改缓存设计。
6. 服务端「每个 Chat 共享一个 `ChatView`」由 SDK `ChatViewRegistry` 提供，从主入口与 `/portable` 导出；`/view` 只含浏览器视图面，不导出该注册表。

### 6.2 readPayload 契约与安全边界

`/view` 导出浏览器安全类型与异步 helper：

```ts
readPayload(ref, reader, { signal, maxBytes })
  → Promise<{ id: string; bytes: number; json: string }>
```

1. `reader` 为注入的 transport-neutral `ToolPayloadReader`；服务端 adapter 经 `ChatService.readPayload`（内部 `PayloadStore`），浏览器 adapter 经 peri-cf 的 HTTP 端点读取。**不导出服务端 Map 读取面。**
2. 请求绑定 `chatId + toolCallId + field + ref.id`；服务器验证归属，随机 ref id 不是授权。SDK helper 校验返回 id、UTF-8 bytes 与合法 JSON；应用拒绝跨会话或已过期 ref 的迟到响应（旧响应不覆盖新选择）。
3. 全文与 preview 不记录日志、不入持久浏览器缓存；JSON 转义显示，不当 HTML 执行；resource URI 默认不自动访问。
4. 默认按需展开读取，不全量预取。HTTP 端点是浏览器唯一读取路径；服务端只读运行侧当前内存版本（重放重建），不含持久 repository；不得以任意 URL、文件路径或 provider endpoint 兜底。
5. 明确失败码：unauthorized、unavailable、too_large、integrity_error、aborted、transport_error；未知/越界 ref 不泄露其他会话存在性；stale 不替换成最新版本内容。
6. 保留 `TOOL_PAYLOAD_INLINE_BYTES` inline 分界与投影接纳时的 inline/外置总量、reasoning、资源与累计版本预算；预算只约束运行侧内存与单次传输，不是持久容量。
7. 浏览器单次展示上限与并发读取由 `VIEW_PAYLOAD_MAX_BYTES` / `VIEW_PAYLOAD_MAX_CONCURRENCY` 定义；更大结果需另行设计流式路径（当前未实现，超限显式拒绝）。这两个常量仍标注为评审起点，平台上限须与 WASM 内存预算一并压测。
8. 不允许“已成功接纳但永久无法读全文”：接纳上限必须与可读路径一致；重放后无法重建的载荷必须显式 unavailable，不伪装成功。
9. preview 也可能含秘密：公开数据过滤须发生在投影之前，过滤后 bytes 对应实际公开 JSON；hidden reasoning 不写浏览器可见正文；UI 折叠不代替安全边界。

## 7. 前端消费路径

### 7.1 ChatReplica → ChatProjection → 展示组件

1. `frontend/remote/contracts.ts`：消息 DTO 带 id/entryId/kind/status/time/error/blocks；展示状态带 tools/turns/session/tasks/plans/interactions/queue；SDK 领域类型与应用 DTO 显式边界转换。
2. `frontend/remote/display-state.ts`：只复制 SDK 副本已确认的公开对象；`readiness !== "ready"` 时不提供条目内容，不补造身份、时间或运行态。
3. `frontend/remote/projection.ts`：持有 `ChatReplica`，缓存最近一次校验结果并在副本更新时失效；不反向控制执行、不写文档。
4. `frontend/remote/controller.ts`：两步打开历史、Action 目标核对、连接与错误状态；切换会话只释放本地连接，不 cancel、不 close。
5. `frontend/chat/runtime/useChat.ts`：装配 `ChatController`、目录查询缓存与 payload reader；选中会话的运行/交互只来自副本。
6. `frontend/chat/timeline/*` 与 composer：Entry 卡片内按 blockOrder 逐块渲染 Text/Reasoning/Tool/Resource；text 沿用 Markdown 安全设置；summary reasoning 可折叠；tool 显示状态/preview/全文入口。
7. React key 用 Entry.id/blockId/toolCallId，不用数组位置；工具晚到更新原卡片不移动；纯工具/纯 reasoning/仅错误 Entry 不提示“没有文本”。
8. Entry 错误显示一次，Turn 错误独立表达；复制默认仅可见 text，不含 hidden reasoning、工具秘密或原始错误。
9. 展开/loading/重试/复制反馈/滚动/表单草稿留浏览器；切会话或登出取消旧请求、关闭旧连接并作废在途命令（未确认不重放），拒绝旧响应写入新会话。

### 7.2 消费范围

| 对象 | 当前目标 | 不在范围 |
|---|---|---|
| Entry/Block/Tool | 保序展示、状态/error/usage、全文读取 | 工具执行重放、自动执行资源 URI |
| Session/Turn | 状态、模型/模式等只读信息、等待/失败说明 | 执行恢复、配置编辑 |
| Task/子 Agent | 状态/摘要/时间及不可观测提示 | 子 Agent 控制台、任务重启 |
| Plan | 当前与按 turn 计划 | 浏览器直接修改计划 Y.Map |
| Interaction | 权限/问题展示与安全回答闭环、超时/取消/重复回答 | 自动批准、猜测回答不支持表单 |
| InputQueue | 队列/执行请求只读关联 | 新增队列调度规则 |
| Resource | 公开描述、支持类型的安全读取/不可读说明 | 任意远端抓取、附件托管平台 |

完整性验收口径：每类对象都有“源→投影→view→DTO→组件→历史/终态”检查项；上游未启用能力显示 unsupported，不以空数组冒充“没有事件”。

## 8. 应用层 `peri-cf` map 收敛去向

| 当前字段 | 去向与理由 |
|---|---|
| entryMetadata.id | SDK Entry.id；应用不再生成对象展示身份。 |
| entryMetadata.createdAt | SDK Entry.createdAt + source；导入保留旧值。 |
| entryMetadata.error | SDK Entry/Turn PublicError；旧字符串经安全转换，不直接信任。 |
| chat | 暂留应用 map：来自应用会话目录（Turso）的 Chat DTO；明确其与 ACP session info 的来源，不混为一份实时元数据。 |
| running | 来自 SDK 副本 `SessionView.running`，由 `ChatProjection` 物化；不由列表或应用状态补造。 |
| executionBlocked | 来自 SDK 副本 `SessionView.executionBlocked`；不由展示投影另行推断。 |
| 顶层 error | 留应用内存：仅目录、打开、Action、连接等公开错误；生成失败归 Entry/Turn。 |

展示身份/时间/错误由 SDK Entry 提供，应用不再发布 `entryMetadata`；应用只保留目录、打开、Action、连接等公开错误与 UI 状态。

## 9. 未决边界与风险

| 边界 | 影响与处理 |
|---|---|
| **ACP 重放覆盖范围** | 重放是否携带完整工具参数/结果与 reasoning 未验证；不可重建的内容必须显式 unavailable，不得伪装空历史。历史缺可信时间时保持 null，不承诺推回原事件时间。 |
| **interaction 表单支持范围** | 首版字段类型/校验/多步形态与不支持时的降级（提示 vs 阻断）仍有限；回答须绑定会话/turn/live identity、幂等、失败默认拒绝；UI 完成不代替能力授权。 |
| **resource 类型范围** | 可读资源的 ACP 来源、安全标识、允许的读取路径与不可读展示契约未定；未知 provider block 不得原样透传。 |
| **公开数据过滤政策** | 工具 arguments/result/preview 的脱敏规则、`PublicError` 字段边界与日志安全码须统一；秘密不得经 preview/error 泄漏，日志只留安全 code/id。 |
| **Agent 语义映射** | TaskView/TurnView 与独立 Agent 对象的最终映射未定；需要独立 Agent 对象时须另行审定数据源与生命周期，不能由 running 布尔值构造。 |
| **payload 生命周期** | 重放后旧 ref 不得返回新内容；不可读必须显式失败；仅内存不等于内存有界，单 payload 与会话总量上限须与 WASM 预算一并压测。 |
| **checkpoint API 去留** | `ChatDocument.fromCheckpoint` / `exportCheckpoint`（`npm-packages/@peri-sdk/src/view/chat-document.ts`）已无生产消费方；删除与否待用户裁决，未裁决前不得为它保留应用侧调用点。 |

## 落位与维护

1. 本文正式位置为 `docs/design/peri-cf-chat-objects.md`，按 `docs/design/README.md`「已批准目标设计」登记；对象契约的稳定结论已并入现行 [Peri SDK Chat](peri-sdk-chat.md)、[服务端 Agent](peri-cf-server-agent.md) 与 [Chat 前端](peri-cf-chat-ui.md)。
2. 实施完成后只保留稳定结果；动态 inventory、固定源码行号与命令输出不入本文，定位信息进 `docs/code-index/peri-ts-sdk.md` 与 `docs/code-index/peri-cf.md`。
3. 文件改名/合并/删除按 DOC-LINK-001 同步根与模块路由、standards、code-index 与 active spec，并运行本地 Markdown 链接检查。
