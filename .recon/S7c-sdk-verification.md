# S7c — SDK 侧第二轮独立复核（上一轮 FAIL / 风险项的修复状态）

- 复核人：verification（独立于实现者自述，不采信 `.recon/S7c-sdk-fix.md` 的结论）
- Worktree（唯一工作目录）：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`，HEAD = `5be107603`，全部改动未提交
- 范围：只审 `npm-packages/@peri-sdk`（含 `README.md` 与 `docs/code-index/peri-ts-sdk.md`）。`peri-agent` / `peri-acp` / `peri-acp-types` 只作 **wire 形状的只读依据**，未改任何 Rust 文件
- 切片冻结情况：SDK 改动文件 mtime 集中在 03:54–03:57，复核期间（04:05–04:45）**无二次改动**；服务端被引用文件最新 mtime 为 `peri-acp/src/dispatch/session_replay.rs` 00:58，均早于本轮复核
- 证据口径：**[实测]**（命令 + 原文输出）或 **[静态]**（读代码/文档）。未跑的一律写「未验证」
- 复核产物（raw 日志）：`.recon/S7c-verify-probe.txt`、`.recon/S7c-verify-probe.test.ts`（探针源码，需复制回 `npm-packages/@peri-sdk/tests/` 才能复跑）、`.recon/S7c-verify-regression.txt`、`.recon/S7c-verify-build.log`。探针已从测试目录移出，`git status` 与复核前一致（复核结束时的 `bun test` 为 252 pass / 2 fail，无残留用例）

---

## 0. 判据表

| # | 判据 | 判定 | 依据类型 |
| --- | --- | --- | --- |
| 1 | S1（原 FAIL）：真实 Compact 失败路径的失败展示 + `compactionStatus` 离开 `running`；服务端确实发该形状 | **PASS**（判据满足）+ 1 处界定残留 | 实测（独立探针 + 切片用例）+ 静态（服务端只读） |
| 2 | S2：初始回放在 `session.stream()` 与 docs 两条面上的行为自洽；记录是否完整 | **PASS** | 实测（独立探针）+ 静态 |
| 3 | S3：`test.failing` 已转正、跨路径块身份统一 | **PASS** | 实测 |
| 4 | S4：`nextCursor===null`(A15) 与「未协商」的 reason 真可区分 | **PASS** | 实测 |
| 5 | S5：导出名与 README / code-index 一致 | **PASS** | 实测（构建产物 + 运行时导入）+ 静态 |
| 6 | A22 回归：代次暂存 / 一次安装 / 失败丢弃 / 重试无重复 / 订阅不漏消息 | **PASS** | 实测（13 条既有用例 + 2 条独立探针） |
| 7 | 回归：typecheck / build / bun test | typecheck、build **PASS**；bun test 252 pass / 2 fail（**环境**，非切片） | 实测 |
| 8 | 公开面：`src/sdk/index.ts` 导出、README、code-index 一致；无删除既有导出 | **PASS** | 实测 + 静态 |

**最需要知道的一件事**：上一轮的 P2 FAIL（Compact 失败卡在 `running`）已修复并被独立复现；但「失败展示」只覆盖 **`compact_started` 之后**的失败，服务端在发 `compact_started` **之前** EarlyReturn 的真实失败（含 `/compact` 在空历史上返回 "no history to compact"）在 SDK 侧**仍然没有任何展示**（§2.1，非本轮引入）。

---

## 1. 逐条判据

### 1.1 S1（原 FAIL）——**PASS**（判据满足）

1. **判据要求 1：构造 `command_feedback` 的 error/cancel，失败展示出现且 `compactionStatus` 离开 `running`** —— 实测通过，且我用自己的探针独立重写（不依赖实现者的断言）。
   1. 探针 `S1-a`：`compact_started` → view 显示 `running`；投递 `command_feedback{level:"error",message:"compact persistence context mismatch",channel:"uiOnly"}` → `SessionView.session.compactionStatus === "failed"`，`compaction` 为 `{status:"failed",message:…}`，已展示消息区不被清空。
   2. 探针 `S1-b`：`level:"warning"` → `cancelled`。
   3. 探针 `S1-d`：终态不被后续反馈改写（`compact_completed` 后再来 error 反馈，仍是 `completed`）。
   4. 探针 `S1-e`：自动 Compact 的失败经 `agent_execution_failed` 收敛为 `failed`，且 `activeTurnStatus === "error"`（补一条实时消息后测得）。
   5. 展示面可读：`src/view/session-view.ts:67,74,254,260` 把 `compactionStatus` / `compaction` 投影进 `SessionView`，探针经公开的 `readSessionView` 读到，不是只读 Y.Map。
2. **判据要求 2：服务端确实会发这个形状** —— [静态、只读] 端到端证据链完整。
   1. 失败/取消载体：`peri-agent/src/session/exec/compact_pipeline.rs:421-459` —— `Completed`→`FeedbackLevel::Info`(:428)、`Cancelled`→`Warning`(:441，message `"compact cancelled"`)、`EarlyReturn`→`Error`(:453)；channel 一律 `FeedbackChannel::UiOnly`。
   2. 发射：`peri-acp/src/dispatch/execute_command.rs:161` → `peri-agent/src/session/exec/executor_helpers/intercept.rs:79-101`（`CommandFeedback` 事件）→ `peri-acp/src/session/event_sink/legacy.rs:218-221`（映射成 `AcpEvent::CommandFeedback`）→ 同文件 `:233-242` 以 `peri/agent_event` + `event_json` 发送；门控在 `peri-acp/src/session/event_sink.rs:315`（`if caps.agent_event`，`:315-317` 调用 `push_legacy_event`），SDK 声明 `peri.agentEvent: true`（`src/agent/session.ts:163`）。
   3. wire 形状：`peri-acp/src/event/mod.rs:41-42`（`#[serde(tag="type", content="value", rename_all="snake_case")]`）+ `:234-238` → `{"type":"command_feedback","value":{level,message,channel}}`；`level/channel` 经 `to_serde_str` 出 camelCase（`peri-acp-types/src/command.rs:206-239` → `"error"`/`"warning"`/`"uiOnly"`）。
   4. 服务端自带 wire 断言（我未运行，仅只读）：`peri-acp/src/session/event_sink_test.rs:388`（`push_event_forwards_command_feedback_wire`）断言通知方法为 `peri/agent_event`（:419）且 `event_json` 恰等于该三元组形状（:426-437） —— 与 SDK 探针/切片用例构造的 envelope 一致。
   5. 自动 Compact 的对应事件来源：`peri-agent/src/agent/stages/compact.rs:169-176`（`CompactStarted`）与 `peri-acp-types/src/event_v2/executor_mapping.rs:203-228`（`CompactStarted`/`MessagesCompacted`→`CompactCompleted{trigger:Auto}`）；自动 Compact 失败为致命错误，经 `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:583` 发 `AgentExecutionFailed` → SDK 的 `agent_execution_failed` 收敛分支（`src/state/session-projection.ts:177-181`）。
