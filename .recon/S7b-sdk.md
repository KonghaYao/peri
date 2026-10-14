# S7b — SDK 前端切片（TypeScript / Yjs）

Worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支：`feat/transcript-shrink-history-pagination`（未提交；服务端切片未改一行）
包：`npm-packages/@peri-sdk`

**证据口径**：本报告把「实测」（命令 + 输出片段）与「静态判断」（读代码）分开标注。凡未跑过的验证一律写「未验证」。

---

## 1. 范围

1. 只改 SDK（TypeScript）+ 对应代码索引/README；Rust 服务端与 TUI 一行未动。
2. 覆盖 P1（能力声明）、P2（Compact 不整表替换）、P3（初始 load 代次暂存）、P4（分页合并）、P5（测试）。
3. 硬约束 REQ-C03：复用现有 `ChatProjection` / `DocModel` / `view` 投影；不引入 Yjs 分文档或轮换、不做全历史虚拟化、不新建前端持久化缓存、不新增依赖。

---

## 2. 起点状态（必须先说明的事实）

接手本任务时该 worktree 的 SDK 目录**已经有大量未提交改动**（P1、P2、P4 与大部分 P3 的实现及其测试已存在），疑似本切片的前一次运行所留。因此：

1. 本报告区分「接手时已存在的实现」与「本次会话补做/修正的部分」。
2. 已存在的实现我一律以**读代码 + 跑测试**核对，不采信侦察底稿。
3. 本次会话的实际产出见 §3 的「本会话」列。

---

## 3. 改动文件清单

| 文件 | 接手时 | 本会话 |
| --- | --- | --- |
| `src/agent/session.ts` | 已改（能力声明、代次暂存、`loadEarlierHistory`） | 修正失效原因判定（`historyWindowInvalidated`） |
| `src/agent/agent.ts` | 已改（`beginLoadGeneration` / `isCurrentLoadGeneration`） | 无 |
| `src/state/session-docs.ts` | 已改（回放批次安装、窗口身份、页合并、Compact/Rewind 分流） | 新增 `historyInvalidated` 状态 + `invalidateHistoryWindow()`；删除已无调用者的 `historySnapshotToken()`；删除 `clearHistoryWindow()` |
| `src/state/history-merge.ts`（新增） | 已有（未跟踪） | 无 |
| `src/state/chat-projection.ts` | 已改（`replaceHistory(raw, mode)`） | 无 |
| `src/sdk/index.ts` | 已改（导出分页类型与错误） | 无 |
| `tests/history-merge.test.ts`（新增） | 已有 | 无 |
| `tests/history-pagination.test.ts`（新增） | 已有；其中 1 条用例**失败** | 由源码修正转绿 |
| `tests/session-docs.test.ts` | 已改 | 给 Compact 用例补 cursor 进度断言（P5 缺口） |
| `tests/tool-payloads.test.ts` | 已改（canonical 用例改走 rewind） | 无 |
| `tests/tool-projection.test.ts` | **未改，1 条用例因 REQ-C02 失效** | 改为 `rewind_completed` |
| `test/domain.test.ts` | **未改，2 条用例因新增 `loadGeneration` 失配** | 断言补 `loadGeneration: expect.any(String)` |
| `test/session-start-cleanup.test.ts` | **未改，2 条用例同上失配** | 同上 |
| `docs/code-index/peri-ts-sdk.md` | 未改 | 更新路由（分页、代次暂存、Compact/Rewind、新测试入口） |
| `README.md` | 未改 | 补分页/Compact 语义；修正 payload 释放表述 |

---

## 4. 改动前 vs 改动后

### P1 能力声明（`src/agent/session.ts`）

1. 改动前：`capabilities` 里没有 `peri.historyPagination`。
2. 改动后：`"peri.historyPagination": true` 与其它 `peri.*` 能力并列，带注释说明「少声明一项 = 放弃该通道」。
3. 实测：`tests/history-pagination.test.ts` 直接断言 `initialize.clientCapabilities._meta["peri.historyPagination"] === true`（见 §6）。

### P2 Compact 不再整表替换（REQ-C02 / A12）

