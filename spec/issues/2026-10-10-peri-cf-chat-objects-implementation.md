# peri-cf 聊天对象实施：P0 冻结与 P1 对象权威

状态：进行中。P0–P4 与 **P5 本地范围**均已交付并通过测试：本地已跑通真实 WASM × workerd × DO × WebSocket × libSQL × 模型 fixture 的闭环（`scripts/smoke.mjs`），workerd 上的载荷端点、权限往返与断宿主验收亦通过。**生产切换（维护窗口、部署端验收、真实浏览器）未执行**，不得称为生产验证。
依据：[已批准设计](../../docs/design/peri-cf-chat-objects.md)。不重新裁决设计，不进入 P3–P5。

## 隔离与实施边界

- 基点 HEAD：`23a6c3e8`；分支 `feature/peri-cf-chat-objects-20261010`，无 upstream。
- worktree：`/Users/konghayao/code/ai/peri-cf-chat-objects-20261010`。
- 从主工作区仅复制设计正文、设计 README、SDK/CF 两份代码索引；主工作区其他 WIP 不带入、不修改。
- 不 commit/push，不变更数据库表、DO 记录或平台资源。peri-cf 本轮只运行构建脚本与测试，未新增任何绑定或存储资源。
- mise 授权（用户批准）：worktree 内 `npm-packages/@peri-sdk/mise.toml` 与 `<worktree>/mise.toml` 先与 worktree HEAD、主工作区工作树三方比对哈希一致（SDK `1ea953c1…`、根 `26423b7b…`），再执行 `mise trust`；`mise install` 报告工具已装，`mise exec -- sqld --version` 输出 `sqld 0.24.32 (40c272de 2025-02-14)`。
- 保留两份 Y.Doc、Map 身份、Array 顺序及 Y.Text 流式追加。schema 数字未提前升级，避免以不完整结构冒充新契约；这是不可发布的分阶段实施分支，不是兼容层或长期双写。

## P0 冻结记录与默认值

1. **身份/版本**：目标 pair 固定 chat=3/session=2；entryId 为 key，id 为展示身份，新建默认同值，晚到 messageId 不改 id。旧记录必须生产端导入新文档；消费端拒绝旧 pair；版本化入口、短维护窗口、无长期双写。上述后四项未实现（见「剩余差距」）。
2. **时间**：SDK 时钟可注入；live 创建时间只观察一次；历史缺可信时间为 null/unknown；首次终态写完成时间，不重复刷新。Turn 记录同规则（重放 turn 的 `startedAt` 为 null，不伪造源时间）。
3. **公开数据（已实现的部分）**：system 历史不投影；reasoning 仅缺省/summary 正文可写，hidden 或未知 visibility 不写；未知 provider block 不透传；公开错误只允许固定白名单 `PublicError`（`execution_failed`/`execution_interrupted`/`tool_failed`/`payload_too_large`，固定中文 message），不转发异常正文、stack、cause。存进文档的载荷是公开 JSON 的往返结果（`bytes` 与实际存储 JSON 一致，preview 为其前缀或 contentText 前 512 字符）。**不做秘密扫描**，不声称已验证的 secrets 过滤。
4. **payload 常量（默认待确认）**：inline 分界 4096 bytes、preview 512 字符（设计固定）；inline 累计 1 MiB（超出后新值降级为外置引用，数据不丢）；单个外置载荷 8 MiB、单会话外置累计 32 MiB（越界显式拒绝并在工具上记 `payload_too_large`，保留上一个可读版本）。
5. **浏览器读取（默认待确认）**：单次展示 1 MiB、并发 2（设计评审起点）；`/view` 提供注入式 reader 的 `readPayload` 与 `limitPayloadReads`，失败码 `unauthorized`/`unavailable`/`too_large`/`integrity_error`/`aborted`/`transport_error`。
6. **interaction（默认待确认）**：稳定 `interaction:<uuid>` id；每 session 保留 50 条终态记录（pending 永不回收，按 resolvedAt 从最旧终态开始删）；单步表单窗口 = 仅 string/number/integer/boolean 属性、required、同类型 enum，字段数上限 32；嵌套 object/array、组合 schema（anyOf/oneOf/allOf/$ref）、多步与非布尔 additionalProperties 一律判为不支持，整表单默认拒绝且**不误发 accept**；不支持的判定与理由写入记录（`formSupport`/`formReason`）。
7. **resource 默认边界（冻结，尚未编码）**：只接纳经真实 ACP fixture 确认的公开描述；读取类型暂为 unsupported，不抓取任意 URI、不内联二进制、不创建附件平台。真实 ACP resource 来源与安全标识未核实前不新增假 fixture。
8. **Agent 映射**：Task 表示子 Agent 活动、Turn 表示会话运行状态、CF 表示宿主资源；不造独立 Agent 对象，不从 running 推断在线。
9. **存储**：SDK checkpoint 方向保持；DO 结构/容量/保留/备份必须另行审批，本批未涉及。