3. **切片用例**（实测，全绿）：`bun test --cwd npm-packages/@peri-sdk tests/session-docs.test.ts tests/history-merge.test.ts tests/history-pagination.test.ts` → `57 pass / 0 fail / 229 expect() / Ran 57 tests across 3 files. [78.00ms]`，其中 `a manual compact failure converges the compact display (A12)`、`a cancelled manual compact converges the compact display (A12)`、`feedback from another command never rewrites the compact display (A12)` 用的是 `command_feedback`（**真实失败载体，不是 turn 失败**）。
4. **残留（界定，不计入本判据失败）**：见 §2.1 / §2.2。

### 1.2 S2 —— **PASS**（两条面自洽，记录完整）

1. **实测（探针 `S2-a`）**：初始 load 被闸门挡住时，文档为空（`entryTexts(agent.docs) === []`），此时 `session.stream()` 尚不可消费（`session.ts:431-435` 的 `activeTransport()` 在第一次 `next()` 时抛 `Session is not active`）；响应回来后文档**一次安装**为 `["older first","compacted summary"]`，随后挂上的 `stream()` 消费者从有界队列里按到达顺序收到同样两条首屏回放（`["older first","compacted summary"]`）。
2. **重复投递的语义自洽**：安装后同代次再来一条回放 → 文档不变（仍两条），`stream()` 照收（原始流是「连接收到什么就有什么」）。
3. **实测（探针 `S2-b`）**：失败代次（`session/load` reject）后文档为空，`stream()` 第一次 `next()` 拒绝 `Session is not active` —— 即**不存在「外部已读到半份首屏」的窗口**；`session.ts:224-225` 失败时 `notifications.return()` + 新建队列。
4. **切片用例**（实测）：`the staged first-screen replay still reaches the raw event stream (REQ-H04)` 通过（上一轮该面缺席；本次实现者 `.recon/S7c-raw-sdk-before.txt:44` 记录修复前该用例 5000ms 超时，属其自证材料，我以探针独立复现修复后行为）。
5. **记录完整性** —— 实现者选择「保留原始事件流」而非「改记行为变化」，记录到位：
   1. `README.md:65`：显式写明 “that staging constrains document installation alone, so the raw `stream()` still carries every first-screen notification”。
   2. `docs/code-index/peri-ts-sdk.md:23`：「按代次暂存只约束文档安装：`session.stream()` 仍逐条投递首屏回放，不因暂存缺条」。
   3. 代码注释：`src/agent/session.ts:134-135`。
   4. 「示例」面：`examples/demo/*` 未被本切片修改，且没有依赖「首屏不进 stream」的写法（`grep` 未见相应断言）；此项按静态确认，未跑 demo。
