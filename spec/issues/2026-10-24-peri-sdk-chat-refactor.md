# Peri SDK Chat 架构重构

- 状态：**实现已提交（Phase 0–6）；A01-A22 全部 PASS；官方 WASM build、真实 WASM/workerd 运行后端与 CF 干净依赖已恢复；Hosted Workers/真实浏览器部署不在本轮范围**
- 分支：`feature/peri-cf-browser-agent`
- 实施提交：`64bfcbc48`（契约）、`e6ccbcd3d`（SDK 三层）、`c0520f625`（SDK 出口/工厂）、`cc447f789`（CF 服务端）、`966b6c7d7`（CF 前端）、`179677996`（文档）、`fb0535e34`（终验修复）、`272e2dcb4`（Timeline/A15 修复）；工具链/运行后端修复见本 issue 后续提交
- 事实源：本 issue；用户裁决与实施计划见工作区 `.peri/plans/`（非仓库长期事实源）。
- 本阶段边界：完成终验指出的换绑有界关闭、View 释放重试、A07/A10/A15 组合用例，修复 CF Timeline 计时竞态并同步文档/证据；不重构业务逻辑，不改 Rust/ACP wire/数据库表；提交由协调者按检查点执行，subagent 不 commit/push。

## 1. 范围

把 SDK 的 Chat 能力收敛为五个核心对象：

1. `ChatManager`：共享依赖快照、句柄目录、SessionOwnership 协调、单个/整体释放；不接收普通执行动作。
2. `ChatService`：单 Chat 的绑定、动作分派、事实日志与载荷读取；不持有 Manager，不依赖 Yjs。
3. `ChatUserAction`：八类可序列化用户意图，携带稳定 `requestId` 与纯数据 `payload`。
4. `PeriInstance`：一次激活的运行后端/Session 装配、交互/队列/历史/载荷控制与事实输出；内部实现。
5. `ChatView` / `ChatReplica`：服务端事实投影到 Yjs，浏览器副本只读并经选择器读取。

依赖方向固定为 `ChatManager → ChatService → PeriInstance → Session → Transport`；`ChatView` 只依赖
`ChatFactSource` 与 `ChatYjsChannel`。结构见 [Peri SDK Chat 设计](../../docs/design/peri-sdk-chat.md)。

## 2. 非范围

- 不修改 Rust、ACP wire 协议或数据库表结构。
- 不恢复 main 的 Cloudflare Worker/DO 目录；CF 消费方沿用工作树 `server/`、`core/`、`frontend/`。
- 不恢复 SSE `/events`、`/commands`、旧 Agent/ManagedAgents/SessionDocs 门面或兼容 shim。
- 不让 SDK 变成第二个调度器，不新增持久执行恢复状态。
- 不声明多实例 ownership、托管 Workers 或生产浏览器部署已验收。

## 3. 阶段状态

| 阶段 | 内容 | 当前状态 |
| --- | --- | --- |
| Phase 0 | 契约冻结与脚手架（action/types/errors/facts/channel 与定向测试） | 已提交 `64bfcbc48`；定向契约测试通过 |
| Phase 1 | 运行侧：Session 纯 ACP 客户端、PeriInstance、交互/历史/任务/载荷 | 已提交 `e6ccbcd3d`；单元/行为测试通过；真实 WASM 由 A18/A22 覆盖 |
| Phase 2 | Chat 控制面：Manager、Service、绑定状态机、SessionOwnership、回执 | 已提交 `e6ccbcd3d`/`c0520f625`；单元/行为测试通过 |
| Phase 3 | View/Yjs：ChatDocument、ChatView、ChatReplica、同步协议 | 已提交 `e6ccbcd3d`；单元/行为测试通过 |
| Phase 4 | SDK 集成：公开出口切换、删除旧层、工厂 | 已提交 `c0520f625`；`public-view` 负向断言通过 |
| Phase 5 | CF 服务端：ChatManager 接线、Action 路由、WS | 已提交 `cc447f789`；typecheck/test 通过；真实运行后端由 A22 覆盖 |
| Phase 6 | CF 前端：Remote Action/回执、Replica 选择器、WS 客户端 | 已提交 `966b6c7d7`；typecheck/test/Vite build 通过；浏览器验收未执行 |
| Phase 7 | 删除旧层、文档更新与验收证据 | 完成：换绑旧激活有界关闭、View 释放失败可重试（manager 与 HTTP DELETE 均可重试）；A07/A10/A15 组合用例补齐；CF Timeline 贴底状态改为随信号同步提交，全量测试连续 10 次稳定；官方 WASM build、真实 WASM/workerd 运行后端与 CF 干净依赖已验证 |

## 4. 已裁决与待决