## 已实现（P1 对象权威）

- `doc-model.ts`：Entry 的 `id/kind/createdAt/createdAtSource/completedAt/error`，Tool 的 `createdAt/completedAt/error/permissionId`；`publicError` 白名单；schema 常量 `CHAT_SCHEMA_VERSION`/`SESSION_SCHEMA_VERSION`；会话根新增 `turns`/`interactions` 容器；payload 预算注入。
- `turn-machine.ts`：`turns` 记录（status/startedAt/completedAt/error/stopReason/interactionStatus）与 `session` 当前索引同事务维护；退出只收敛未终态对象；重复终态不刷新时间；`resetAfterHistory` 不再遗留无法观察的 live turn（标 `execution_interrupted`）。
- `interaction-projection.ts`：`interactions` Map、稳定字符串 id、`permissionId` 关联、resolve/expire 单次终态、迟到回答不复活、turn 等待状态派生、数量预算、表单窗口校验。
- `chat-projection.ts`：载荷容量拒绝路径（公开错误、保留旧版本）、缺省 status 不推进、跨 turn 工具身份防护、前序已完成 Entry 迟到防护。
- `checkpoint.ts` + `SessionDocs.exportCheckpoint()/fromCheckpoint()`：纯展示材料导出（含 payload 全文与索引一致性校验）与全新文档导入（重建索引、清除 live 身份、活动对象收敛、任务 `unobserved:`、拒绝未知版本/未知 pair/缺失或错配引用）。
- `view/payload-reader.ts` + `/view` 导出：`readPayload`/`limitPayloadReads` 契约与预算默认值；`session-view.ts` 的待处理交互与 `activeTurnInteractionStatus` 改为从 `interactions` 派生（不再读 `info.pendingInteractions`/`latestInteraction`）。
- `agent/types.ts`、`interaction-responder.ts`、demo：interaction 身份改为稳定字符串（原裸计数器只存在于旧实现，已删除）。
- 测试：`tests/chat-object-contract.test.ts`（14）、`tests/checkpoint.test.ts`（5）、`tests/payload-reader.test.ts`（7）、`tests/interaction-projection.test.ts`（6）为本轮契约入口；两个缺口哨兵已按承诺转为 checkpoint 正例（旧记录 id/time 保留、payload 全文可恢复），不再有 `test.failing`。
- 消费端一处期望更新：`npm-packages/peri-cf/tests/projection-hot-path.test.ts` 原断言「取消把所有 assistant 消息变 cancelled」，与已批准设计 4.4.3（取消不污染已完成 Entry）冲突，改为 `["completed","cancelled"]` 并注明依据。

## 验证记录

worktree 内，工作目录 `npm-packages/@peri-sdk`，除另注外。本轮命令按时间顺序：

| 命令 | 结论 |
| --- | --- |
| mise.toml 哈希三方比对 + `mise trust` ×2 + `mise install` | 一致后执行；`sqld 0.24.32` 可用 |
| `bun test tests/wasm-acp-integration.test.ts`（补齐被阻塞项） | exit 0：2 pass / 0 fail，含此前失败的真实 WASM ACP 两项 |
| `bun test`（本轮改动前基线） | 235 tests：230 pass / 5 fail，记录改动前状态 |
| `bun run typecheck` | exit 0（源码 + demo 两套配置） |
| `bun test tests/{checkpoint,payload-reader,chat-object-contract,interaction-projection,session-view,session-sync}.test.ts` | 全部通过；关键回归：session-sync 批处理帧数在 turn 记录去重写入后恢复上界内 |
| `bun run test`（最终，含 Rust WASM 构建） | exit 0：**252 pass / 0 fail，35 files，2100 assertions** |
| peri-cf：`bun install`、`bun run build:wasm`、`bun run prepare:wasm`、`bun run typecheck`、`bun run test` | exit 0：**360 pass / 0 fail，28 files**；`build:wasm`/`prepare:wasm` 仅为本地构建产物（CF profile + provenance），未新增平台资源 |
| 根目录 `bash scripts/check-file-size.sh` | exit 0：2217 文件，0 超限 |
| 根目录 `git diff --check`、所涉文档本地链接检查 | exit 0 |
| 主工作区保护核对 | 六个 WIP 文件 SHA-256、工作区/暂存区 binary diff 均与创建 worktree 前基线一致；本轮未触碰主工作区 |