6. **口径说明（重要，避免误读）**：因为 `stream()` 要求 `state === "active"`，首屏回放只能由 **start 之后才挂载**的消费者从有界队列里读到；队列上限 1024 条 / 4 MiB，超限是**显式** `EventStreamOverflowError`（`src/transport/event-queue.ts:14-30`），不静默丢条。这是与 HEAD 一致的行为（HEAD 同样 push），不是本轮新增语义。

### 1.3 S3 —— **PASS**

1. **`test.failing` 已全部转正** [实测]：`grep -rn "test.failing" --include=*.ts <包内>` 只命中 `node_modules/bun-types/test.d.ts`（类型声明），源码与测试内**零**命中。
2. **跨路径块身份统一** [实测，探针 `S3-a`/`S3-b`]：页先落库→回放后到、回放先落库→页后到，两种方向都得到 `entries.length === 1`、`blocks.length === 1`，且 `blockId === "h:{messageId}:text:0"`（实现者切片用例 `the same sub item merged from a page and from a replay keeps one block`、`a page adopts the block a replay already created for the same sub item` 同结论）。
3. **不误并** [实测，探针 `S3-c`]：同一 `messageId` 下两条不同正文的子项仍各自成块（`["First","Second"]`，块身份 2 个）。
4. 实现路径与声明一致 [静态]：`src/state/history-merge.ts:225-270`（`textBlockId` + `claimTextBlock`，页路径按下标命中优先、再按正文认领无下标块；回放路径只认领带下标块）。

### 1.4 S4 —— **PASS**（三种原因真的可区分）

1. **实测（探针，全部经公开 `Session.loadEarlierHistory` 抛出）**：
   1. `S4-a`：响应带 `snapshotToken` 但 `nextCursor === null` → `HistoryPageUnavailableError.reason === "exhausted"`，message 含 `no earlier history`；
   2. `S4-b`：响应无 `snapshotToken` → `reason === "not_negotiated"`；
   3. `S4-c`：`rewind_completed` 之后 → `reason === "invalidated"`。
