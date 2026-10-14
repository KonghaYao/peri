# S7 — SDK 侧独立复核（REQ-C01/C02/C03、A11/A12/A20/A22/A23/A24）

复核人：verification（独立于实现者 .recon/S7b-sdk.md，不采信其结论）
Worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
范围：**只审 SDK**（`npm-packages/@peri-sdk`，TypeScript/Yjs）。Rust/TUI 仅作为 wire 契约与失败根因证据读取，不做评审。
基线：未提交改动（HEAD = `5be107603`）vs 工作区；需求：`~/Desktop/peri-transcript-history-requirements.md`（§5.1/§5.2/§7/ch9）。
证据口径：每条标注 **[实测]**（命令 + 输出）或 **[静态]**（读代码）。未跑的一律写「未验证」。

---

## 1. 结论速览

| 项 | 判定 |
| --- | --- |
| P1 能力声明（`peri.historyPagination`，无删除其它能力） | **PASS** [实测+静态] |
| REQ-C02 Compact 不整表替换 / 保留执行与未决交互 / 与 rewind 分流 | **PASS**（主体）[实测+静态] |
| REQ-C02 Compact **失败**展示（A12「完成／失败状态正确」） | **FAIL**（SDK 无失败展示路径，`compactionStatus` 停在 `running`）[实测+静态] |
| A22 代次暂存 / 一次安装 / 失败丢弃 / 重试无重复 / 订阅不漏消息 | **PASS** [实测] |
| REQ-C01 合并（entryId+子项位置去重、区间合并、头插、并发合并、迟到丢弃） | **PASS**（主体）+ 1 处防御性缺口 [实测+静态] |
| REQ-C03 未扩大前端架构（无分文档/轮换/全历史虚拟化/新持久化缓存/新依赖） | **PASS** [实测+静态] |
| 回归：typecheck / build / bun test | typecheck、build **PASS**；bun test 240 pass / 2 fail（**环境**，非本切片）[实测] |
| 行为改变（实现者报告未提）：初始回放通知不再进入 `session.stream()` | **风险 P2** [静态] |

---

## 2. 改动文件清单（SDK 面，相对 HEAD）

**修改 11 个**（`git diff --numstat -- npm-packages/@peri-sdk` [实测]）
1. `src/agent/session.ts` (+128/-7)、`src/agent/agent.ts` (+13/-0)、`src/state/session-docs.ts` (+101/-9)、`src/state/chat-projection.ts` (+39/-3)、`src/sdk/index.ts` (+2/-1)
2. 测试：`tests/session-docs.test.ts`、`tests/tool-payloads.test.ts`、`tests/tool-projection.test.ts`、`test/domain.test.ts`、`test/session-start-cleanup.test.ts`
3. 文档：`README.md`；`docs/code-index/peri-ts-sdk.md`（非 SDK 包内，但同切片）

**新增 3 个**（未跟踪 [实测]）：`src/state/history-merge.ts`（313 行）、`tests/history-merge.test.ts`、`tests/history-pagination.test.ts`
**复核产物**：`tests/zz-verification-repro.test.ts`（207 行，**不属于切片**，仅用于本次最小复现；可删）

未改动（REQ-C03 相关 [实测] `git status`）：`src/state/doc-model.ts`、`src/sync/*`、`src/view/*`、`package.json`、`tests/wasm-acp-integration.test.ts`。

---

## 3. 改动前 vs 改动后

### 3.1 能力声明（`src/agent/session.ts`）
- 前：`initialize` 的 `clientCapabilities._meta` 无分页项。
- 后：新增 `"peri.historyPagination": true`（**session.ts:167**），注释说明未声明即放弃该通道。
- [实测] `git diff -U0 -- src/agent/session.ts | grep -E '^[+-].*peri\.'` → 只有一行 `+ "peri.historyPagination": true,`；同一 capabilities 对象的其余 8 项（session.ts:156-163：`userInputQueue/agentEvent/sessionWorkspaceV1/agentEventDone/replay/tokenStats/planEntryActiveForm/unstableEvent`）原样保留。
- [静态] 服务端读取同名键：`peri-acp-types/src/peri_caps.rs:104` `meta_bool(meta, "peri.historyPagination")`。