1. 改动前：`compact_completed` → `chatProjection.replaceHistory(value.messages_json, false)`（`replayEpoch++` + `resetChat()` + 重建）+ `interaction.abortAll()`。
2. 改动后：`compact_completed` 只走 `sessionProjection.acceptAgentEvent`（写 `compaction` / `compactionStatus`），不碰消息区、不调 `replaceHistory`、不 `abortAll`。
3. rewind 与 Compact 分开：`rewind_completed` 仍重建窗口（服务端真的删了尾部行），但用新 `"preserve-execution"` 模式——重建前快照并恢复 `activeTurnId` / `activeTurnStatus` / `activeAssistantEntryId` / `activeRequestId` / `pendingRequestId`，未决交互本就存在 Session Doc 的 `info` 中、`resetChat` 不清理它。同时 `historyMerge.reset()` + 作废游标。
4. 摘要语义：`messages_json` 不再进入 Chat Doc（旧窗口原样保留），摘要只以既有 `compaction` 元数据展示，不伪装成用户新输入。
5. 签名变化（静态）：`replaceHistory(raw, keepTurnOpen: boolean)` → `replaceHistory(raw, mode: "reset-execution" | "preserve-execution")`。HEAD 上唯一调用者传的是 `false`，即旧的 `keepTurnOpen=true` 分支在 HEAD 已是死代码；`ChatProjection` 未从 `sdk/index.ts` 或 `portable.ts` 导出，故非对外破坏性变更。

### P3 初始 load 代次暂存与丢弃（A22 / REQ-H04）

1. 改动前：订阅后每条通知立即 `docs.accept(event)`，响应到达才 `completeTurn()`；回放中途可见半份首屏，失败靠 `discardFailedSessionDocs()` 整体丢弃。
2. 改动后（顺序写清）：
   1. 先 `transport.subscribe(...)`（在发请求之前，所以回放一条不漏）；
   2. 再 `agent.beginLoadGeneration(crypto.randomUUID())`；
   3. 通知按 `_meta.periLoadGeneration` 识别：非本代次 → 丢弃；本代次且响应未回 → 只 push 到 `stagedReplay`；
   4. `session/load` 响应到达 → 校验 `_meta.periLoadGeneration` 回带与 `isCurrentLoadGeneration` → `docs.acceptHistoryBatch(stagedReplay)`（整批一个 Yjs 事务）→ 置 `replayInstalled`（此后同代次回放视为重复投递丢弃）→ `seedConfig` / `seedHistoryWindow`；
   5. 失败/被取代 → 暂存数组随栈丢弃，`discardFailedSessionDocs()` 换新 `SessionDocs`（`historyMerge` 与窗口身份一并归零）。
3. 无 `periLoadGeneration` 的旧式回放仍走原实时投影 + `completeTurn()` 收口（`replayTurnPending`）。
4. 我这次的修正：`loadEarlierHistory` 原来用 `historySnapshotToken()` 判定「窗口被作废」，但作废已经把它清成 `null`，导致 `reason: "invalidated"` 不可达（新用例实测失败）。改为独立 `historyInvalidated` 标记：rewind 设位、成功初始 load 清位。

### P4 分页合并（REQ-C01 / A11、A20、A23、A24）

1. 入口 `loadEarlierHistory(limit?)`：`session/load` 只带 `{sessionId, cursor, limit}`（不带 `cwd` / `mcpServers`）；相同 `cursor|limit` 并发合并为一次请求；失败保留窗口与游标可重试。
2. 合并身份：`entryId`（回放 `_meta.periEntryId` = 页 `entryId` = `{session}:{messageId}`）+ 子项位置（页 `subItems[].position`；回放用 `(messageId, 类型, 出现次序)` 派生）。**不按 messageId 去重**，所以一条 assistant 的多个 block 各占一个块身份。
3. 排序：条目级 `historyPosition` 三分区（继承前缀 → 自有历史按 position 升序 → 实时尾部），块级按 `historySubPosition` 插入；重复页/重叠页 `applyEntry` 幂等。
4. 历史投影模式：不建 active turn、不伪造用户输入、不写 plan/任务/输入队列/交互；工具无结果时状态为 `awaiting_result`（既不报失败也不显示运行中），终态不可回退。
5. 迟到/失效：`state !== active` 或 `sessionId` 变了 → `StaleHistoryPageError("session")`；响应 `snapshotToken` 与当前窗口不符（或窗口已作废）→ `StaleHistoryPageError("window")`，整页不并入。

---

## 5. 需求与验收映射

| 需求 | 验收 | 落点 |
| --- | --- | --- |
| REQ-C01-1/3/4 | A20、A23 | `history-merge.ts` 条目/子项位置合并；`loadEarlierHistory` 独立于执行状态 |
| REQ-C01-2 | A08（前端侧） | `pendingPages` 同 cursor+limit 合并 |
| REQ-C01-5 | A11 | 会话身份 + `snapshotToken` 双重校验，整页丢弃 |
| REQ-C01-6/7 | A23、A24 | 历史投影不推进 plan/任务/队列/turn；无 Human 前导 |
| REQ-C02 | A12 | Compact 不调 `replaceHistory`/`abortAll`；保留窗口、游标、执行与未决交互；rewind 分流 |
| REQ-H04 | A22 | 代次暂存、响应配对校验、一次安装、失败/取代丢弃 |
| REQ-C03 | — | 复用 `ChatProjection`/`DocModel`/`view`；无新依赖、无新持久化、无分文档 |
| §5.3 / A25（客户端侧） | A25 | 能力按当前连接声明；初始 load 不带 `cursor`/`limit`，历史页不带 setup 参数；不在客户端降级 |