| 编号 | 结论 |
| --- | --- |
| D1 | 已裁决：CF 按工作树 `server/` + `core/` + `frontend/` 实施，不恢复 Worker/DO。 |
| D2 | 已裁决：同步采用 WebSocket + Yjs；SSE 路由已删除，`ChatYjsChannel` 只承载逻辑帧。 |
| D3 | 已裁决（本轮范围）：只做手动 `closeChat`/`shutdown`，不启用运行后端空闲自动回收。 |
| D4 | 已裁决：加载历史保留持久 cwd、不发送 `_meta.peri.instructions`；MCP 声明随 load 重发（连接环境，不是 prompt 冻结内容）。 |
| D5 | 已按本阶段任务执行：旧公开门面删除，不留 shim；对外发布说明需标注 breaking change。 |
| D6 | 已执行：`listSessions` 只返回目录事实，不再提供每会话运行态；选中会话运行态来自副本。 |
| D7 | 已实现：内容寻址 payload ref 与运行侧预算；回归见 `tests/peri-instance.test.ts`、`tests/tool-payloads.test.ts`。 |
| D8 | 部署限制：CF SessionOwnership 默认 `createStorage()`（unstorage memory driver），仍是单进程；多实例需共享 storage，并由路由层保证单实例绑定、换绑/崩溃清理。 |
| D9 | 公开面：Session ownership 只公开 `KvStorage`（type）与 `clearSessionOwnership`（function）；`SessionOwnership` 协调器、`KvSessionOwnership`、`SessionOwnershipConflictError` 与 key 函数均为内部实现，不从主入口、`/portable` 或 `/view` 导出。 |

## 5. 验收 A01-A22

状态口径：PASS 表示有声明的自动化行为证据；BLOCKED 表示关键平台/真实运行后端证据不可得，不能以
typecheck 或旧 dist 冒充。测试名省略文件时均为 `npm-packages/@peri-sdk/tests/` 或
`npm-packages/peri-cf/` 下同名文件。

| 编号 | 场景 | 证据 | 状态 |
| --- | --- | --- | --- |
| A01 | 声明句柄 → 新建 → 发送 | `chat-manager.test.ts`：createChat 同步/不启动，create_new_session 绑定并取得 ownership；`chat-actions.test.ts`：execute 同步、accepted ≠ delivery；`chat-instance-factory.test.ts`：默认工厂；CF `server/api/index.test.ts`：空句柄 create_new_session。真实 WASM 证据归 A18/A22（BLOCKED） | PASS |
| A02 | 空闲 A 换 B，B 加载失败 | `chat-binding.test.ts`：candidate failure keeps old binding/revision/ownership；old close failure blocks；CF `frontend/remote/controller.test.ts`：A/B 隔离 | PASS |
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
| A16 | 启动失败/关闭未确认再清理 | `chat-close-bounded.test.ts`：startup close incomplete retains ownership/index、later close confirms；`chat-binding.test.ts`：换绑旧激活 close 挂起时有界返回 incomplete、保留旧 ownership/索引、恢复 close 后重试成功；`chat-manager.test.ts`：close failure retains；`peri-instance.test.ts`：unconfirmed cleanup blocks execution、retry | PASS |
| A17 | URL/factory-only/custom transport/不同配置 | `chat-instance-factory.test.ts`：transport/wasm、persisted cwd、abort；`wasm-loader.test.ts`：factory-only 不解析 moduleUrl；`session-start-cleanup.test.ts`：load persisted path | PASS |
| A18 | 进程重启加载历史 | 单元：`history-pagination.test.ts`、`session-start-cleanup.test.ts`、`chat-manager.test.ts` 覆盖 load/续聊与不重放旧输入；`tests/wasm-acp-integration.test.ts` 8/8 通过（真实打包 WASM + 真实 Turso listener + fixture 模型）：关闭后新 Chat/新 WASM 实例加载同一 Session，历史含前两轮输入与回复；`smoke:workers` 冷恢复 `session/list`→`session/load` 通过（`persistedAcrossRestart:true`）。OS 进程级重启未单独建模，证据为 WASM 运行后端重实例化与 workerd 冷恢复 | PASS |
| A19 | 沿生产链检查 send/cancel/close | `chat-actions.test.ts`、`peri-instance.test.ts`、CF `server/api/index.test.ts`；实现只走 ACP input/cancel/close，无 SDK dispatcher 或平行会话表 | PASS |
| A20 | cancel 写出但未结束；close 未完成 | `chat-actions.test.ts`：cancel requested≠stopped；`peri-instance.test.ts`：session/runtime close failure retains；`chat-close-bounded.test.ts`：incomplete preserves ownership/cause；CF controller：cancel requested | PASS |
| A21 | 加载历史、双会话 MCP、延迟事件 | `history-pagination.test.ts`：持久 cwd/分页；`acp-mcp-lifecycle.test.ts`：工厂双激活、owned close、失败清理；`peri-instance.test.ts`：late done；`chat-document.test.ts`：late chunk 不造幽灵 turn | PASS |
| A22 | CF 编译、真实运行后端、浏览器包检查 | SDK `MISE_ENV=wasm mise exec -- bun run build` exit 0（新 `dist`，无旧门面导出）；`test:wasm` 8/8；`smoke:workers` 在本地 workerd 通过（fixture 模型 3 次调用、sqld、SDK 运行后端、queue/prompt/冷恢复）；CF `bun run typecheck` 0、`bun test` 287 pass / 0 fail、`vite build` exit 0；浏览器包关键词检查无旧门面/SSE。Hosted Workers 部署未执行 | PASS |