2. 实现 [静态]：`src/state/session-docs.ts:66-88`（`seedHistoryWindow` / `historyWindowOpen` / `historyWindowInvalidated`）+ `src/agent/session.ts:265-275` 的三段映射；类型 `HistoryPageUnavailableReason = "not_negotiated" | "exhausted" | "invalidated"`（`session.ts:14`）。
3. 切片用例同名三条（`...reports an exhausted window, not a missing capability (A15)`、`a load response without a browsing window reports a missing capability`、`a page that arrives after rewind is rejected...`）实测全绿。

### 1.5 S5 —— **PASS**

1. **导出** [实测]：`src/sdk/index.ts:16-18` 新增 `AWAITING_RESULT_STATUS`、`COMPACTION_STATUS`、类型 `CompactionStatus`（`HistoryPageUnavailableError` / `StaleHistoryPageError` / `HistoryPageOutcome` / `HistoryPageUnavailableReason` 为先前切片已加）。构建后 `dist/sdk/index.d.ts:7,8,16,17,18` 均在；运行时 `bun -e 'import("./dist/index.js")'` 实测：`AWAITING_RESULT_STATUS → "awaiting_result"`、`COMPACTION_STATUS → {running,completed,failed,cancelled}`、`HistoryPageUnavailableError → function`（`new` 出来 `reason === "exhausted"`，`name` 正确）。
2. **`EntryView.historyOversize`** [实测]：`src/view/session-view.ts:26` 声明、`:177` 写入（判定 `=== true` 才置位），`dist/view/session-view.d.ts:32` 有该可选字段；探针 `S5-b` 从 `readSessionView` 读到 `true`/`undefined` 两态。
3. **与文档一致** [静态]：`README.md:69`（三种 reason 文案、`h:{messageId}:{type}:{slot}`、`awaiting_result`、`EntryView.historyOversize`、Compact 终态）与 `docs/code-index/peri-ts-sdk.md:20,23,24,30` 逐项对得上；`/view` 子入口同样导出 `EntryView`。
4. **没有删除既有导出** [实测]：`git diff -- npm-packages/@peri-sdk | grep -E "^-.*\bexport\b"` 只有一行 `-export { Session } from "../agent/session";`，它是被下一行扩展式替换（`Session, HistoryPageUnavailableError, StaleHistoryPageError`），非净删除；`src/portable.ts` / `src/view/index.ts` / `src/wasm*.ts` 入口文件未被改动。

### 1.6 A22 回归 —— **PASS**（13 条既有用例，逐条实跑）

`bun test --cwd npm-packages/@peri-sdk tests/history-pagination.test.ts` → `13 pass / 0 fail / 57 expect() / Ran 13 tests across 1 file. [55.00ms]`（EXIT=0）。映射：

1. 代次暂存与一次安装：`replay is staged until the load response installs it in one pass (A22)`。
2. 订阅不漏消息 + 首屏进原始流：`the staged first-screen replay still reaches the raw event stream (REQ-H04)`；另有独立探针 `A22-b`（`session/load` 请求确实带 `loadGeneration`，响应前到达的回放不丢条）。
3. 失败丢弃：`a failed load leaves no staged replay behind`；被取代丢弃：`replay from a superseded generation never reaches the window`。
4. 重试无重复 + 失败代次迟到不污染：`a failed load discards its staged replay, and a retry installs exactly one copy (A22)`；独立探针 `A22-a` 以自写 transport 复现同一组断言并通过（恰好一份、新代次不同、迟到回放不改动文档）。
5. 其它相关：`declares peri.historyPagination and pages with only sessionId, cursor, and limit`、`a first screen with no earlier content reports an exhausted window...`、`concurrent calls with the same cursor and limit share one request`、`a failed page keeps the window and the same cursor can be retried`、`a page that arrives after rewind is rejected and the window is cleared`、`a page from a different snapshot is rejected whole (A11)`、`a page that arrives after close is dropped`。