### 3.2 Compact 处理（`src/state/session-docs.ts`）
- 前：`compact_completed` 与 `rewind_completed` 共用一支 → `chatProjection.replaceHistory(value.messages_json, false)` + `interaction.abortAll()` + `sessionProjection.acceptAgentEvent(...)`。即：整表替换消息区、清 `activeTurnId/activeTurnStatus/activeAssistantEntryId`、删 `activeRequestId/pendingRequestId`（`turn-machine.ts:71-78` 的 `resetAfterHistory(..., false)`）、未配对工具标 `cancelled`、批量中止未决交互，摘要进入 Chat Doc。
- 后：`compact_completed` 只走 `sessionProjection.acceptAgentEvent`（session-docs.ts:215-220）→ 仅写 `compaction` / `compactionStatus`；不触碰消息区、不 `abortAll`。
- 后：`rewind_completed`（session-docs.ts:221-229）→ `replaceHistory(..., "preserve-execution")` + `historyMerge.reset()` + `invalidateHistoryWindow()`；重建前快照并恢复 `activeTurnId/activeTurnStatus/activeAssistantEntryId/activeRequestId/pendingRequestId`（`chat-projection.ts:150-211`）。
- 签名变化 [静态]：`replaceHistory(raw, keepTurnOpen: boolean)` → `replaceHistory(raw, mode: "reset-execution" | "preserve-execution")`；`ChatProjection` 未从 `sdk/index.ts` 导出，非对外破坏性变更。

### 3.3 初始 load 代次（`src/agent/session.ts`、`agent.ts`）
- 前：订阅后每条通知立即 `docs.accept`，响应到达才 `completeTurn()`；失败靠 `discardFailedSessionDocs()` 整体丢弃。
- 后：订阅（session.ts:123）→ 生成代次（:118-120）→ 通知按 `_meta.periLoadGeneration` 分流（:130-139：非本代次丢弃、本代次且未响应只入 `stagedReplay`、已安装后同代次重复丢弃）→ 响应校验代次回带与被取代（:190-195）→ **一次安装** `acceptHistoryBatch`（:196）→ 才 `seedConfig`/`seedHistoryWindow`。
- 后：`session/load` 请求新增顶层参数 `loadGeneration`（:185-186）；服务端同名读取（`peri-acp/src/host/history_page.rs:158,249`，响应回带 `session_meta.periLoadGeneration`：`session_restore.rs:659`）[静态]。

### 3.4 历史合并（新文件 `src/state/history-merge.ts`）
- 前：无历史合并；回放走 `ChatProjection` 实时投影（建 active turn、写 plan/队列、`replayEpoch` 重写块）。
- 后：带 `_meta.periEntryId` 的 `session/update` 走 `HistoryMerge.applyEntry`（session-docs.ts:180-183）→ 条目身份 `{会话}:{payload}`、条目级 `historyPosition`（存储序键）、块级 `historySubPosition`（页 `subItems[].position`）；回放无子项下标时按 `(messageId, 类型, 出现次序)` 派生。工具状态新增 `awaiting_result`（"结果未加载"，非"运行中"也非"失败"）。

### 3.5 分页入口（`src/agent/session.ts:245-298`）
- 后新增 `canLoadEarlierHistory` / `loadEarlierHistory(limit?)` / `HistoryPageUnavailableError` / `StaleHistoryPageError`；页请求只带 `{sessionId, cursor, limit}`；同 `cursor|limit` 并发复用同一 promise。

---

## 4. 需求与验收映射（逐条判定）

### 4.1 REQ-C02 / A12 — **PASS（主体）+ FAIL（失败展示）**
1. **不再整表替换** [实测+静态]：`grep -rn replaceHistory src/` → 唯一调用者 `session-docs.ts:224`（rewind）；`grep resetChat` → 唯一调用者 `chat-projection.ts:167`（`replaceHistory` 内部）。Compact 路径两者都不到。
2. **保留 active request/turn、流式输出、未决交互** [实测]：切片用例 `compact keeps the loaded window, live turn, and pending interactions`（`tests/session-docs.test.ts:466-512`）断言窗口逐条保留、`compaction` 元数据前进、`activeTurnId/activeTurnStatus/activeRequestId/pendingRequestId/pendingInteractions` 不变、未完成工具仍 `running`、`historyCursor()` 仍是 `cursor-earlier`（游标不跳回）。我的补充用例 [实测] `A12: after an auto compact the live turn keeps streaming into the same entry`：Compact 后再来流式 chunk 仍落在同一 `activeAssistantEntryId`（文本 `partial answer` → `partial answer continued`），不新建 turn/条目。
3. **rewind 与 Compact 分流** [实测+静态]：rewind 重建窗口 + 作废游标（`tests/session-docs.test.ts:155-179`、`tests/history-pagination.test.ts:264-287` 断言 `reason: "invalidated"` 与 `StaleHistoryPageError("window")`）。执行身份/未决交互保留（用例断言 `pendingInteractions` 对象身份不变）。
   - 差异（非缺陷，需知悉）：rewind 会**丢弃当前已渲染的流式块**（窗口按 `messages_json` 重建，用例断言 `entries` 为 0），Compact 不丢。执行身份保留，但"当前输出"只在 Compact 语义下保留。