汇总：**22 PASS / 0 PARTIAL / 0 BLOCKED**。A18/A22 的 PASS 以本轮真实 WASM、workerd、fixture 模型与 sqld 证据为准；Hosted Workers、真实浏览器部署与多实例 ownership 仍不在验收范围。独立复验指出的 A15 证据缺口已由组合用例补齐；CF 全量测试在修复后连续 10 次稳定通过。

## 6. 验证证据（2026-10-24 独立复验与修复）

### 6.1 成功项

> 下表是 2026-10-24 复验的历史记录。其中 `views.test.ts`、`sandbox.test.ts` 已在后续重构删除，
> 对应命令不可复跑；行内结论保留，不改写历史。

| 命令 | exit | 结果 |
| --- | --- | --- |
| `cd npm-packages/@peri-sdk && bun run typecheck` | 0 | `tsc --noEmit` 与 demo tsconfig 均通过 |
| `cd npm-packages/@peri-sdk && bun test tests/chat-close-bounded.test.ts tests/chat-binding.test.ts tests/chat-manager.test.ts tests/chat-actions.test.ts tests/chat-view.test.ts` | 0 | 52 pass / 0 fail；273 expect；5 files |
| `cd npm-packages/@peri-sdk && bun test tests/chat-*.test.ts tests/session-sync.test.ts tests/public-view.test.ts tests/sandbox.test.ts` | 0 | 150 pass / 0 fail；806 expect；13 files |
| `cd npm-packages/@peri-sdk && bun test tests test` | 0 | 427 pass / 0 fail；3017 expect；49 files（含 A15 新增组合用例） |
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
  `session-a` 的 ownership/索引/句柄保留，候选 `session-b` 清理确认后释放；恢复 `portA.close` 后
  `chat.close()` 成功释放全部 ownership。
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

### 6.2 工具链与发布门禁（本轮修复后）

| 命令 | exit | 关键输出 |
| --- | --- | --- |
| `MISE_ENV=wasm mise exec -- ./scripts/cargo-wasm.sh check -p peri-middlewares --lib --locked --target wasm32-unknown-emscripten` | 0 | `Finished … target(s) in 35.08s`；`context_archive` 仅在非 emscripten 目标编译 |
| `cd npm-packages/@peri-sdk && MISE_ENV=wasm mise exec -- bun run build` | 0 | release 构建完成；新 `dist/sdk/index.d.ts` 1.3 KB、无旧门面导出；`dist/view/index.js` 含 `pendingConnections`；`dist/wasm/{peri-wasm.js,peri_wasm.wasm}` 为新产物 |
| `cd npm-packages/@peri-sdk && bun test tests/wasm-acp-integration.test.ts tests/wasm-transport.test.ts` | 0 | 8 pass / 0 fail（真实打包 WASM、真实 Turso listener、fixture 模型） |
| `cd npm-packages/@peri-sdk && MISE_ENV=wasm mise exec -- bun run smoke:workers` | 0 | `{"passed":true,"modelCalls":3,"queueTurn":"queue","promptTurn":"prompt","persistedAcrossRestart":true}`（本地 workerd） |
| `cd npm-packages/peri-cf && bun install --force` | 0 | `node_modules/@peri-code/sdk` 为指向 `../@peri-sdk` 的符号链接，CF 直接消费最新 `dist` |
| `cd npm-packages/peri-cf && bun run typecheck && bun test` | 0 | typecheck 0；287 pass / 0 fail |
| `cd npm-packages/peri-cf && bun node_modules/vite/bin/vite.js build --outDir /tmp/peri-cf-dist --emptyOutDir` | 0 | 3441 modules；浏览器包无旧门面/SSE 关键词 |
| `bash scripts/check-file-size.sh` | 1 | 环境缺 `ripgrep (rg)`；改用 `wc -l` 人工核对 |

**修复点**：