补充：上一轮我的复核产物 `tests/zz-verification-repro.test.ts`（未被 git 跟踪）被实现者删除。我逐条核对了它的 5 条语义是否仍有覆盖：A22 重试（切片用例同名）、A12 Compact 后继续流式（`after a compact the live turn keeps streaming into the same entry (REQ-C02)`）、A15 无更早历史（`...exhausted window...`）、Compact 失败状态（3 条新用例）、跨路径重复块（2 条新用例）→ **无覆盖丢失**；我本轮又用独立探针重写了其中 8 条语义（§1 各处）。

### 1.7 回归（typecheck / build / bun test）

| 命令 | 结果 | 判定 |
| --- | --- | --- |
| `bun run --cwd npm-packages/@peri-sdk typecheck` | `TYPECHECK EXIT=0`（`tsc --noEmit && tsc --noEmit -p tsconfig.demo.json`，无输出） | PASS |
| `bun run --cwd npm-packages/@peri-sdk build` | `Finished \`release\` profile [optimized] target(s) in 5.34s` / `Built @peri-code/sdk with the workspace peri-wasm artifact` / `BUILD EXIT=0`（日志 `.recon/S7c-verify-build.log`；构建期 warning 全在 `langfuse-client`/`peri-resources`/`peri-middlewares`，非 SDK 切片） | PASS |
| `bun test --cwd npm-packages/@peri-sdk`（构建前 / 构建后 / 移除探针后各一次） | 252 pass / 2 fail / 2058 expect() / Ran 254 tests across 34 files | **环境失败**，非切片 |

1. **两条失败的归因（环境）**：均为 `tests/wasm-acp-integration.test.ts`，报错发生在夹具阶段 —— `error: sqld exited (1) at waitForSqld (tests/wasm-acp-integration.test.ts:23:44)`（单跑该文件 → `0 pass / 2 fail`）。
   1. 该文件未被本切片修改（`git status --porcelain -- <file>` 无输出）。
   2. 夹具用 `Bun.spawn(["mise","exec","--","sqld",...])`（同文件 `:49-53`），环境里 `sqld` 不可用：worktree 内 `mise exec -- sqld --version` 报 `Config files ... are not trusted`；主仓库报 `mise ERROR "sqld" couldn't exec process: No such file or directory`。
   3. 结论：失败在 SDK 逻辑之前，且与上一轮基线一致（上一轮同为 2 条）。
2. **实现者未跑的 `build` 我补跑了**，且补跑后重跑全量：仍 252 pass / 2 fail（同一 2 条环境失败）——排除了「`dist` 陈旧导致 public-view 类用例假绿」的可能（复核开始时 `dist` 是 03:38、源码是 03:54–03:57，确实落后于源码；现已用当前源码重建）。
3. **切片自跑一致性**：实现者报告的 `57 pass / 0 fail`、`252 pass / 2 fail`、`TYPECHECK EXIT=0` 三项均被独立复现。

### 1.8 公开面一致性（含「有没有顺手删除既有导出」）

1. `src/sdk/index.ts`（包 `exports` 的 `"."` 入口，运行期由 `scripts/build.ts` 打成 `dist/index.js`，类型由 `tsc` 出 `dist/sdk/index.d.ts`）只增不删，见 §1.5.4。
2. README：新增段落与代码逐项对得上（暂存语义、reason 三取值、块身份、`awaiting_result`、`historyOversize`、Compact 终态），未出现「分页保证累计内存恒定」这类未验证承诺（REQ-C03 要求的口径保留在 `README.md:95` 原文）。
3. `docs/code-index/peri-ts-sdk.md`：会话与输入 / 实时 Yjs 投影 / 前端会话读模型 / 验证 四行同步更新，导出名与取值与代码一致（无错名、无遗漏的 `exhausted`）。
4. 未发生：删除既有导出、改 `package.json`（`git status` 无此项）、改 `src/sync/*` 或 `src/doc-model.ts`（REQ-C03 相关面）。

---

## 2. 反例与最小复现（本轮实测得到，均**不是**判据失败，但需知悉）

### 2.1 【P3，非本轮引入】`compact_started` 之前的真实 Compact 失败在 SDK 侧没有任何展示