4. **Compact 失败只更新失败展示** — **FAIL** [实测+静态]：
   - [静态] 手动 Compact 失败的真实载体：`peri-agent/src/session/exec/compact_pipeline.rs:447-459` `PipelineOutcome::EarlyReturn → CommandFeedback{level: FeedbackLevel::Error}`（错误级在 :454）→ `peri-acp/src/dispatch/execute_command.rs:161` → `peri-acp/src/event/mapper.rs:269`（无 SessionUpdate）→ `peri/agent_event` 的 `AcpEvent::CommandFeedback`（`peri-acp/src/event/mod.rs:234`，`rename_all = "snake_case"` → `type: "command_feedback"`）。SDK 的 `acceptAgentEvent` 无该分支，`session-projection.ts` 无该分支 → 无任何展示。
   - [静态] `compactionStatus` 全仓写入点只有 `session-projection.ts:164`（`compact_started` → `"running"`）与 `:178`（`compact_completed` → `"completed"`）；**没有任何路径把失败收敛回非 running**。
   - [实测] 我的 repro：`compact_started` 后投递 `command_feedback(level=error)`，断言 `compactionStatus` 仍为 `"running"`、`compaction` 仍 `undefined` → 记录当前行为即缺陷。
   - 切片用例所谓的"失败"用的是 `agent_execution_failed`（turn 失败，`tests/session-docs.test.ts:514-529`），**不是 Compact 失败**，因此 A12 的"失败状态正确"未被覆盖。结论：不清空消息区 ✔；失败展示 ✘。

### 4.2 A22 / REQ-H04 — **PASS** [实测]
1. 订阅顺序：`transport.subscribe`（session.ts:123）在 `session/load`（:181）之前 → 回放不漏。
2. 暂存/一次安装/成功后发布窗口：`tests/history-pagination.test.ts:182-203`（响应前文档为空、响应后一次安装、之后同代次重复丢弃）。
3. 失败/被取代丢弃：`session.ts:215-222`（`discardFailedSessionDocs()` 换新 `SessionDocs`）；用例 `replay from a superseded generation never reaches the window`、`a failed load leaves no staged replay behind`。
4. **我的独立用例（实现者未覆盖的重试路径）** [实测]：`A22: a failed load discards its staged replay, and a retry installs exactly one copy` →
   - 首次 load 中途回放到达后失败 → 文档为空；
   - 同 `Session` 重试（失败后 `state` 回 `declared`，session.ts:230）→ 新代次（UUID 不同）→ 恰好一份 `"compacted summary"`，`historyCursor() === "cursor-1"`；
   - 注入**失败代次**的迟到回放 → 文档不被污染。
   即"失败代次无成功 cursor / 暂存丢弃 / 重试无重复 / 迟到不污染"四条均在 SDK 侧有可执行证据。

