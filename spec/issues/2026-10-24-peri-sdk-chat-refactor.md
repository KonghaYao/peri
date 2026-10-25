# Peri SDK Chat 架构重构

- 状态：**实现已提交（Phase 0–6）；独立复验指出的 P1（CF Timeline 间歇失败、A15 组合证据）与 P2（证据校准）已修复；真实 WASM/宿主与 SDK 发布门禁仍 BLOCKED**
- 分支：`feature/peri-cf-browser-agent`
- 实施提交：`64bfcbc48`（契约）、`e6ccbcd3d`（SDK 三层）、`c0520f625`（SDK 出口/工厂）、`cc447f789`（CF 服务端）、`966b6c7d7`（CF 前端）
- 事实源：本 issue；用户裁决与实施计划见工作区 `.peri/plans/`（非仓库长期事实源）。
- 本阶段边界：完成终验指出的换绑有界关闭、View 释放重试、A07/A10/A15 组合用例，修复 CF Timeline 计时竞态并同步文档/证据；不重构业务逻辑，不改 Rust/ACP wire/数据库表，不 commit/push。

## 1. 范围

把 SDK 的 Chat 能力收敛为五个核心对象：

1. `ChatManager`：共享依赖快照、句柄目录、Session claim 协调、单个/整体释放；不接收普通执行动作。
2. `ChatService`：单 Chat 的绑定、动作分派、事实日志与载荷读取；不持有 Manager，不依赖 Yjs。
3. `ChatUserAction`：八类可序列化用户意图，携带稳定 `requestId` 与纯数据 `payload`。
4. `PeriInstance`：一次激活的宿主/Session 装配、交互/队列/历史/载荷控制与事实输出；内部实现。
5. `ChatView` / `ChatReplica`：服务端事实投影到 Yjs，浏览器副本只读并经选择器读取。

依赖方向固定为 `ChatManager → ChatService → PeriInstance → Session → Transport`；`ChatView` 只依赖
`ChatFactSource` 与 `ChatYjsChannel`。结构见 [Peri SDK Chat 设计](../../docs/design/peri-sdk-chat.md)。

## 2. 非范围

- 不修改 Rust、ACP wire 协议或数据库表结构。
- 不恢复 main 的 Cloudflare Worker/DO 目录；CF 消费方沿用工作树 `server/`、`core/`、`frontend/`。
- 不恢复 SSE `/events`、`/commands`、旧 Agent/ManagedAgents/SessionDocs 门面或兼容 shim。
- 不让 SDK 变成第二个调度器，不新增持久执行恢复状态。
- 不声明多实例 claim、托管 Workers 或生产浏览器部署已验收。

## 3. 阶段状态

| 阶段 | 内容 | 当前状态 |
| --- | --- | --- |
| Phase 0 | 契约冻结与脚手架（action/types/errors/facts/channel 与定向测试） | 已提交 `64bfcbc48`；定向契约测试通过 |
| Phase 1 | 运行侧：Session 纯 ACP 客户端、PeriInstance、交互/历史/任务/载荷 | 已提交 `e6ccbcd3d`；单元/行为测试通过；真实 WASM BLOCKED |
| Phase 2 | Chat 控制面：Manager、Service、绑定状态机、claims、回执 | 已提交 `e6ccbcd3d`/`c0520f625`；单元/行为测试通过 |
| Phase 3 | View/Yjs：ChatDocument、ChatView、ChatReplica、同步协议 | 已提交 `e6ccbcd3d`；单元/行为测试通过 |
| Phase 4 | SDK 集成：公开出口切换、删除旧层、工厂 | 已提交 `c0520f625`；`public-view` 负向断言通过 |
| Phase 5 | CF 服务端：ChatManager 接线、Action 路由、WS | 已提交 `cc447f789`；typecheck/test 通过；真实宿主 BLOCKED |
| Phase 6 | CF 前端：Remote Action/回执、Replica 选择器、WS 客户端 | 已提交 `966b6c7d7`；typecheck/test/Vite build 通过；浏览器验收未执行 |
| Phase 7 | 删除旧层、文档更新与验收证据 | 完成：换绑旧激活有界关闭、View 释放失败可重试（manager 与 HTTP DELETE 均可重试）；A07/A10/A15 组合用例补齐；CF Timeline 贴底状态改为随信号同步提交，全量测试连续 10 次稳定；WASM/真实宿主/发布门禁 BLOCKED |