1. 事实链 [静态]：`compact_pipeline.rs` 在 `emit_compact_started`（`:241`）**之前**有 9 条 `EarlyReturn`（`:101,113,126,142,150,164,174,186,224`，含 `"no history to compact"`、`"compact persistence is unavailable"`、`"compact persistence context mismatch"`）。它们同样经 `execute_compact` 映射为 `CommandFeedback(Error)` 送达 SDK。
2. SDK 的收敛条件是「只有当 `compactionStatus === "running"` 才接受终态」（`src/state/session-projection.ts:223-231`），而 `running` 只由 `compact_started` 写入；因此这些反馈被整条丢弃，且 SDK 也没有通用的命令反馈出口。
3. 最小复现（我的探针 `S1-c`，实测）：新建 `SessionDocs`，直接投递 `command_feedback{level:"error",message:"no history to compact",channel:"uiOnly"}` → `readSessionView(...).session.compactionStatus === undefined`、`.compaction === undefined`。用户可见结果是「什么都没发生」。
4. 界定：相对 HEAD 与上一轮**都不是回归**（HEAD 同样不展示命令反馈），且判据 1 要求的「真实失败路径（`compact_started` 之后）」已满足；但如果把 REQ-C02「Compact 失败只更新失败展示」读成「任何失败都要有失败展示」，这一条不成立。

### 2.2 【P3】归因启发式：Compact 运行窗口内其它命令的 warning/error 会被记成 Compact 的终态

1. 根因 [静态]：`command_feedback` 的 wire 形状不带命令名（服务端冻结，未改），SDK 只能按「有 Compact 在进行」推断。
2. 最小复现（探针 `S1-f`，实测）：`compact_started` → 投递 `command_feedback{level:"warning",message:"rewind cancelled"}` → `compactionStatus === "cancelled"`。
3. 上界由 `feedback from another command never rewrites the compact display (A12)` 固定：终态不会被后续反馈翻回；影响面是「Compact 未结束时其它命令的反馈」这一窄窗口。
4. 文档面：README 未披露该推断前提（只写 "manual compact reports it as `command_feedback`"），代码注释 `session-projection.ts:182-191` 有写明。

### 2.3 【P3，信息】首屏回放现在占用 `session.stream()` 的有界队列预算

1. 事实 [静态]：`event-queue.ts:14-30`，预算 1024 条 / 4 MiB；超限置 `EventStreamOverflowError` 并结束流（显式，不静默丢条）。
2. 与 HEAD 行为一致（HEAD 也 push 全部通知），本轮的 S2 修复是**回到**该行为；对以 docs 为渲染源的消费者无影响；对「attach 后一次读完首屏」的消费者，超大首屏可能直接得到显式 overflow 错误。未构造实测（未验证项 §3.6）。

### 2.4 【P3，命名】旧用例名仍叫 "compact failure"，实际载体是 turn 失败

`tests/session-docs.test.ts:515` 的 `compact failure only updates the failure display` 用 `agent_execution_failed`（turn 失败），断言本身正确（新增了 `compactionStatus` 收敛断言），但名字容易与「Compact 失败」混淆；真正的 Compact 失败现在是另外 3 条（§1.1.3）。建议改名，属文档/维护上下文问题，不影响功能。

---

## 3. 未验证项（明确列出，不作为判据通过的依据）