日志（本机临时证据，非仓库产物）：`/tmp/peri-chat-round2-sdk-final.log`、`/tmp/peri-cf-tests-3.log`、`/tmp/peri-cf-build-wasm.log`、`/tmp/peri-chat-round2-full.log`。第一轮日志见上一版记录对应文件。

## P2 交付记录（view 层 + 应用侧消费面）

### SDK view（上一轮完成，本轮复用）

`readiness`/`schemaVersions`、完整 entries、独立 toolCalls、turns 与 activeTurn、interactions 与派生 pendingInteractions、未知块与孤立工具的显式诊断、5 个纯选择器；契约与回归见 `tests/session-view-surface.test.ts` 与 `tests/session-view.test.ts`。

### 应用侧（本轮完成）

- `shared/chat.ts`：块式 Entry DTO（entryId/id/kind/turnId/role/status/createdAt/createdAtSource/completedAt/error/blocks）+ ToolCall/Turn/Interaction/Task/Plan/InputQueue/SessionInfo schema；`chatDetailSchema`、公开 `messageSchema`、`sessionRecordSchema` 删除；`persistedMessageSchema` 仅服务旧 DO 记录（P3 迁移输入）。
- `shared/sync.ts` / `shared/sync-state.ts`：`SyncState`（chat/running/executionBlocked/error/readiness/entries/toolCalls/turns/activeTurn/session/tasks/plans/interactions/pendingInteractions/inputQueue）；`syncMetadataSchema` 只保留应用自有运行元数据；删除 flatten 与 entryMetadata 回填；`readSyncState` 在不受支持 pair 时抛 `UnsupportedSchemaError`。
- `worker/chat/projection.ts`：移除 `presentation` 与 entryMetadata 发布；展示身份/时间/错误全部取自 SDK Entry；旧记录写回保留（`legacyIdentity` 身份桥，仅 restore 配对，P3 移除）；新增 `state()` 供 HTTP 首屏与 WS 共用。
- `worker/chat/session.ts`：`GET /api/chats/:id` 返回 `projection.state()`；重启打断改为应用级 error（"Execution interrupted by host restart…"），不再改写 SDK 生成结果。
- `web/api/client.ts`、`web/api/sync.ts`、`web/chat/{types,queries,session}.ts`：消费 SyncState；not-ready 不发布空历史；unsupported 停用连接并提示升级。
- `web/chat/MessageList.tsx`：按 blockOrder 渲染 Text / Reasoning（summary 折叠，hidden 不渲染）/ Tool（状态、preview、全文入口、缺失引用原因）/ 未知块（含 resource 未支持原因）；React key 用 Entry.id 与 blockId；工具更新就地更新；纯工具/纯推理/仅错误 Entry 不显示"没有文本内容"。
- `web/chat/Panels.tsx`：会话/任务/计划/队列/交互只读面；interaction 展示状态、选项/字段、`formUnsupported` 原因与"回答闭环待后续版本"提示（回答闭环属 P4）。
- `web/chat/payloadReader.ts`：注入式 reader 接线（默认 `unavailablePayloadReader` 明确报告 P3 未提供）；HTTP 端点留 P3。
- 验证：SDK `bun run test` **257 pass / 0 fail**；peri-cf `bun run typecheck` 与 `bun run test` **360 pass / 0 fail**。

### 本轮迁移的既有断言（语义变化，逐条依据）

