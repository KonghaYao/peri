# Peri SDK Chat 架构重构

- 状态：**实现已提交（Phase 0–6）；Phase 7 文档与旧符号收口完成；真实 WASM/宿主验收 BLOCKED，待独立复验**
- 分支：`feature/peri-cf-browser-agent`
- 实施提交：`64bfcbc48`（契约）、`e6ccbcd3d`（SDK 三层）、`c0520f625`（SDK 出口/工厂）、`cc447f789`（CF 服务端）、`966b6c7d7`（CF 前端）
- 事实源：本 issue；用户裁决与实施计划见工作区 `.peri/plans/`（非仓库长期事实源）。
- 本阶段边界：只做文档、证据与旧符号收口；不重构业务逻辑，不改 Rust/ACP wire/数据库表，不改 CF 行为，不 commit/push。

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
| Phase 7 | 删除旧层、文档更新与验收证据 | 本轮完成文档/旧符号收口；WASM/全量门 BLOCKED |

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
| A07 | 有输入等待时 cancel/answer | `chat-actions.test.ts`：cancel requested/idle、force_send/answer/history 直接转发；`peri-instance.test.ts`：permission answers settle ACP request。未设专门“等待队列时并发”用例，控制路径不经过 input 队列 | PASS |
| A08 | 首屏 → 同页并发 → 失败重试 → 换会话 | `history-pagination.test.ts`：same cursor 合并、失败保留 cursor、rewind 后拒页、不同 snapshot 整页拒绝、close 后丢弃 | PASS |
| A09 | 无 View 执行 → 审批 → 挂两 View | `chat-view.test.ts`：late View rebuilds output/queue/tasks/interactions；two Views share facts | PASS |
| A10 | 等审批销毁一个 View | `chat-view.test.ts`：dispose 一个后另一个仍 synced 且继续收事实。未专门组合“同一待决审批”场景，但 View 无回答/取消入口，销毁不触发 ACP 结算 | PASS |
| A11 | A 的结果在选 B 后到达 | `chat-actions.test.ts`：late results keep admitted target；`history-pagination.test.ts`：different snapshot page rejected；`peri-instance.test.ts`：late done for another request；CF controller A/B 隔离 | PASS |
| A12 | session/new 成功、后续失败 | `chat-manager.test.ts`：partial creation surfaces createdSessionId/stage；`peri-instance.test.ts`：rename failure stage；CF API：createdSessionId/stage | PASS |
| A13 | 断线、换代、schema 不支持 | `chat-replica.test.ts`：HELLO resume、gap repair、generation mismatch、unsupported ≠ empty；`session-sync.test.ts`：resume/替换；CF `server/api/sync.test.ts`：reconnect resume；controller unsupported | PASS |
| A14 | 高频输出 + 慢客户端 | `chat-view.test.ts`：mute client credit/deadline close；`session-sync.test.ts`：slow subscriber bounded queue、fast subscribers current；CF sync：backpressure close；`chat-replica.test.ts`：read coalescing | PASS |
| A15 | 载荷读取中切会话 | `chat-actions.test.ts`：late result target；`payload-reader.test.ts`：字节/JSON/预算；`peri-instance.test.ts`：content-addressed refs、integrity/unavailable、budget；CF `payloadReader.test.ts`：引用不在当前会话时显式失败 | PASS |
| A16 | 启动失败/关闭未确认再清理 | `chat-close-bounded.test.ts`：startup close incomplete retains claim/index、later close confirms；`chat-manager.test.ts`：close failure retains；`peri-instance.test.ts`：unconfirmed cleanup blocks execution、retry | PASS |
| A17 | URL/factory-only/custom transport/不同配置 | `chat-instance-factory.test.ts`：transport/wasm、persisted cwd、abort；`wasm-loader.test.ts`：factory-only 不解析 moduleUrl；`session-start-cleanup.test.ts`：load persisted path | PASS |
| A18 | 进程重启加载历史 | 单元：`history-pagination.test.ts`、`session-start-cleanup.test.ts`、`chat-manager.test.ts` 覆盖 load/续聊与不重放旧输入；真实进程重启/WASM 证据需 `tests/wasm-acp-integration.test.ts`，当前 BLOCKED | BLOCKED |
| A19 | 沿生产链检查 send/cancel/close | `chat-actions.test.ts`、`peri-instance.test.ts`、CF `server/api/index.test.ts`；实现只走 ACP input/cancel/close，无 SDK dispatcher 或平行会话表 | PASS |
| A20 | cancel 写出但未结束；close 未完成 | `chat-actions.test.ts`：cancel requested≠stopped；`peri-instance.test.ts`：session/host close failure retains；`chat-close-bounded.test.ts`：incomplete preserves claim/cause；CF controller：cancel requested | PASS |
| A21 | 加载历史、双会话 MCP、延迟事件 | `history-pagination.test.ts`：持久 cwd/分页；`acp-mcp-lifecycle.test.ts`：工厂双激活、owned close、失败清理；`peri-instance.test.ts`：late done；`chat-document.test.ts`：late chunk 不造幽灵 turn | PASS |
| A22 | CF 编译、真实宿主、浏览器包检查 | CF `bun run typecheck` exit 0；`bun test` 284 pass；`vite build` exit 0；`dist/client` 凭据/Node/DB 关键词 0 命中。真实 WASM/模型/Store 宿主未执行，SDK build BLOCKED | BLOCKED |