## 4. 已裁决与待决

| 编号 | 结论 |
| --- | --- |
| D1 | 已裁决：CF 按工作树 `server/` + `core/` + `frontend/` 实施，不恢复 Worker/DO。 |
| D2 | 已裁决：同步采用 WebSocket + Yjs；SSE 路由已删除，`ChatYjsChannel` 只承载逻辑帧。 |
| D3 | 已裁决（本轮范围）：只做手动 `closeChat`/`shutdown`，不启用宿主空闲自动回收。 |
| D4 | 已裁决：加载历史保留持久 cwd、不发送 `_meta.peri.instructions`；MCP 声明随 load 重发（连接环境，不是 prompt 冻结内容）。 |
| D5 | 已按本阶段任务执行：旧公开门面删除，不留 shim；对外发布说明需标注 breaking change。 |
| D6 | 已执行：`listSessions` 只返回目录事实，不再提供每会话运行态；选中会话运行态来自副本。 |
| D7 | 已实现：内容寻址 payload ref 与运行侧预算；回归见 `tests/peri-instance.test.ts`、`tests/tool-payloads.test.ts`。 |
| D8 | 部署限制：CF Session claim 默认单进程 `MemoryKV`；多实例需注入共享原子 KV。 |

## 5. 验收 A01-A22

状态口径：PASS 表示有声明的自动化行为证据；BLOCKED 表示关键平台/真实宿主证据不可得，不能以
typecheck 或旧 dist 冒充。测试名省略文件时均为 `npm-packages/@peri-sdk/tests/` 或
`npm-packages/peri-cf/` 下同名文件。