1. `tests/client-sync.test.ts`：DTO 身份从 `entryMetadata` 改为 SDK Entry（`entryId`/`createdAt`/`createdAtSource`）；依据设计 5.1/8「SDK view 暴露完整 Entry」「entryMetadata 移除，应用不再生成展示身份」。
2. `tests/sync-wire.test.ts`：同上；entry 状态直接透出 SDK 值（`streaming`，不再映射为应用侧 `running`）；应用结算失败不再把已完成的 SDK 结果改写为 error，改为应用级 `error` + `executionBlocked`；依据设计 4.4.7「模型已完成后的保存/清理失败体现为应用 executionBlocked/error，不改写模型生成结果」。
3. `tests/server-sync.test.ts`：`state().messages` → `state().entries`（文本由 text 块拼接）；"application error overrides early SDK completion" 改为"不改写已完成结果、以应用级 error 与执行阻断表达"；旧记录恢复用 `messageId`/文本/状态核对，并新增"旧记录行 id/createdAt 保留"断言（身份桥）；依据设计 8 与 4.4.7。
4. `tests/projection-hot-path.test.ts`：新内容身份来自 SDK Entry（不再是 messageId/随机 UUID），旧记录行沿用 SDK 公开错误文案；同长度 compact 历史使用 `message:{id}` 身份；依据设计 5.1/8。
5. `tests/chat-queries.test.ts`、`tests/web-api.test.ts`、`tests/backend.test.ts`、`tests/real-workerd-ws.test.ts`：HTTP 首屏/详情断言从 `{chat, messages}` 迁移为 SyncState；宿主诊断文本从"记录行 error"迁移为"应用级 error"（记录行沿用 SDK 公开错误）；`projection-hot-path` 与 `real-workerd-ws` 明确记录旧记录路径的身份过渡限制。依据设计 8「单读旧数据、单写新权威」与「顶层 error 留应用」。
6. `tests/client-sync.test.ts` 手工文档对补 `schemaVersion`；依据文档版本标记是 SDK `DocModel` 恒写字段、新 view 依赖它判定 readiness。

### 新增测试

- `web/chat/messageStatus.test.tsx`：DTO 边界（readiness/entry 字段/不支持 pair）、渲染（仅错误、纯工具、hidden reasoning、孤立工具、未知块、空完成回答）、payload reader 接线（成功/不可用/超限/字节不符）。
- `tests/server-sync.test.ts` 新增"HTTP 首屏与 WS 实时返回同一 DTO 与同一批对象身份"。

## P3 交付记录（存储与升级，已批准方案）

- `worker/chat/record.ts`（新）：`recordVersion:2` 单键索引 + `payload:<id>:<n>` 1 MiB 分段（UTF-8 安全切分）；写入顺序「全部分段 → 索引提交」；v1 转换（幂等、`record:v1-backup` 7 天窗口、索引超 1 MiB 时从最旧 turn 裁剪并标记 `legacyIncomplete`）；加载回填（引用必须可解析，否则 `RecordCorruptionError` 显式失败）；按新旧索引差立即回收被替换段；孤儿段与过期备份清扫；`restoreBackup()` 回滚还原；未知 `recordVersion` 拒绝读取。
- `worker/chat/session.ts`：记录读写改经适配器；`save()` 提交 checkpoint（含分段）；接纳预检改为索引 1 MiB（`MAX_INDEX_BYTES`）；新增 `GET /api/v2/chats/:id/payloads/:refId`（同源 Bearer、`no-store`、1 MiB 下载上限；404 `unavailable` / 413 `too_large` / 500 `integrity_error`）。
- `worker/chat/projection.ts`：导入替换文档对（`restoreCheckpoint`）；移除旧记录重放与 `legacyIdentity` 身份桥（P3 按设计删除）；`checkpoint()` 供提交。
- 版本化入口：`/api/v2/chats`（含 `/messages`、`/cancel`、`/resources`、`/payloads/:refId`、`/sync`）；旧 `/api/chats/...` 全部方法返回 426 `{error, upgrade: true}`（含 WS 路径，不升级成功）。
- 前端：`httpPayloadReader(chatId, token)` 接 `/api/v2` 端点（Bearer + no-store，失败码映射 SDK 契约），`MessageList` 注入 reader；`unavailablePayloadReader` 只作未就绪占位。
- schema 升级：SDK `CHAT_SCHEMA_VERSION = 3`、`SESSION_SCHEMA_VERSION = 2`；checkpoint 支持集与 view readiness 随常量生效；旧 pair 文档在新客户端判 `unsupported`（不伪装空历史），旧客户端由 426 门禁拦截。
- 测试：`tests/record-store.test.ts`（11 例：切分、转换幂等、裁剪、分段读写、缺段/错配、孤儿清扫、索引提交失败、版本拒绝、备份过期与还原）；`tests/p3-migration-drill.test.ts`（演练：冻结 → 转换 → 切入口 → 回滚还原）；backend 新增「版本化入口与载荷端点」3 例（200/401/404/413/500、旧路径 426）；前端 `httpPayloadReader` 3 例。

