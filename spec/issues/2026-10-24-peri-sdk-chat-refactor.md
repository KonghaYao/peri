# Peri SDK Chat 架构重构

- 状态：**W0 契约修订完成（2026-10-24）；W1 运行/控制面修复未实施，待独立复验**
- 事实源：本 issue；用户裁决与实施计划见工作区 `.peri/plans/peri-sdk-chat-refactor-plan.md`（非仓库长期事实源）；跨流 FAIL 修复以 `.peri/plans/peri-sdk-chat-integration-fix-plan.md` §1 为准。
- 分支：`feature/peri-cf-browser-agent`。
- 实施边界：已完成 Phase 0 契约冻结与 W0 修订；W1/W2 尚未完成，不删除现有门面，不宣称验收通过。

## 1. 范围

把 SDK 的 Chat 能力收敛为五个核心对象：

1. `ChatManager`：共享依赖快照、句柄目录、Session claim 协调、单个/整体释放；不接收普通执行动作。
2. `ChatService`：单 Chat 的绑定、动作分派、事实日志与载荷读取；不持有 Manager，不依赖 Yjs。
3. `ChatUserAction`：八类可序列化用户意图，携带稳定 `requestId` 与纯数据 `payload`。
4. `PeriInstance`：一次激活的宿主/Session 装配、交互/队列/历史/载荷控制与事实输出。
5. `ChatView` / `ChatReplica`：服务端事实投影到 Yjs，浏览器副本只读并经选择器读取。

依赖方向固定为 `ChatManager → ChatService → PeriInstance → Session → Transport`；`ChatView` 只依赖 `ChatFactSource` 与 `ChatYjsChannel`。

## 2. 非范围

- 不修改 Rust、ACP wire 协议或数据库表结构。
- 不恢复 main 的 Cloudflare Worker/DO 目录；CF 消费方沿用工作树 `server/`、`core/`、`frontend/`。
- 不新增 SSE 同步通道；目标同步为 WebSocket + Yjs。
- Phase 0 不删除 `Agent`、`ManagedAgents`、`SessionDocs`、`SessionViewStore` 等现有门面。
- 不让 SDK 变成第二个调度器，不新增持久执行恢复状态。

## 3. 阶段

| 阶段 | 内容 | 当前状态 |
| --- | --- | --- |
| Phase 0 | 契约冻结与脚手架（action/types/errors/facts/channel 与定向测试） | 完成 |
| Phase 1 | 运行侧：Session 纯 ACP 客户端、PeriInstance、交互/历史/任务/载荷 | 未实施 |
| Phase 2 | Chat 控制面：Manager、Service、绑定状态机、claims、回执 | 未实施 |
| Phase 3 | View/Yjs：ChatDocument、ChatView、ChatReplica、同步协议 | 未实施 |
| Phase 4 | SDK 集成：公开出口切换、删除旧层 | 未实施 |
| Phase 5 | CF 服务端：ChatManager 接线、Action 路由、WS | 未实施 |
| Phase 6 | CF 前端：Remote Action/回执、Replica 选择器、WS 客户端 | 未实施 |
| Phase 7 | 删除旧层、文档更新与最终验收 | 未实施 |

## 4. Phase 0 冻结接口

| 文件 | 冻结内容 |
| --- | --- |
| `npm-packages/@peri-sdk/src/chat/action.ts` | 八类 Action、`ChatUserAction` 工厂、`parseChatUserAction`；`requestId` 解析保留原值；文本上限 32768；`answer` 复用 `parseInteractionAnswer`；工厂结果冻结；不依赖 `instanceof` |
| `npm-packages/@peri-sdk/src/chat/types.ts` | `ChatBindingTarget`、`SessionReadyResult`、`SendReceipt`、取消/控制/历史结果、Manager/Service 接口、数据库/claim/宿主配置、`PeriInstancePort`（含 `readPayload` 与 `bindNew.onSessionCreated`）、`ChatLifecycleOptions`、`ChatHost.transport.factory(path, {signal})` |
| `npm-packages/@peri-sdk/src/chat/errors.ts` | `ChatDisposition`、`ChatActionError` 及校验/忙碌/未就绪/关闭/绑定冲突/新建失败/回放溢出错误；`ChatCloseIncompleteError`（保留 failures 与原因链） |
| `npm-packages/@peri-sdk/src/chat/facts.ts` | `ChatFact`、`ChatFactEnvelope`、`InteractionFactRecord`（含 `questions`）、`InteractionQuestion`、`ChatFactObserver`、`ChatFactSource` |
| `npm-packages/@peri-sdk/src/agent/types.ts` | `AcpMcpServerInstance`（可选 `close()`）、`AcpMcpServerInstanceFactory`、`AcpMcpServer` 的 acp 实例/工厂分支 |
| `npm-packages/@peri-sdk/src/view/session-view.ts` | `InteractionQuestionView`、`InteractionView.questions?`（仅类型镜像） |
| `npm-packages/@peri-sdk/src/state/protocol.ts` | `SnapshotBlock` 的 `argumentsError?: "payload_too_large"` 及其解码 |
| `npm-packages/@peri-sdk/src/view/channel.ts` | `ChatYjsChannel` 逻辑传输接口；不创建 WebSocket/HTTP，不依赖 Node |
| `npm-packages/@peri-sdk/tests/chat-action.test.ts` | 工厂、解析、requestId、可序列化、冻结、非法形状拒绝 |
| `npm-packages/@peri-sdk/tests/chat-fixtures.ts` | `FakePort` 显式实现 `readPayload`；`bindNew` 支持 `onSessionCreated` 与可编程 gate |