| 编号 | 场景 | 证据 | 状态 |
| --- | --- | --- | --- |
| A01 | 声明句柄 → 新建 → 发送 | `chat-manager.test.ts`：createChat 同步/不启动，create_new_session 绑定并 claim；`chat-actions.test.ts`：execute 同步、accepted ≠ delivery；`chat-instance-factory.test.ts`：默认工厂；CF `server/api/index.test.ts`：空句柄 create_new_session。真实 WASM 证据归 A18/A22（BLOCKED） | PASS |
| A02 | 空闲 A 换 B，B 加载失败 | `chat-binding.test.ts`：candidate failure keeps old binding/revision/claim；old close failure blocks；CF `frontend/remote/controller.test.ts`：A/B 隔离 | PASS |
| A03 | A 执行时打开 B 再返回 A | `chat-binding.test.ts`：running rejects switch without cancelling/closing A；CF controller：opening B releases local connection only | PASS |
| A04 | 两句柄并发加载同 Session | `chat-binding.test.ts`：index visible during path resolution；`chat-manager.test.ts`：same manager ownerChatId、two managers one winner | PASS |
| A05 | 同句柄并发加载同目标 | `chat-binding.test.ts`：concurrent loads merge into one ACP load | PASS |
| A06 | 入队 → 投递/派发/撤回 | `chat-actions.test.ts`：accepted ≠ delivery、take_back rejects waiter；`session-start-cleanup.test.ts`：delivery resolves waiters；`peri-instance.test.ts`：queue/send/delivery | PASS |
| A07 | 有输入等待时 cancel/answer | `chat-actions.test.ts`：cancel requested/idle、force_send/answer/history 直接转发；新增“双会话输入等待中并发 cancel/answer”用例：两个 sendGate 未结算时并发 cancel(A)/answer(B)，cancel 只到达 A、answer 只到达 B，pending input 保留且不误取消；`peri-instance.test.ts`：permission answers settle ACP request | PASS |
| A08 | 首屏 → 同页并发 → 失败重试 → 换会话 | `history-pagination.test.ts`：same cursor 合并、失败保留 cursor、rewind 后拒页、不同 snapshot 整页拒绝、close 后丢弃 | PASS |
| A09 | 无 View 执行 → 审批 → 挂两 View | `chat-view.test.ts`：late View rebuilds output/queue/tasks/interactions；two Views share facts | PASS |
| A10 | 等审批销毁一个 View | 新增 `chat-view.test.ts` 组合用例：同一会话存在待决 interaction 时晚挂 View 重放该 interaction；dispose 一个 View 后另一个仍 synced、pendingInteractions 保留并继续收事实 | PASS |
| A11 | A 的结果在选 B 后到达 | `chat-actions.test.ts`：late results keep admitted target；`history-pagination.test.ts`：different snapshot page rejected；`peri-instance.test.ts`：late done for another request；CF controller A/B 隔离 | PASS |
| A12 | session/new 成功、后续失败 | `chat-manager.test.ts`：partial creation surfaces createdSessionId/stage；`peri-instance.test.ts`：rename failure stage；CF API：createdSessionId/stage | PASS |
| A13 | 断线、换代、schema 不支持 | `chat-replica.test.ts`：HELLO resume、gap repair、generation mismatch、unsupported ≠ empty；`session-sync.test.ts`：resume/替换；CF `server/api/sync.test.ts`：reconnect resume；controller unsupported | PASS |
| A14 | 高频输出 + 慢客户端 | `chat-view.test.ts`：mute client credit/deadline close；`session-sync.test.ts`：slow subscriber bounded queue、fast subscribers current；CF sync：backpressure close；`chat-replica.test.ts`：read coalescing | PASS |
| A15 | 载荷读取中切会话 | 新增 `payload-reader.test.ts` 组合用例：旧绑定上的两个读取（一个晚到成功、一个晚到失败）在 `changeSessionTo` 后都按发起代次丢弃为 `unavailable`，新 binding/文档目标不变、未产生新事实、新端口未收到旧读取；`chat-actions.test.ts`：late result target；`payload-reader.test.ts`：字节/JSON/预算；`peri-instance.test.ts`：content-addressed refs、integrity/unavailable、budget；CF `payloadReader.test.ts`：引用不在当前会话时显式失败 | PASS |
| A16 | 启动失败/关闭未确认再清理 | `chat-close-bounded.test.ts`：startup close incomplete retains claim/index、later close confirms；`chat-binding.test.ts`：换绑旧激活 close 挂起时有界返回 incomplete、保留旧 claim/索引、恢复 close 后重试成功；`chat-manager.test.ts`：close failure retains；`peri-instance.test.ts`：unconfirmed cleanup blocks execution、retry | PASS |
| A17 | URL/factory-only/custom transport/不同配置 | `chat-instance-factory.test.ts`：transport/wasm、persisted cwd、abort；`wasm-loader.test.ts`：factory-only 不解析 moduleUrl；`session-start-cleanup.test.ts`：load persisted path | PASS |
| A18 | 进程重启加载历史 | 单元：`history-pagination.test.ts`、`session-start-cleanup.test.ts`、`chat-manager.test.ts` 覆盖 load/续聊与不重放旧输入；真实进程重启/WASM 证据需 `tests/wasm-acp-integration.test.ts`，当前 BLOCKED | BLOCKED |
| A19 | 沿生产链检查 send/cancel/close | `chat-actions.test.ts`、`peri-instance.test.ts`、CF `server/api/index.test.ts`；实现只走 ACP input/cancel/close，无 SDK dispatcher 或平行会话表 | PASS |
| A20 | cancel 写出但未结束；close 未完成 | `chat-actions.test.ts`：cancel requested≠stopped；`peri-instance.test.ts`：session/host close failure retains；`chat-close-bounded.test.ts`：incomplete preserves claim/cause；CF controller：cancel requested | PASS |
| A21 | 加载历史、双会话 MCP、延迟事件 | `history-pagination.test.ts`：持久 cwd/分页；`acp-mcp-lifecycle.test.ts`：工厂双激活、owned close、失败清理；`peri-instance.test.ts`：late done；`chat-document.test.ts`：late chunk 不造幽灵 turn | PASS |
| A22 | CF 编译、真实宿主、浏览器包检查 | `bun run typecheck` exit 0；修复后 `bun test` 连续 10 次均为 287 pass / 0 fail；独立复验的 `vite build` exit 0 与浏览器包关键词检查通过。真实 WASM/模型/Store 宿主未执行，SDK build 仍 BLOCKED | BLOCKED |