### 验证记录（本轮）

| 命令 | 结论 |
| --- | --- |
| `npm-packages/@peri-sdk`: `bun run typecheck`、`bun run test` | exit 0；**257 pass / 0 fail**（36 files） |
| `npm-packages/peri-cf`: `bun run typecheck`、`bun run test` | exit 0；**378 pass / 0 fail**（30 files） |

### 演练与平台验证边界

- 本地演练（`tests/p3-migration-drill.test.ts`，可重复执行）：旧路径 426 → 首次 v2 访问触发转换（备份写入、旧键删除、`recordVersion:2` + pair 3/2）→ v2 读取与列表可用 → `restoreBackup()` 还原 v1 后可再次转换。命令：`bun test tests/p3-migration-drill.test.ts`，1 pass / 17 断言。
- **需部署端验证（本地无法证明）**：部署端 2 MB 单值上限的实际强制点与错误形态（本地 workerd 未复现）；真实 DO 存储的写入速率/CPU 计入；生产维护窗口的流量切换与旧构建回滚（须在预发演练一次）；R2 未引入。

## P4 交付记录（interactions 往返闭环）

- `worker/chat/interactions.ts`（新）：每轮执行的 live 交互登记表——只保存 settle 闭包与会话身份；无回答者保持安全默认（权限拒绝、提问 decline）；`answer()` 校验 kind 并把跨会话/迟到/重复/失效统一按未知处理；`close()` 在轮次结束/宿主关闭时撤销。
- `worker/chat/session.ts`：`setRequestHandler` 把 `session/request_permission` 与 `elicitation/create` 交给 SDK 投影（`SessionDocs.handleRequest`），其它方法保持默认拒绝；`session/load` 后 `seedConfig` 与 `session/bg-tasks` 快照入口；轮次结束先由投影 `abortAll` 置 `expired` 再兜底关闭队列（宿主关闭导致的挂起不记为“用户拒绝”）；新增鉴权命令 `POST /api/v2/chats/:id/interactions/:interactionId`（202 接受 / 400 非法或 kind 不符 / 409 迟到、重复、跨会话、失效）。
- `worker/chat/bootstrap.ts`：能力声明新增 `peri.unstableEvent`（任务快照与后台任务事件通道；Rust 门控见 `peri-acp/src/session/event_sink.rs` 的 `caps.unstable_event`）。**交互本身不需要新的 capability 开关**：Rust 侧 `PeriCaps` 无权限/提问相关标志，broker 在 Forward 模式下无条件转发请求。
- SDK：`InteractionCallbacks`（宿主只提供两个回调）；表单窗口字段描述扩展为 `{type,required,enum}` 并写入交互记录与视图；新增 `validateElicitationContent`（类型/必填/enum/未知字段；投影在接受前复核，前端 `buildElicitationContent` 复用同一套语义）；**不支持的表单不再调用回调**，直接按 decline 结算（不产生无法回答的 pending）。
- 前端：`web/chat/interactionForm.ts`（纯函数：初值与提交内容）；`web/chat/Panels.tsx` 回答面（权限按钮仅支持 `allow_once`/`reject_once`，其它选项显示“暂不支持”；提问表单按类型渲染 text/number/checkbox/select 并标注必填；提交/拒绝按钮；不可回答时给明确原因）；`web/api/client.ts::submitInteractionAnswer`（Bearer + no-store，409/401 映射为可读提示）。
- 测试（先补后验）：SDK `tests/interaction-projection.test.ts` 增表单窗口与内容校验（含缺必填/类型不符/enum 越界/未知字段均不误发 accept）；SDK `tests/session-view-surface.test.ts` 改为「不支持表单直接拒绝、支持表单 pending→accept」；peri-cf 新增 `tests/chat-interactions.test.ts` 7 例往返（权限允许/拒绝、提问提交与内容校验、不支持表单、重复/未知/跨会话/kind 不符、轮次退出置 expired 且迟到回答被拒、重启后 pending 失效且命令关闭、能力声明+配置源+任务快照修复），`tests/backend.test.ts` 保留默认拒绝策略用例并拆分到 1000 行以内；前端 `web/chat/interaction-panel.test.tsx` 6 例（选项可见性与支持范围、表单渲染、不可回答原因、已结算只读、取值构造）。