---

## 6. 实测证据

### 6.1 typecheck

```
$ cd npm-packages/@peri-sdk && bun run typecheck
$ tsc --noEmit && tsc --noEmit -p tsconfig.demo.json
（无输出，退出码 0）
```

### 6.2 build

```
$ bun run build
...
    Finished `release` profile [optimized] target(s) in 6.29s
Built @peri-code/sdk with the workspace peri-wasm artifact
BUILD EXIT=0
```

（构建会从当前 worktree 的 Rust 源码重编 peri-wasm，包含未提交的服务端切片。）

### 6.3 全量测试

```
$ bun test
 240 pass
 2 fail
 2011 expect() calls
Ran 242 tests across 34 files. [1.78s]
```

两条失败均为**环境缺失**，非本切片引入（详见 §7.1）：

1. `(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host`
2. `(fail) a second ManagedAgents owner cannot claim the same real Session and never starts its transport`

失败细节（`tests/wasm-acp-integration.test.ts`）：

```
error: sqld exited (1)
      at waitForSqld (tests/wasm-acp-integration.test.ts:23:44)
      at async fixture (tests/wasm-acp-integration.test.ts:74:15)
```

环境核对：`which sqld` → `bash: sqld: command not found`。

### 6.4 本切片测试（全绿）

```
$ bun test tests/history-merge.test.ts tests/history-pagination.test.ts
 17 pass / 0 fail / 66 expect() calls

$ bun test tests/session-docs.test.ts
 28 pass / 0 fail / 116 expect() calls
```

覆盖对照 P5 清单：

| P5 要求 | 用例 |
| --- | --- |
| 重叠页 / 重复页幂等 | `repeated and overlapping pages merge idempotently` |
| 中间填补 | `a page fills the gap between first-screen entries and stays in ascending order` |
| 整页更早头插 | `a whole older page is inserted before the current earliest entry` |
| 同 messageId 多 block 不互相覆盖 | `sub items of one entry keep their own identity when they share a messageId (A23)` |
| 稀疏首屏保持继承前缀 | `a sparse first screen keeps the inherited prefix in front of the older own page (A20)` |
| 无 Human 前导 / 不建 active turn | `assistant and tool entries render without a Human prefix or an active turn (A23)` |
| 结果晚到不伪报失败 | `a tool result that arrives in a later page completes the earlier call without faking failure (A10)` |
| 旧 TodoWrite 不覆盖当前基线 | `history pages never advance the live plan, tasks, queue, or current turn (A24)` |
| Compact 不整表替换 + 窗口/执行/未决交互保留 | `compact keeps the loaded window, live turn, and pending interactions`（本会话补 cursor 进度断言） |
| Compact 失败只更新失败展示 | `compact failure only updates the failure display` |
| 回放响应前一次安装、之后重复丢弃 | `replay is staged until the load response installs it in one pass (A22)` |
| 被取代代次不进窗口 | `replay from a superseded generation never reaches the window` |
| 失败 load 不留暂存 | `a failed load leaves no staged replay behind`；`failed session/load discards replay from the rejected attempt` |
| 同 cursor+limit 并发合并 | `concurrent calls with the same cursor and limit share one request` |
| 翻页失败可重试、窗口不变 | `a failed page keeps the window and the same cursor can be retried` |
| rewind 后旧页丢弃 + 明确错误 | `a page that arrives after rewind is rejected and the window is cleared` |
| 快照不符整页丢弃（A11） | `a page from a different snapshot is rejected whole (A11)` |
| 关闭后迟到页丢弃 | `a page that arrives after close is dropped` |
| 历史页只带 sessionId/cursor/limit | `declares peri.historyPagination and pages with only sessionId, cursor, and limit` |

### 6.5 其他命令

```
$ git diff --check
（无输出）
```

---

## 7. 未完成与未验证项

### 7.1 未通过项（环境，非本切片）