汇总：**20 PASS / 0 PARTIAL / 2 BLOCKED**。独立复验指出的 A15 证据缺口已由组合用例补齐；CF 全量测试在修复后连续 10 次稳定通过。BLOCKED 项仍是 A18（真实进程重启/WASM）与 A22（真实宿主）。发布门禁仍因 SDK 官方 build/干净 CI 不可复现而 BLOCKED，不能用 A01-A22 的 PASS 代替发布验收。

## 6. 验证证据（2026-10-24 独立复验与修复）

### 6.1 成功项

| 命令 | exit | 结果 |
| --- | --- | --- |
| `cd npm-packages/@peri-sdk && bun run typecheck` | 0 | `tsc --noEmit` 与 demo tsconfig 均通过 |
| `cd npm-packages/@peri-sdk && bun test tests/chat-close-bounded.test.ts tests/chat-binding.test.ts tests/chat-manager.test.ts tests/chat-actions.test.ts tests/chat-view.test.ts` | 0 | 52 pass / 0 fail；273 expect；5 files |
| `cd npm-packages/@peri-sdk && bun test tests/chat-*.test.ts tests/session-sync.test.ts tests/public-view.test.ts tests/sandbox.test.ts` | 0 | 150 pass / 0 fail；806 expect；13 files |
| `cd npm-packages/@peri-sdk && bun test tests test` | 0 | 409 pass / 0 fail；2945 expect；49 files（含 A15 新增组合用例） |
| `cd npm-packages/peri-cf && bun run typecheck` | 0 | 根 TS 与 `server/tsconfig.json` 均通过 |
| `cd npm-packages/peri-cf && bun test server/agent/views.test.ts server/api/index.test.ts` | 0 | 20 pass / 0 fail；73 expect；2 files |
| `cd npm-packages/peri-cf && bun test frontend/chat/timeline/Timeline.dom.test.tsx` | 0 | 连续 10 次均为 4 pass / 0 fail |
| `cd npm-packages/peri-cf && bun test` | 0 | 修复后连续 10 次均为 287 pass / 0 fail；另在 8 路并发压力下 32/32 次全量通过 |
| `cd npm-packages/peri-cf && ./node_modules/.bin/vite build` | 0 | 3441 modules；仅有 >500 kB chunk 提示 |
| `git diff --check` | 0 | 无空白错误 |
| 改动文件 `wc -l` | 0 | 最大为 `src/agent/peri-instance.ts` 999 行；`src/chat/service.ts` 967 行（A15 修复前 941）；全部 ≤1000 |

关键行为证据：

- **换绑有界关闭**：`chat-binding.test.ts` 新增用例让 `portA.close` 永不 resolve，`changeSessionTo("session-b")`
  在 25ms 预算内以 `binding_cleanup_failed`/`unknown` 返回；`phase=blocked`（不停留在 `switching`），
  `session-a` 的 claim/索引/句柄保留，候选 `session-b` 清理确认后释放；恢复 `portA.close` 后
  `chat.close()` 成功释放全部 claim。