### 验证记录（本轮）

| 命令 | 结论 |
| --- | --- |
| `npm-packages/@peri-sdk`: `bun run typecheck`、`bun run test` | exit 0；**259 pass / 0 fail**（36 files） |
| `npm-packages/peri-cf`: `bun run typecheck`、`bun run test` | exit 0；**391 pass / 0 fail**（31 files） |

### 能力声明状态

- **交互（权限/提问）**：无需新增 capability（协议无对应开关，Rust `PeriCaps` 与 broker 均不门控）；闭环已在本地 fixture 层面验证（真实 ACP 请求 → 投影 → 鉴权命令 → live responder → 原 response）。真实 WASM 宿主 + workerd + 浏览器端到端属 P5，尚未验证。
- **任务快照/后台事件**：本轮声明 `peri.unstableEvent`，依据 Rust 门控代码（`event_sink.rs`）与本地 fixture 验证（修订缺口触发 `session/bg-tasks` 并回填任务）。部署端平台验收属 P5。

## P5 交付记录（本地闭环验收与收尾）

### 端到端闭环（本地）

1. **真实 WASM × workerd × DO × WS × libSQL × 模型 fixture**：`SQLD_BIN=<sqld> bun scripts/smoke.mjs` → `{"passed":true,"runtime":"workerd","wasm":true,"turns":3,"primaryModelCalls":4,"historyAfterRestart":true,"acpContextAfterRestart":true,"cancelledTurn":true,"partialReplyPersisted":true,"continuedAfterCancel":true,"webSocketSync":true,"commandHttp202":true,"tursoDirectory":true,...}`。该脚本经本轮迁移到 v2 入口与块式 DTO；覆盖：真实 WASM ACP 宿主启动、三轮对话、取消与部分回复保留、取消后继续、DO 重启后历史与 ACP 上下文恢复、Turso 目录、命令 202。
2. **workerd 验收（`tests/real-workerd-ws.test.ts`，8 例）**：既有 5 例（WS 交付/休眠/信用/重放）+ 新增 3 例 P5 闭环——① 工具大载荷写入真实 DO 分段存储并经 `/api/v2/.../payloads/:refId` 读取（200 + `no-store` + 字节一致；未鉴权 401；未知引用与跨会话 404）；② 权限往返（fixture 反向请求 → 投影 pending → 鉴权命令 202 → live responder 结算 → ACP 响应 `selected/allow_once`；重复回答 409、未鉴权 401）；③ 断宿主（取消返回 500 不伪装成功停止、`executionBlocked` 置位、未决交互 `expired`、迟到回答 409、后续命令 503）。
3. **修复的真实缺陷**：`startMessage` 先投影用户输入再发布运行元数据，导致副本可能先看到条目而 `running` 仍为 false（误判轮次结束）——真实闭环验收暴露后修复为先 `setRunState(running:true)` 再投影；`scripts/smoke.mjs` 的等待条件改为以终态条目为准（跨 Y.Doc 无原子提交，设计 4.4.8）。

### 发布演练（本地范围）

`bun test tests/p3-migration-drill.test.ts`（1 pass / 17 断言）覆盖：旧入口 426 → 首次 v2 访问触发 v1→v2 转换（备份写入、旧键删除、`recordVersion:2`、pair 3/2）→ v2 读取/资源可用 → `restoreBackup()` 回滚还原并可重复转换。维护窗口中的**冻结新 turn**在本地以 `executionBlocked` 语义等价验证（503），见 workerd 第③例。

### 过渡遗留清理

`entryMetadata`、flatten 写回、`legacyIdentity`、`presentation` 映射、`persistableMessages`、`MAX_TRANSCRIPT_BYTES` 均已删除（全仓 grep 无残留；`role="presentation"` 为 ARIA 属性）。旧 `session` 键与 `checkpointFromLegacyRecord` 仅作 v1→v2 迁移输入保留；`scripts/e2e.mjs`、`scripts/smoke.mjs` 已迁移到 v2 入口与块式 DTO；创建阶段的默认拒绝保留并注明原因（无 prompt 即无交互）。