错误 `disposition`：`rejected` 明确未生效；`unknown` 可能已写出/执行且不得自动重试；`not_sent` 只由调用端在提交前产生。`cancel` 的 `requested` 只表示通知已写出，不是停止确认。

### 4.1 W0 契约修订（2026-10-24）

依据修复计划 §1.1–1.6 与 §2.2，本轮只改契约/共享类型与测试夹具，不实现运行或控制面逻辑：

| 冻结项 | 修订 | 计划小节 |
| --- | --- | --- |
| `PeriInstancePort.readPayload(ref, options?)` | 必填成员；错误码沿用 `PayloadReadError` 语义；不再是结构探测 | §1.1 |
| `InteractionQuestion` / `InteractionFactRecord.questions?` | 保留 `variant:"ask_user"`；仅 ask_user 使用；`fields` 继续服务普通 elicitation | §1.2 |
| `ChatLifecycleOptions` / `ChatManagerOptions.lifecycle?` | 四个有界预算（3000/5000/5000/15000ms）；超时为 Incomplete | §1.3 |
| `ChatHost` transport 工厂 | `factory(path, {signal?})`，启动可取消 | §1.3 |
| `ChatCloseIncompleteError` | 保留 `failures`；多失败时 `cause` 为 `AggregateError` | §1.3、§1.8 |
| `AcpMcpServer.acp.server` | 接受直接实例或工厂；`AcpMcpServerInstance.close?()` 仅工厂实例必须实现 | §1.4 |
| `PeriInstancePort.bindNew.onSessionCreated` | `session/new` 返回后先登记索引/claim | §1.5 |
| `SnapshotBlock.argumentsError` | 仅接纳 `"payload_too_large"`，供投影映射公开 tool error | §1.6 |
| `FakePort` | 显式实现 `readPayload`；`bindNew` 支持回调与 gate | §2.2 |

未在 W0 修改（按 §2.1 属 W1）：`src/agent/{session,peri-instance,acp-mcp-bridge,interaction-broker,ask-user-question}.ts`、`src/state/interaction-projection.ts`、`src/chat/{service,manager,binding,close-chat,payload-access,service-*}.ts`、`src/view/chat-document.ts`。`PeriInstanceOptions.signal`/`PeriInstanceCloseOptions` 与 `PeriInstanceCloseIncompleteError` 由 W1-A 在 `peri-instance.ts` 落地。

## 5. 已裁决与待决

| 编号 | 结论 |
| --- | --- |
| D1 | 已裁决：CF 按工作树 `server/` + `core/` + `frontend/` 实施，不恢复 Worker/DO。 |
| D2 | 已裁决：同步采用 WebSocket + Yjs；删除 SSE 路由，`ChatYjsChannel` 只承载逻辑帧。 |
| D3 | 待裁决：本轮只做手动 `closeChat`/`shutdown`，不启用宿主空闲自动回收。 |
| D4 | 待裁决：加载历史保留持久 cwd、不发送 `_meta.peri.instructions`；MCP 声明是否随 load 重发按计划暂定重发。 |
| D5 | 待确认：删除公开 `Agent`/`ManagedAgents`/`SessionDocs`/`SessionViewStore` 与交互回调属于 breaking change，不留 shim。 |
| D6 | 待确认：`listSessions` 只返回目录事实，不再提供每会话运行态。 |
| D7 | 实现风险：内容寻址 payload ref 的预算/版本语义需在运行侧测试覆盖。 |
| D8 | 部署限制：CF Session claim 为单进程；多实例需注入共享原子 KV。 |

## 6. 验收 A01-A22