- **View 释放重试**：`views.test.ts` 首次 `channel.close` 抛错时条目保留、source 已退订；第二次
  `dispose` 重试并删除条目，channel 关闭。`manager.test.ts` 验证 View 释放失败后句柄已移除、
  `hasPendingClose` 为 true，随后通过 HTTP DELETE 重试返回 204；`api/index.test.ts` 覆盖
  “句柄已移除、View 释放首次失败”的 DELETE：首次 500 `close_incomplete`，第二次同 `chatId`
  重试返回 204 且无残留。
- **A07**：双会话输入等待中并发 `cancel(A)`/`answer(B)`，cancel 只调用 A 端口、answer 只写 B，
  两个待投递输入都保留并可继续投递。
- **A10**：同一会话待决 interaction 下晚挂 View 重放该 interaction；dispose 一个 View 后另一个
  仍 synced、`pendingInteractions` 保留并继续收事实。
- **CF Timeline 稳定性**：跨会话迟到信号必须被忽略；当前会话自己的信号在 effect 内同步提交
  贴底状态（收起“回到底部”、记录位置），`scrollToEnd` 仍延后一帧等待新内容布局。测试不再用
  固定 40ms 猜测动画帧，而是等状态与帧完成。修复前 8 路并发压力下 32 次全量失败 4 次（该用例
  约 1.4–1.6s）；同一新用例在旧实现上确定性失败（1 fail），修复后通过。修复后压力下 32/32、
  最终版本 24/24 通过，顺序全量连续 10 次通过。
- **A15**：`ChatService.readPayload` 在发起时捕获当前激活端口与 `bindingRevision`；读取结算后
  若绑定已切换/关闭，晚到成功结果和晚到错误都转换为 `PayloadReadError("unavailable")` 丢弃。
  组合用例让旧绑定两个读取挂起，`changeSessionTo("session-b")` 后再分别 resolve/reject：
  两个 Promise 均为 `unavailable`，`ChatView.binding` 仍是 B、事实观察器没有收到新绑定/事实、
  新端口没有收到旧读取，随后 B 的读取只经新端口成功返回。

### 6.2 失败/阻塞项

| 命令 | exit | 关键输出 |
| --- | --- | --- |
| `cd npm-packages/@peri-sdk && bun run build` | 1 | 首个错误：`Cloudflare Tokio patches require Emscripten 6.0.10: /Users/mino/.local/share/mise/shims`；随后 `scripts/cargo-wasm.sh exited with status 1`，未进入 peri-wasm 构建与 SDK 打包 |
| `cd npm-packages/@peri-sdk && bun run test:wasm` | 1 | 同一 Emscripten 前置失败；真实 WASM ACP 未运行 |
| 干净 CI 发布门禁 | BLOCKED | 官方 SDK build 无法完成；CF 的 typecheck/test 依赖预构建 `node_modules/@peri-code/sdk`，**干净 CI 不可复现**，不能宣称发布门禁通过 |
| `bash scripts/check-file-size.sh` | 1 | `❌ 需要 ripgrep (rg)`；改用 `wc -l` 人工核对 |

**SDK dist 前置与发布面**：官方 `bun run build` 仍未完成。独立复验确认：

- 根 `dist/index.js` 与 HEAD 源码重新 bundle 的哈希一致，CF 副本主入口也一致；
- 但根 `dist/sdk/index.d.ts` 仍导出已删除的 `agent/agent`/`session-docs`；
- `dist/portable.js`/`.d.ts`、`dist/view/index.js`/`.d.ts`（根 `/view` 没有 `ChatView`/`ChatReplica`）
  与 `dist/wasm/*` 仍是旧产物；
- CF 预构建副本 `dist/view/index.js` 不含终验的 retry 逻辑（`pendingConnections` 为 0 处）。