1. `tests/wasm-acp-integration.test.ts` 两条用例需要本机 `sqld`（真实 loopback libSQL Store），本机未安装 → 失败发生在夹具 `waitForSqld`，尚未执行任何 SDK 逻辑。
2. **未与 HEAD 版本做基线对照**（禁止 `checkout`/`stash`/`worktree` 写操作）。判定依据是：失败点位于外部二进制启动 + `tests/wasm-acp-integration.test.ts` 全程未被本切片修改。
3. `bun run smoke:workers`（需 wrangler + 本地 sqld）未跑 → **未验证**。

### 7.2 未实现（明确不在本切片）

1. 页内 `systemReminder` 子项（`{position, systemReminder}`）不被合并：`decodeHistoryPageItems` 只接受带 `update` 的子项。因此**compact 摘要不在 SDK 视图里显示**——但回放路径同样不显示（`peri/unstable_event` 仅路由到 `taskProjection.acceptBackground`，`system-reminder*` 事件名不在其白名单）。这是接手前就存在的缺口，本切片未扩大也未修复；A23 的「摘要可展示」在 SDK 侧靠 assistant 文本路径验证（`tests/history-merge.test.ts` 用 `agent_message_chunk` 承载摘要）。修它需要先有 reminder 的展示模型，超出 REQ-C03 的「不扩大架构改造」。
2. `historyGenerationOf` 只读 `params.update._meta` 与 `params._meta`；`peri/unstable_event` 的代次在 `params.data` → reminder 回放不参与暂存。影响面为零（该通道当前不产生视图状态），但严格说它不是本代次收口的一部分。
3. `compactionStatus` 在紧凑失败后停留在 `"running"`：线上没有 `compact_failed` 事件（已核对 `peri-acp-types/src/event.rs`），失败表现为 `agent_execution_failed` → 只更新执行状态。未改（避免把通用执行失败一律当成 Compact 失败）。

### 7.3 未实测（静态判断）

1. **wire 字段名一致性**：`periEntryId` / `periPosition` / `periLoadGeneration` / `snapshotToken` / `nextCursor` / `items[].subItems[].update` 全部由我读 `peri-acp/src/host/history_page.rs`、`dispatch/session_replay.rs`、`host/requests/session_restore.rs` 核对（写入点：`write_entry_marks`、`replay_meta`、`attach_initial_cursor`、初始 load 响应 `_meta["periLoadGeneration"]`）。**没有跑真实服务端联调**，SDK 侧测试全部使用假 transport。
2. **A11/A20/A22 的端到端语义**（真实 Host 发页、`-32011`/`-32012` 实际返回）在 ACP 侧切片验证，不由本报告背书。
3. A21（长 turn 执行中翻页、并发加载另一会话）未测：翻页不取执行锁属服务端行为；SDK 侧无全局锁，`loadEarlierHistory` 与实时投影只共享 Yjs 事务。
4. 同一 transport 上多会话交错、页在初始 load 进行中到达、同 cursor 不同 limit 并发等组合未测。

---

## 8. 风险

1. **能力声明是硬开关**：`peri.historyPagination: true` 现在总是声明，服务端会据此打位置戳并在响应里附 `snapshotToken`/`nextCursor`；`attach_initial_cursor` 取不到边界事实时会让 load **失败**（不降级）。老服务端 + 新 SDK 的表现为 startup 失败而非静默无分页——符合 REQ §5.3，但升级顺序需要留意。
2. **回放去重偏保守**：`replayInstalled` 之后同代次的任何带代次回放通知都整条丢弃。若未来服务端在响应后补发同代次增量（同一 payload 的后续块），会被丢。当前契约（回放只在响应前）下安全。
3. **历史条目与实时条目 id 空间不同**（`{session}:{messageId}` vs `turn:{messageId}:{role}`），跨路径不去重：若某页返回了已经实时展示过的同一条 payload，会出现两份。服务端契约声明页范围从首屏最旧自有条目之前开始，理论上不发生；**未实测**。
4. **新的工具状态取值 `awaiting_result`**：`view` 侧 `status: string` 直通，SDK 内部消费者不受影响；外部若按白名单渲染状态需要同步认识它（TUI 走 Rust 路径，不受影响）。
5. **`replaceHistory` 语义收敛**：`preserve-execution` 下不再有「未完成工具按 keepTurnOpen 推断为 running」的分支（HEAD 上该分支已死）。若将来有调用方需要「重建历史但保留 running 工具」，需要显式补回，而不是复用布尔参数。
6. **TUI 侧未动**：本次只交付 SDK；REQ-C02 的「删除手动 Compact 后强制完整 session/load」在 `peri-tui` 仍存在（侦察底稿 E 已指出 `handle_turn_done` 的 `THREAD_LOAD_TX` 路径），需要 TUI 切片处理，本报告不宣称已完成。