### 4.3 REQ-C01 — **PASS（主体）**
1. 身份与去重 [实测]：页 `entryId`+`subItems[].position`（`history-merge.ts:63-88,186-227`）；同 messageId 多 block 各自成块（`tests/history-merge.test.ts:131-150`）。**不按 messageId 去重**（grep 无按 messageId 的块去重路径）。
2. 区间合并/头插 [实测]：`orderEntry`（:157-175）三段（继承前缀→自有按 position 升序→实时尾部）；用例 `a sparse first screen keeps the inherited prefix in front of the older own page (A20)`、`a page fills the gap ...`、`a whole older page is inserted before the current earliest entry`。注意实现是**逐条目**按 position 就位，天然满足"只有整页更早才整体头插"（不会误置）。
3. 并发合并 [实测]：`session.ts:255-277` 同 `cursor|limit` 复用 promise；用例 `concurrent calls with the same cursor and limit share one request`（1 次请求，两个调用同一结果）。
4. 迟到/旧范围丢弃 [实测]：`session.ts:282-291`（session 身份）+ `session-docs.ts:99-102`（`snapshotToken`）；用例覆盖 rewind 后（`invalidated` + `window`）、异 token、close 后。
5. 历史投影不推进实时状态 [实测]：用例 `history pages never advance the live plan, tasks, queue, or current turn (A24)`、`assistant and tool entries render without a Human prefix or an active turn (A23)`；代码上 `HistoryMerge` 只写 chat entries/blocks/tools/payloads，不建 turn、不写 plan/队列/交互。
6. **缺口（防御性）** [实测，`test.failing` 记录]：同一条目先由**页**（`h:{position}` 块身份）落库、再由**回放**（无子项下标 → `h:{messageId}:{type}:0`）到达时，产生第二个文本块（探测输出 `"Answer|Answer"`）。当前 SDK 生命周期不可达（服务端契约：页永不返回首屏条目 `peri-acp/src/host/history_page.rs:70-78`；失败重试换 `SessionDocs`），故记为潜在风险而非现网缺陷。

### 4.4 REQ-C03 — **PASS** [实测+静态]
1. 无新依赖：`package.json` 未修改（`git status`）。
2. 无 Yjs 分文档/轮换：`doc-model.ts` 未修改（仍 2 个 `Y.Doc`，`schemaVersion` 2/1）；新增文件不创建 `Y.Doc`。
3. 无全历史虚拟化：`src/view/*`、`src/sync/*` 未改；分页只是把已加载页并进同一 Y.Doc。
4. 无新持久化缓存：`grep localStorage|indexedDB|persist` 无新增；新增状态仅内存 `Map<entryId, docEntryId>`（只覆盖已加载条目，`reset()`/`destroy()` 清理）。
5. 未宣称"分页保证前端累计内存恒定"：README/代码索引的新增文字无此承诺（原"Retaining or reading every payload still costs space proportional to the complete session"保留）。

---

## 5. 实测证据

1. `bun run typecheck` → **exit 0**（`tsc --noEmit && tsc --noEmit -p tsconfig.demo.json`，无输出）。
2. `bun run build` → 末尾 `Finished \`release\` profile [optimized] target(s) in 1.36s` / `Built @peri-code/sdk with the workspace peri-wasm artifact` / **BUILD EXIT=0**。
3. `bun test` → **240 pass / 2 fail / 2011 expect() / Ran 242 tests across 34 files [1.487s]**，与实现者报告一致。
4. 两条失败 = `tests/wasm-acp-integration.test.ts`，均为夹具阶段：`error: sqld exited (1) at waitForSqld (tests/wasm-acp-integration.test.ts:23:44)`。**基线证据**：夹具命令是 `Bun.spawn(["mise","exec","--","sqld",...])`（该文件 :49-53）；本 worktree `mise.toml` 只声明 `rust`，`mise exec -- sqld --version` 在 worktree 报 "Config files ... are not trusted"、在主仓库报 "couldn't exec process: No such file or directory" → **sqld 在两种环境下都不可用，失败发生在任何 SDK 逻辑之前**；且该测试文件未被本切片修改。判定：非本切片引入。
5. 切片用例：`bun test tests/history-merge.test.ts tests/history-pagination.test.ts tests/session-docs.test.ts` → **45 pass / 0 fail / 182 expect()**（与实现者 17+28 的口径一致）。
6. 我的独立用例 `bun test tests/zz-verification-repro.test.ts` → **5 pass / 0 fail**：A22 重试无重复（含失败代次迟到回放）、A12 Compact 后继续流式、A15 无更早历史、Compact 失败状态（记录缺陷）、跨路径重复块（`test.failing` = 期望失败已复现）。
7. 加我的用例后 `bun test` → 245 pass / 2 fail（同一 2 条环境失败）。

---

## 6. 未完成与未验证项