汇总：**20 PASS / 2 BLOCKED**。BLOCKED 项是 A18（真实进程重启/WASM）与 A22（真实宿主）；其余
PASS 均为自动化行为/接线证据，不表示浏览器或平台验收完成。

## 6. 验证证据（2026-10-25 本阶段执行）

### 6.1 成功项

| 命令 | exit | 结果 |
| --- | --- | --- |
| `cd npm-packages/@peri-sdk && bun run typecheck` | 0 | `tsc --noEmit` 与 demo tsconfig 均通过 |
| `cd npm-packages/@peri-sdk && bun test tests/chat-{action,manager,binding,actions,close-bounded,view,replica,document,instance-factory}.test.ts tests/peri-instance.test.ts tests/history-pagination.test.ts tests/acp-mcp-lifecycle.test.ts tests/public-view.test.ts` | 0 | 161 pass / 0 fail；906 expect；13 files |
| `cd npm-packages/peri-cf && bun run typecheck` | 0 | 根 TS 与 `server/tsconfig.json` 均通过 |
| `cd npm-packages/peri-cf && bun test` | 0 | 284 pass / 0 fail；851 expect；39 files |
| `cd npm-packages/peri-cf && ./node_modules/.bin/vite build` | 0 | 3441 modules；`dist/client` 产物生成；仅有 chunk >500 kB 警告 |
| 浏览器包关键词检查 | 0 | `PERI_MODEL_API_KEY`、`PERI_STORAGE_TOKEN`、`node:fs`、`@tursodatabase`、`PERI_MACHINE_ID` 均 0 命中 |
| `git diff --check` | 0 | 无空白错误 |
| `bash scripts/check-changed.sh` | 0 | 改动未命中 workspace crate，按设计跳过 cargo check |

### 6.2 失败/阻塞项

| 命令 | exit | 关键输出 |
| --- | --- | --- |
| `cd npm-packages/@peri-sdk && bun run build` | 1 | `Cloudflare Tokio patches require Emscripten 6.0.10: /Users/mino/.local/share/mise/shims`；`emcc` shim 未配置版本；未进入 Bun bundle/声明生成 |
| `cd npm-packages/@peri-sdk && bun test tests test` | 1 | 403 pass / 1 fail / 1 error；2873 expect；404 tests / 49 files。`tests/wasm-acp-integration.test.ts`：`SyntaxError: Export named 'AtomicSessionClaims' not found in module .../dist/index.js`（本地 `dist` 仍是旧构建） |
| `cd npm-packages/@peri-sdk && bun run test:wasm` | 1 | 同一 Emscripten 前置失败；未运行真实 WASM |
| `bash scripts/check-file-size.sh` | 1 | `❌ 需要 ripgrep (rg)`；改用 `wc -l` 人工核对 |

**SDK dist 前置**：本阶段未能重建 `npm-packages/@peri-sdk/dist`（工具链阻塞）。CF 的
`node_modules/@peri-code/sdk` 是 Phase 6 重建的副本（`dist/chat/*`、`dist/agent/peri-instance.d.ts`
存在，mtime 晚于 SDK 源码），因此 CF typecheck/test 使用的是该预构建依赖，**不是本阶段干净构建**。
干净 CI 必须先执行 `cd npm-packages/@peri-sdk && bun run build` 并安装/复制最新包；否则会像 SDK
全量测试一样因旧 dist 缺失新导出而失败。该限制已写入 CF README/CLAUDE 与代码索引。

### 6.3 文件规模与残留

- 本轮修改的源码/测试/文档文件均 ≤1000 行；`wc -l` 覆盖 `check-file-size.sh` 的 rg 缺失。
- 全量 `wc -l` 仍列出两个既有超限 vendored 文件：`frontend/components/assistant-ui/elements/tool-fallback.aui.tsx`（1431）、`thread.aui.tsx`（1166）。本轮未改动；按 `STD-SIZE-001` 另立任务治理，不宣称全库通过。
- 生产源码无 `TODO`/`FIXME`，无旧符号兼容 shim；旧符号只保留在负向测试与明确标注的历史文档中。
- `npm-packages/peri-cf/docker-compose.yml` 为未跟踪文件，本阶段未触碰。

## 7. 已知限制

1. `tests/` 不在任一 tsconfig 的 `include` 中，`bun run typecheck` 不检查测试类型。
2. 真实 WASM ACP、进程重启续聊、hosted Workers 与浏览器/真实模型验收未完成；A18/A22 标记 BLOCKED。
3. CF 的单测与 typecheck 依赖预构建 SDK dist；本阶段没有干净重建证据。
4. 本阶段是清理与文档执行，不替代独立验收；A01-A22 的 PASS 仅覆盖所列自动化证据。
5. `sessionClaimKey` 保留既有协调域前缀 `peri:managed-agent:session:`。它是存储 key，不是公开符号；改名会破坏升级期跨版本 claim 互斥，需单独迁移，不在本轮修改。