| 编号 | 场景 | 主要证据 |
| --- | --- | --- |
| A01 | 声明句柄 → 新建 → 发送 | create 不启动；返回真实 sessionId；真实 WASM 集成 |
| A02 | 空闲 A 换 B，B 加载失败 | 旧绑定/旧展示保留；无半份 B |
| A03 | A 执行时打开 B 再返回 A | A 不被 cancel/close；Manager 仍管理 A |
| A04 | 两句柄并发加载同 Session | 本地冲突带 ownerChatId；跨 Manager claim 冲突；无候选泄漏 |
| A05 | 同句柄并发加载同目标 | 合并同一 Promise；各调用方拿到结果；失败可重试 |
| A06 | 入队 → 投递/派发/撤回 | inputId 稳定；accepted 与投递分离；所有 waiter 结算 |
| A07 | 有输入等待时 cancel/answer | 控制不被队列挡住；沿用 ACP 语义 |
| A08 | 首屏 → 同页并发 → 失败重试 → 换会话 | 合并；失败不动 cursor；旧页不写新会话 |
| A09 | 无 View 执行 → 审批 → 挂两 View | 晚挂重建输出/队列/任务/交互 |
| A10 | 等审批销毁一个 View | 不自动拒绝/回答/取消；另一个正常 |
| A11 | A 的结果在选 B 后到达 | 只归 A；unknown 不重发；B 不污染 |
| A12 | session/new 成功、后续失败 | createdSessionId + stage；不盲目重复新建 |
| A13 | 断线、换代、schema 不支持 | replica 恢复/成对替换；unsupported ≠ 空历史 |
| A14 | 高频输出 + 慢客户端 | 无变化历史不重建；有界队列；正常客户端不被拖住 |
| A15 | 载荷读取中切会话 | 引用/字节/JSON/预算校验；旧结果不写新画面 |
| A16 | 启动失败/关闭未确认再清理 | 保留占用与原资源；不重开掩盖失败 |
| A17 | URL/factory-only/custom transport/不同配置 | 宿主分支均可用；历史 cwd/实例环境隔离 |
| A18 | 进程重启加载历史 | 可 load/续聊；不自动恢复旧执行；chat.id 非持久身份 |
| A19 | 沿生产链检查 send/cancel/close | 仍走 ACP；无 SDK 调度器/旧执行协议/平行会话表 |
| A20 | cancel 写出但未结束；close 未完成 | requested ≠ stopped；close 失败保留资源/占用/原因 |
| A21 | 加载历史、双会话 MCP、延迟事件 | 不覆盖 frozen；MCP/交互不串会话；历史不生成执行义务 |
| A22 | CF 编译、真实宿主、浏览器包检查 | 保留平台注入；客户端无运行依赖/模型/DB 凭证；未验证平台标记 blocked |

## 7. Phase 0 验证证据

| 命令（`npm-packages/@peri-sdk`） | 结果 |
| --- | --- |
| `bun run typecheck` | exit 0；`tsc --noEmit` 与 `tsc --noEmit -p tsconfig.demo.json` 均通过；`src` 全量纳入，新增 `src/chat/**`、`src/view/channel.ts` 被覆盖 |
| `bun test tests/chat-action.test.ts` | exit 0；16 pass / 0 fail；227 expect() calls |

### 7.1 独立验证 FAIL 项修复（2026-10-24）

`src/chat/json-value.ts` 新增 JSON 纯数据校验/深拷贝/深冻结；`src/chat/action.ts` 的工厂与 `parseChatUserAction` 共用该边界：

| 探针 | 结果 |
| --- | --- |
| answer content 含函数、BigInt、undefined、NaN/Infinity、循环引用、Date | 工厂与解析均抛 `ChatActionValidationError`（`invalid_action`），错误信息含字段路径；不再由 `JSON.stringify` 静默丢字段或抛 `TypeError` |
| 工厂返回后修改原始嵌套对象/数组 | Action 不受影响；嵌套对象、数组逐层冻结 |
| `JSON.parse(JSON.stringify(action))` | 与 action 深度相等，`requestId` 与嵌套 content 保留 |
| 纯空白 send（`"   "`、`"\n\t"`、`"\u00a0"`） | 工厂与解析均拒绝；非空白文本原样保留，不执行 trim |

已知限制（未在本阶段消除）：

1. `tests/` 不在任一 tsconfig 的 `include` 中，`bun run typecheck` 不检查测试类型。
2. 未运行 `bun run build`、完整 `bun test`、真实 WASM/ACP host/CF 集成；上述证据仅覆盖 Phase 0 定向契约。
3. 本修复记录为执行者自报，仍需独立验证复跑；不据此宣称 Phase 0 最终验收通过。
4. A01-A22 均未在本阶段执行；不得以上述命令冒充后续阶段或真实宿主验收。