为让 CF 测试覆盖本轮 SDK 源码，本轮额外执行了局部 JS bundle（只替换 `dist/index.js`，未重建
wasm/声明）：

```bash
cd npm-packages/@peri-sdk
bun -e 'const r = await Bun.build({ entrypoints: ["./src/sdk/index.ts"], outdir: "./dist", target: "bun", format: "esm", external: ["yjs"] }); if (!r.success) { console.error(r.logs); process.exit(1); }'
cp dist/index.js ../peri-cf/node_modules/@peri-code/sdk/dist/index.js
```

这是**本地局部预构建**，不是干净构建，也不能作为发布证据。CF 结果只证明当前源码在该预构建依赖下
通过；干净 CI 必须先成功执行官方 SDK build 并把最新包安装/复制到 CF，当前工具链下不可复现。
因此 **SDK 发布门禁 BLOCKED**。

### 6.3 文件规模与残留

- 本轮修改的源码/测试/文档文件均 ≤1000 行；最大 `src/agent/peri-instance.ts` 999 行，
  `src/chat/service.ts` 967 行（A15 修复前 941）。
- 全量 `wc -l` 仍列出两个既有超限 vendored 文件：`frontend/components/assistant-ui/elements/tool-fallback.aui.tsx`（1431）、`thread.aui.tsx`（1166）。本轮未改动；按 `STD-SIZE-001` 另立任务治理，不宣称全库通过。
- 生产源码无 `TODO`/`FIXME`，无新增兼容 shim；旧符号只保留在负向测试与明确标注的历史文档中。
- `npm-packages/peri-cf/docker-compose.yml` 为未跟踪文件，本轮未触碰。
- **CF 测试稳定性**：`Timeline.dom.test.tsx` 的“迟到跨会话信号”用例在修复前 8 路并发压力下
  32 次全量失败 4 次；修复后单文件连续 10 次、全量连续 10 次均为 0 fail，压力下 32/32 通过。
- **HTTP DELETE 重试已打通**：首次关闭移除句柄后若 View 释放失败，`AppChatManager` 保留 View 条目并
  通过 `hasPendingClose(chatId)` 暴露残留下沉；DELETE 在句柄解析失败时检查该信号，同一 `chatId` 的
  第二次请求进入 `closeChat` 重试释放 View，成功后 204。首次失败仍返回 500 `close_incomplete`，
  句柄已移除时 WS 升级继续返回 410，不能建立新 View。残留重试以错误响应中的 `chatId` 寻址。

## 7. 已知限制

1. `tests/` 不在任一 tsconfig 的 `include` 中，`bun run typecheck` 不检查测试类型。
2. 真实 WASM ACP、进程重启续聊、hosted Workers 与浏览器/真实模型验收未完成；A18/A22 标记 BLOCKED。
3. CF 的单测与 typecheck 依赖预构建 SDK dist；根 `dist` 的 declaration、`/portable`、`/view` 与
   `dist/wasm/*` 仍是旧产物，CF 副本 `/view` 不含终验 retry；官方 build 与干净 CI 仍 BLOCKED，
   不能用该结果宣称发布门禁通过。
4. 独立复验已完成，其 P1/P2 两项已在本轮修复；A01-A22 的 PASS 仍只覆盖所列自动化证据，
   A07/A10/A15 的 PASS 来自新增组合单测，不代表浏览器/真实宿主验收。
5. `sessionClaimKey` 保留既有协调域前缀 `peri:managed-agent:session:`。它是存储 key，不是公开符号；改名会破坏升级期跨版本 claim 互斥，需单独迁移，不在本轮修改。
6. HTTP DELETE 的残留重试按 `chatId` 寻址；若首次 DELETE 使用 Session ID 且句柄移除后 View 仍残留，
   需用 500 响应体中的 `chatId` 重试（GC/app 入口均使用 chat id，不提供 Session ID tombstone）。