### 验证记录（本轮）

| 命令 | 结论 |
| --- | --- |
| `npm-packages/@peri-sdk`: `bun run typecheck`、`bun run test` | exit 0；**259 pass / 0 fail**（36 files） |
| `npm-packages/peri-cf`: `bun run typecheck`、`bun run test` | exit 0；**394 pass / 0 fail**（32 files） |
| `npm-packages/peri-cf`: `SQLD_BIN=… bun scripts/smoke.mjs`（真实 workerd + WASM + sqld + 模型 fixture） | 输出 `passed:true`（见上） |
| `npm-packages/peri-cf`: `bun test tests/real-workerd-ws.test.ts` | 8 pass / 0 fail |
| 根 `check-file-size.sh`、`git diff --check`、链接检查、主工作区核对 | 0 超限；无空白错误；链接有效；主工作区基线文件逐字节一致（1 个属主自行移除） |

### 需部署端/预发验证（本地无法证明）

1. 部署端 DO 单值 2 MB 的实际强制点与错误形态（本地 workerd 8 MiB 内未拒绝、16 MiB 报 `SQLITE_TOOBIG`）。
2. 真实平台计费/CPU/写入速率与 10 GB 每对象上限的实际表现。
3. 生产维护窗口：流量切换、旧客户端 426 行为、预发完整回滚演练（旧构建 + 备份还原）。
4. 真实浏览器验收（登录、刷新/重连、表单交互、复制/展开）——仓库既有 `scripts/e2e.mjs` 需要真实部署与模型。
5. resource 块解码（真实 ACP 来源核实后决定）。
6. elicitation `_meta`（`peri.elicitationUnanswered`）需 Rust 侧扩展 `UnansweredCause` 才能如实声明「表单不支持」。
7. workerd 组合中 WS 信用/休眠在真实平台（含 hibernating socket 跨版本混跑）的复核。

## 剩余差距与后续

1. **resource 块**仍未解码：真实 ACP 来源与安全标识未核实；当前以显式 `unknown` 块与不可读原因展示（设计 4.2）。
2. **生产切换未执行**：schema 3/2 与 v2 入口已就绪，本地闭环（真实 WASM × workerd × DO × WS × sqld × 模型 fixture）已跑通，但维护窗口、部署端验收、真实浏览器与预发回滚演练仍需部署端完成（见「需部署端/预发验证」清单）。
3. **R2 未引入**（批准为零新增资源）：当单载荷常态 >8 MiB、需要 Range 流式下载或持久量成为常态时另立小项。
4. **交互闭环**已接通（P4）；真实 WASM 宿主 + workerd + 浏览器的端到端验收属 P5，未验证前不宣称生产可用。
5. 无时间保留（按引用回收）；更长保留窗口与容量压测待真实数据后再评估。

## P3 批准包

P3（DO 记录切换、payload 持久化、容量/保留/GC、平台资源、3/2 升级路径）的调研与方案已产出，**待用户逐项裁决**：见 [P3 批准包](2026-10-10-peri-cf-chat-objects-p3-approval.md)。该文件只做调研与方案，不含实施；平台限制以官方文档链接与本仓库一次性实测为证（探针脚本运行后已删除，源码留档于附录）。

## 下一轮范围与门禁

1. 部署端/预发：维护窗口切换与回滚演练、部署端 2 MB/计费/CPU 验收、真实浏览器验收（见上「需部署端/预发验证」清单）；resource 来源核实后决定解码，否则保持 unsupported；`elicitationUnanswered` 若需要则先扩展 Rust 枚举。
2. P2 起步：新 SessionView 读面（完整 tools/turns/interactions、`schemaVersions`）、纯选择器、消费端 schema pair 拒绝（`not-ready`/`UnsupportedSchema`）、shared DTO 与块组件；先补 DTO/组件测试。
3. 上述两条完成后才具备一次性升级 3/2 的条件；升级前仍需版本化入口与维护窗口设计（P3 + 审批）。
4. 不得将本批加法字段与 2/1 版本当作对外发布契约。
5. 任何 DO 持久化结构、存储容量、保留/GC、bucket/绑定或数据库表变更 → 先停下请求批准。