- `peri-middlewares/src/mcp/{mod,middleware}.rs` 将 `context_archive` 模块与工具注册加
  `cfg(not(target_os = "emscripten"))`，与仅非 emscripten 的 `peri-mcp-common` 依赖一致；`E0433` 与连带
  的 `waiting()` 类型错误消失。
- `scripts/cargo-rmcp-patched.sh` 不再把目录或未配置版本的 mise shim 当 Python；优先可执行的
  `python3`，回退 `python`/`/usr/bin/python3`。
- `examples/workers/wrangler.toml` 把 `snippets/**/*.js` 纳入 ESModule 规则，workerd 能附加
  Emscripten 生成的 `turso_serverless-*/inline0.js`。
- 官方 build 需在 `mise exec` 下运行，以注入 `EMSDK`：`MISE_ENV=wasm mise exec -- bun run build`。
  CF 通过 `file:../@peri-sdk` + `bun install --force` 链接工作区包，无需手工复制。

### 6.3 文件规模与残留

- 本轮修改的源码/测试/文档文件均 ≤1000 行；最大 `src/agent/peri-instance.ts` 999 行，
  `src/chat/service.ts` 967 行（A15 修复前 941）。
- 全量 `wc -l` 仍列出两个既有超限 vendored 文件：`frontend/components/assistant-ui/elements/tool-fallback.aui.tsx`（1431）、`thread.aui.tsx`（1166）。这是**截至 2026-10-24 的历史文件清单；两者已于 2026-10-26 删除**。本轮未改动；按 `STD-SIZE-001` 另立任务治理，不宣称全库通过。
- 生产源码无 `TODO`/`FIXME`，无新增兼容 shim；旧符号只保留在负向测试与明确标注的历史文档中。
- `npm-packages/peri-cf/docker-compose.yml` 为未跟踪文件，未纳入任何提交。工作树副本在 2026-10-25 22:26:02 发生外部变更（size 2765→3082，sha256 `a8af2136…`→`655be552…`）；用户裁决不处理该变更，本轮不修改、不恢复该文件。
- **CF 测试稳定性**：`Timeline.dom.test.tsx` 的“迟到跨会话信号”用例在修复前 8 路并发压力下
  32 次全量失败 4 次；修复后单文件连续 10 次、全量连续 10 次均为 0 fail，压力下 32/32 通过。
- **HTTP DELETE 重试已打通**：首次关闭移除句柄后若 View 释放失败，`AppChatManager` 保留 View 条目并
  通过 `hasPendingClose(chatId)` 暴露残留下沉；DELETE 在句柄解析失败时检查该信号，同一 `chatId` 的
  第二次请求进入 `closeChat` 重试释放 View，成功后 204。首次失败仍返回 500 `close_incomplete`，
  句柄已移除时 WS 升级继续返回 410，不能建立新 View。残留重试以错误响应中的 `chatId` 寻址。

## 7. 已知限制

1. `tests/` 不在任一 tsconfig 的 `include` 中，`bun run typecheck` 不检查测试类型。
2. A18/A22 的 PASS 覆盖真实打包 WASM、真实 Turso listener、fixture 模型与本地 workerd；Hosted
   Workers 部署、真实浏览器与真实模型供应商未执行。OS 进程级重启未单独建模，A18 以 WASM 运行后端
   重实例化与 workerd 冷恢复为证据。
3. 官方 build 依赖 `MISE_ENV=wasm mise exec --` 注入 `EMSDK`；直接 `bun run build` 在仅有 mise shim
   的环境会把 shim 路径当作工具链并失败，文档已给出正确命令。
4. 独立复验已完成；A01-A22 的 PASS 仍只覆盖所列自动化/本机运行后端证据，不代表托管部署验收。
5. Session ownership key 改为 `ownership:<base64url(sessionId)>`，value 为 `{ sessionId, owner }`。旧 key `peri:managed-agent:session:<encodeURIComponent(id)>` 与新 key 互不可见；混合版本会各自 acquire 成功，**必须停机切换**（drain 旧实例 → 全量部署 → 放开准入），回滚同样需要全量停机。SDK 不做 TTL/续租/CAS；acquire 是读后写、非原子，KV 记录是绑定标记/防御路由 bug，不是互斥原语；跨 Manager 并发 acquire 不再保证互斥，只有路由层串行化能保证单实例绑定。错误映射：加载路径冲突 409 `binding_conflict`、KV 读/写失败 500 `claim_failed`；新会话 claim 阶段失败 500 `session_creation_failed`（stage `claim`）；没有 ownership 专用 503。
6. HTTP DELETE 的残留重试按 `chatId` 寻址；若首次 DELETE 使用 Session ID 且句柄移除后 View 仍残留，
   需用 500 响应体中的 `chatId` 重试（GC/app 入口均使用 chat id，不提供 Session ID tombstone）。