1. **未做端到端（真实 `peri-acp` + Store + 模型）的 Compact 失败链路**：需要真实模型与存储，本轮以「服务端只读代码 + 服务端自带 wire 断言（`event_sink_test.rs`）+ SDK 侧真实 envelope 探针」替代。
2. **未跑 `bun run test:wasm`**：其首文件即因缺 `sqld` 夹具失败（§1.7.1）；`bun run smoke:workers` 未跑（环境无 `wrangler`，`command -v wrangler` 无输出）。
3. **未构造失效 cursor 的服务端错误码**（如 `-32012`）在 SDK 的最终呈现；SDK 只透传 `RpcError(code,message,data)`，本轮未见新增映射。
4. **未实测不同 `limit` 并发时的游标推进次序**（静态判断后到者写回自己的 `nextCursor`，靠 entryId 幂等吸收）。
5. **A25 服务端拒绝语义**（属服务端切片）与 REQ-C01-1 的滚动触发 / 锚点策略（属消费方，TUI 侧见 `.recon/S7-tui-verification.md` 的 D1/D6）不在本轮 SDK 范围。
6. **`historyOversize` 只有曝光没有端到端消费**（我验证了「写入 → view 可读」，未验证任何真实客户端据此按需取正文）。
7. **TUI 侧**：本轮未重跑 `cargo test -p peri-tui`（本轮 SDK 切片未改 Rust；并发切片仍在编辑 `peri-tui`，重跑不构成对本 SDK 切片的证据）。

---

## 4. 风险清单（本轮不修，供决策）

1. P3｜§2.1 失败展示的边界（pre-start EarlyReturn 静默）。
2. P3｜§2.2 归因启发式（窗口内其它命令反馈被记为 Compact 终态）。
3. P3｜§2.3 首屏回放消耗 `stream()` 预算 → 显式 `EventStreamOverflowError`。
4. P3｜§2.4 旧用例命名与语义不符。
5. P3｜README 未记录 §2.1/§2.2 两条边界（文档完整性）。
6. P3（既有，本轮未改）｜工具卡按原始 `toolCallId` 全局合并（`src/state/doc-model.ts` 未改），与 REQ-H03「稳定调用实例身份」的严格读法仍有差距——上一轮已记，本轮维持原判。
7. 范围提示（不变）｜分页不保证客户端累计内存恒定；SDK 与文档均无相反承诺，符合 REQ-C03 的表述要求。

---

## 5. 结论（明确回答 A12 的「完成／失败状态正确」）

1. **成立（主体）**：SDK 侧现在有真实失败载体（`command_feedback` 的 error/warning）与轮次失败（`agent_execution_failed`）两条收敛路径，`compactionStatus` 一定离开 `running` 并给出 `failed`/`cancelled`，且终态不被后续信号改写；完成态仍是 `completed`。这一面由我自己的探针（`S1-a/b/d/e`）与切片 3 条用例独立复现，服务端形状只读核对通过。
2. **成立范围需界定**：仅覆盖「`compact_started` 已发出之后」的失败/取消；服务端在 `emit_compact_started` 之前的 EarlyReturn（含 `/compact` 空历史）在 SDK 侧无任何展示（§2.1）。若验收把 A12 读成「所有 Compact 失败都要有失败展示」，则应记为**部分成立**并追加一条约定（例如：SDK 增加通用命令反馈出口，或服务端在早退路径也补一个可归因的信号——两者都超出本轮「不改服务端 wire」的约束）。
3. **上一轮的 P2 行为改变（初始回放不进 `stream()`）已消除**：`stream()` 恢复逐条投递首屏回放，与 docs 的一次安装投影两条面自洽、无丢条、无半份首屏外泄（§1.2）。
4. 没有发现本轮引入的回归：typecheck / build 通过，`bun test` 252 pass / 2 fail（2 条为缺 `sqld` 的既有环境失败，已给单跑与对照证据），切片三文件 57 pass / 0 fail，A22 相关 13 条全绿。

---

## 6. 复核产物与清理

1. `.recon/S7c-verify-probe.txt`（18 pass / 0 fail / 46 expect() / EXIT=0）、`.recon/S7c-verify-probe.test.ts`（探针源码；复制到 `npm-packages/@peri-sdk/tests/` 即可复跑）。
2. `.recon/S7c-verify-regression.txt`（typecheck + 切片用例 + A22 用例原文）、`.recon/S7c-verify-build.log`（build 全量日志，EXIT=0）。
3. 探针文件已从 `npm-packages/@peri-sdk/tests/` 移出；工作区 git 状态与复核开始时一致（未提交、无新增跟踪文件），未做任何 git 写操作与 DDL。