1. **未跑**：`bun run smoke:workers`、`bun run test:wasm`（需 wrangler + sqld）→ **未验证**。
2. **未验证**：失效 cursor 的服务端可区分错误（-32012 等）在 SDK 侧的最终呈现——SDK 只透传 `RpcError(code, message, data)`（`json-rpc-transport.ts:134`），未做二次映射 [静态]；我没有构造真实服务端错误码响应。
3. **未验证（SDK 范围外/消费方）**：REQ-C01-1 的"滚动靠近边界触发"、REQ-C01-3 的"阅读锚点不跳动"、A20 的锚点行为——SDK 只提供 `canLoadEarlierHistory` / `loadEarlierHistory` 与稳定身份（`historyPosition`/`historySubPosition`），触发与锚点策略属消费方。
4. **未验证**：A08 中"不同 limit 并发"时游标推进次序——[静态] 两个响应共用同一 `snapshotToken`，后到者会把自己的 `nextCursor` 写回窗口（可能需要多取一页，靠 entryId 幂等吸收）。未构造实测。
5. **未验证**：A25 中"服务端拒绝未协商/非法参数"（属服务端切片）；SDK 侧只做了"不主动降级"的静态确认。
6. **未实测**：`historyOversize` 字段无任何读取方（`grep historyOversize` 只有写入），`awaiting_result` 常量未从公开入口导出、README 未记录 → 客户端无法据常量判断状态（见风险 P3）。

---

## 7. 风险

1. **P2｜Compact 失败无收敛**：手动 Compact 失败（`command_feedback` error/cancel）后，SDK 界面既无失败展示，`compactionStatus` 又停在 `"running"`（A12 要求"完成／失败状态正确"）。切片自带用例把 `agent_execution_failed` 当 Compact 失败，掩盖了该缺口。
2. **P2｜初始回放不再进入 `session.stream()`**（实现者报告未提的行为改变）：staged 分支在 `this.notifications.push(event)`（`session.ts:152`）**之前** return，因此带 `periLoadGeneration` 的回放通知（含回放期 `peri/unstable_event`）不再出现在 `session.stream()`；改动前会。以 docs 为渲染源的消费者无影响，但 README:40 与 `examples/demo` 的原始事件流会缺少首屏。需明确是否有意（README 仅说"consuming stream() is not required"）。
3. **P3｜跨路径块身份不一致**：页（`h:{position}`）与回放（`h:{messageId}:{type}:{n}`）对同一条目给出不同块身份 → 同一条目被两条路径投递会重复渲染（已用 `test.failing` 复现）。当前不可达，属防御性缺口。
4. **P3｜`loadEarlierHistory` 错误语义不精确**：窗口存在但 `nextCursor === null`（A15"确实没有更早内容"）时报 `HistoryPageUnavailableError("not_negotiated")`，与"本连接未协商分页"共用一个 reason 与文案 [静态]。
5. **P3｜工具卡按原始 `toolCallId` 全局合并**：`DocModel.ensureTool` 以 raw id 为键（`doc-model.ts:114-124`），跨 turn 同 id 会并成一张卡；这是**既有**行为（实时路径同样如此），本切片未改变，但与 REQ-H03"使用稳定调用实例身份"的严格读法有差距。
6. **P3｜公开面缺少两个新语义的出口**：`historyOversize` 无读取方；`AWAITING_RESULT_STATUS`（`"awaiting_result"`）未从 `sdk/index.ts` 导出、README/code-index 未记录。
7. **范围提示**：分页不保证客户端累计内存恒定（用户持续翻页并保留页面时仍增长）——SDK 与文档均未做相反承诺，符合 REQ-C03 的表述要求。

---

## 8. 复核产物

`npm-packages/@peri-sdk/tests/zz-verification-repro.test.ts`（新增，**不属于 S7b 切片**，建议复核后删除或并入切片用例）：
- `A22: a failed load discards its staged replay, and a retry installs exactly one copy` [pass]
- `A12: after an auto compact the live turn keeps streaming into the same entry` [pass]
- `A12: a failed compact leaves compactionStatus stuck at running (current behaviour)` [pass，记录缺陷]
- `A15 probe: a first screen without earlier history reports reason not_negotiated` [pass]
- `probe: the same entryId reaching the doc from a page and from a replay keeps one block` [`test.failing` → 缺陷已复现]

本次复核未修改任何切片文件（`git status` 仅新增上述 1 个文件）；未做任何 git 写操作。
