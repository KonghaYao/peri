# S7c SDK 修复报告（实现者）

范围：`npm-packages/@peri-sdk`。依据复核报告 `.recon/S7-sdk-verification.md`、`.recon/S7b-sdk.md` 的 S1~S5。
约束：不改服务端 wire 形状；不新增依赖；工作区不提交；每条修复有先失败/先复现的测试支撑。

自跑结论（详见 §6）：SDK 目标三文件 57 pass / 0 fail；全量 252 pass / 2 fail（2 条为缺 `sqld` 的既有环境失败）；typecheck exit 0；TUI `-p peri-tui --lib` **1801 passed / 0 failed / 6 ignored**（首次尝试因并发切片正在编辑 `peri-tui/src/kit/message_area/scroll{,/event.rs}` 而编译失败，待其编辑稳定后重跑通过，证据见 §6.3）。

---

## 1. S1（P2）Compact 失败收敛

1. **修了什么**
   1. `src/state/session-projection.ts`：新增导出 `COMPACTION_STATUS`（`running`/`completed`/`failed`/`cancelled`）与 `CompactionStatus`；`compact_started`/`compact_completed` 改用常量。
   2. 新增 `command_feedback` 分支：`level==="error"` → `failed(message)`，`level==="warning"` → `cancelled(message)`，其余忽略。这正是手动 Compact 失败/取消的真实载体（pipeline `PipelineOutcome::EarlyReturn`/`Cancelled` → `AcpEvent::CommandFeedback{level,message,channel}`）。
   3. `agent_execution_failed` 先 `settleCompaction(failed)` 再 `turns.exit("error")`：轮次失败时 Compact 不可能再上报终态。
   4. 新增 `settleCompaction`：只在 `compactionStatus === "running"` 时收敛，终态不被后续信号改写。
2. **证据**
   1. 修复前（verification 实测，`.recon/S7-sdk-verification.md:78`）：`compact_started` 后投递 `command_feedback(level=error)`，`compactionStatus` 仍 `"running"`、`compaction` 仍 `undefined`；同报告 17 行判定 A12 **FAIL**。
   2. 修复后：`.recon/S7c-raw-zz-before-delete.txt`（删除 zz 探针前最后一次运行）原文 — 记录旧行为的断言 `expect(info.get("compactionStatus")).toBe("running")` 得到
      `Expected: "running" / Received: "failed"`，即行为已从「卡在 running」变为「收敛为 failed」。
   3. 新用例（`tests/session-docs.test.ts`）：`a manual compact failure converges the compact display (A12)`（真实 `command_feedback{level:"error",channel:"uiOnly"}`）、`a cancelled manual compact converges the compact display (A12)`（warning → `cancelled`）、`feedback from another command never rewrites the compact display (A12)`、`after a compact the live turn keeps streaming into the same entry (REQ-C02)`。
   4. 命令：`bun test tests/session-docs.test.ts tests/history-merge.test.ts tests/history-pagination.test.ts` → `57 pass / 0 fail / 229 expect()`。
3. **未达成与原因**
   1. `command_feedback` 的 wire 形状**不带命令名**（服务端冻结，不得改），无法从事件本身证明它属于 Compact，故采用「仅当 `compactionStatus==="running"` 时收敛」的启发式。
   2. 服务端没有 `compact_failed` 事件（`CompactEnded` 在 executor_mapping 映射为 `None`，不到客户端），失败只能经 `command_feedback` 或轮次失败推断。
   3. 残余缺口：`compact_started` 之后若既无 `compact_completed`、也无 `agent_execution_failed`、也无反馈（例如连接中断），展示仍停在 `running`；本轮未新增超时兜底（无权威信号，避免猜造终态）。
4. **风险**
   1. P3：Compact 运行期间，其它命令的 warning/error 反馈会被记为 Compact 的 `cancelled`/`failed`（窗口期窄，且 `settleCompaction` 保证终态不被后续信号改写，上界由 `feedback from another command never rewrites...` 用例固定）。
   2. 取消映射为 `cancelled` 而非 `failed`：与 pipeline 的 Warning 语义一致，但消费方若只认 `failed` 需同步取值表（README 已记录 4 个取值）。

## 2. S2（P2）初始回放是否仍进 `session.stream()`

1. **修了什么**
   1. `src/agent/session.ts` staged 分支重排：本代次回放先 `acceptDeliveryEvent(event)` + `notifications.push(event)`，然后才 `if (replayInstalled) return; stagedReplay.push(event)`。
   2. 即：**按代次暂存只约束文档安装**，原始事件流「连接收到什么就有什么」，首屏回放不再缺席。选择「保留原始事件流」而非「记录行为变化」。
2. **证据**
   1. 修复前：`.recon/S7c-raw-sdk-before.txt` 原文 — `(fail) history load staging and pagination > the staged first-screen replay still reaches the raw event stream (REQ-H04) [5000.00ms]` + `^ this test timed out after 5000ms.`（`stream()` 里没有任何首屏事件）。
   2. 修复后：同用例通过并断言首条即 `session/update`、文本 `compacted summary`、`_meta.periLoadGeneration` 等于本次 load 代次；三文件 `57 pass / 0 fail`。
   3. 判断依据：`.recon/S7-sdk-verification.md:22,134`（staged 分支在 `notifications.push` 之前 return，README `stream()`/examples 会缺首屏）。
3. **未达成与原因**：无。
4. **风险**
   1. 响应安装完成后再到的同代次回放通知仍整条丢弃（防重复块）。这是既有契约行为，由 `replay is staged until the load response installs it in one pass (A22)` 固定；服务端契约里响应即回放结束，不会再有新内容回放。

## 3. S3（P3）页/回放块身份统一

1. **修了什么**
   1. `src/state/history-merge.ts`：文本块身份统一为 `h:{messageId}:{type}:{slot}`（原：页 `h:{position}`、回放 `h:{messageId}:{type}:{n}`）。
   2. 新增 `claimTextBlock`：页路径（有子项下标）先按下标命中（重复页幂等），再按正文认领回放先建的无下标块并补写 `historySubPosition`；回放路径（无下标）只按正文认领页先建的**带下标**块。回放自身不按正文合并，避免把两条相同文本的不同子项并成一条。
   3. `tests/history-merge.test.ts`：`test.failing` 探针改写为两条正常断言用例（页→回放同一子项一个块、回放→页认领同一块）。
2. **证据**
   1. 修复前：`.recon/S7-sdk-verification.md:97`（探测输出 `"Answer|Answer"`，同一条目两个文本块）。
   2. 修复后：`.recon/S7c-raw-zz-before-delete.txt` 原文 — `probe: the same entryId reaching the doc from a page and from a replay keeps one block` 报 `^ this test is marked as failing but it passed`，即 `test.failing` 标记已不再成立（行为已修复）。
   3. 三文件 `57 pass / 0 fail`，含新断言 `blocks` 长度 1 与 `view.entries[0].blocks` 单块。
3. **未达成与原因**：无（当前 SDK 生命周期下该路径不可达，属防御性修复，与复核报告 97 行判断一致）。
4. **风险**
   1. **行为变化**：`EntryBlockView.blockId` 语义从「页位置」变为「`h:{messageId}:{type}:{slot}`」。仓库内无其它消费方，README 未承诺该字段稳定性；已列入 §5 公开面变化。
   2. 页路径按正文认领时，若同一 `(messageId, 类型)` 有多条相同正文且都无下标，认领第一条（即回放先到的那条）；正文相同即渲染相同，不影响展示。

## 4. S4（P3）区分 `nextCursor===null`(A15) 与未协商

1. **修了什么**
   1. `src/state/session-docs.ts`：新增 `historyWindowOpen(): boolean`（窗口存在）。
   2. `src/agent/session.ts`：无游标时映射三种原因 — `invalidated`（rewind）/ `exhausted`（窗口存在但 `nextCursor===null`，文案 `This browsing window has no earlier history left`）/ `not_negotiated`（本连接无窗口，原文案）。`HistoryPageUnavailableReason` 增 `"exhausted"`。
   3. 测试：新增两条（exhausted、not_negotiated 各自 `reason` + `message`）。
2. **证据**
   1. 修复前：`.recon/S7c-raw-sdk-before.txt` 原文 — 两处 `Received  [HistoryPageUnavailableError: This connection has no history pagination window]`，即 A15「确实没有更早内容」被报成 `not_negotiated`。
   2. 修复后：三文件 `57 pass / 0 fail`，其中 `...reports an exhausted window, not a missing capability (A15)` 断言 `{reason:"exhausted", message: contains("no earlier history")}`；`...reports a missing capability` 断言 `{reason:"not_negotiated"}`；`invalidated` 沿用原 reason 与文案（`tests/history-pagination.test.ts:362`）。
3. **未达成与原因**：无。
4. **风险**：`reason` 取值集合扩大（`not_negotiated`/`invalidated` → `+exhausted`），对穷举 switch 的消费方是破坏性增量；这是本轮刻意的语义修正，已列入 §5。

## 5. S5（P3）导出与出口

1. **修了什么**
   1. `src/sdk/index.ts`：导出 `AWAITING_RESULT_STATUS`、`COMPACTION_STATUS`、类型 `CompactionStatus`（同文件已有先前切片的 `HistoryPageUnavailableError`/`StaleHistoryPageError`/`HistoryPageOutcome`/`HistoryPageUnavailableReason`）。
   2. `src/view/session-view.ts`：`EntryView` 增可选字段 `historyOversize?: boolean`；`entryView` 读 `entry.get("historyOversize") === true`。语义：该条目**自身**超过页字节目标（服务端 `oversize`），消费方可据此按需处理大条目。
   3. 文档：`npm-packages/@peri-sdk/README.md`、`docs/code-index/peri-ts-sdk.md` 记录新导出、`reason` 三取值、`historyOversize`、Compact 终态、S2 的「暂存只约束文档安装」。
2. **证据**
   1. 修复前：`.recon/S7c-raw-sdk-before.txt` 原文 — `SyntaxError: Export named 'AWAITING_RESULT_STATUS' not found in module '.../src/sdk/index.ts'`、`SyntaxError: Export named 'COMPACTION_STATUS' not found in module '.../src/state/session-projection.ts'`（两个测试文件整体未能加载，即修复前无法消费）。
   2. 修复后：`bun run --cwd npm-packages/@peri-sdk typecheck` → `TYPECHECK EXIT=0`；`bun test tests/history-merge.test.ts` 含 `oversize 标记可从 view 读`、`awaiting_result 常量可消费` 用例并通过。
3. **未达成与原因**：未新增「按需取正文」API——服务端页已内联正文，`historyOversize` 只是给消费方的提示位，不需要新接口。
4. **风险**
   1. `historyOversize` 为可选字段，旧消费方不受影响；但 `historyOversize` 只标条目级，不指示哪一段超预算。
   2. `AWAITING_RESULT_STATUS` 仅表示「历史投影下工具结果未加载」，不等于「正在执行」；README 已写明。

---

## 6. 改动文件清单与自跑结果

### 6.1 改动文件（绝对路径，均位于 worktree `/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/`）

1. `npm-packages/@peri-sdk/src/state/session-projection.ts` — S1（本切片新增，先前切片未改）
2. `npm-packages/@peri-sdk/src/state/history-merge.ts` — S3（该文件同时含先前切片的分页合并实现）
3. `npm-packages/@peri-sdk/src/agent/session.ts` — S2（staged 分支重排）、S4（reason 映射）
4. `npm-packages/@peri-sdk/src/state/session-docs.ts` — S4（`historyWindowOpen`）
5. `npm-packages/@peri-sdk/src/view/session-view.ts` — S5（`EntryView.historyOversize`）
6. `npm-packages/@peri-sdk/src/sdk/index.ts` — S5（新导出）
7. `npm-packages/@peri-sdk/tests/session-docs.test.ts` — S1、S5 用例
8. `npm-packages/@peri-sdk/tests/history-merge.test.ts` — S3、S5 用例
9. `npm-packages/@peri-sdk/tests/history-pagination.test.ts` — S2、S4 用例
10. `npm-packages/@peri-sdk/README.md` — 文档
11. `docs/code-index/peri-ts-sdk.md` — 文档
12. 删除：`npm-packages/@peri-sdk/tests/zz-verification-repro.test.ts`（verification 产物，`.recon/S7-sdk-verification.md:115,148-150` 声明其独立于切片并建议复核后删除或并入）

### 6.2 SDK 自跑（命令与原文结果）

1. 目标三文件：
   `cd npm-packages/@peri-sdk && bun test tests/session-docs.test.ts tests/history-merge.test.ts tests/history-pagination.test.ts`
   → `57 pass / 0 fail / 229 expect() calls / Ran 57 tests across 3 files. [103.00ms]`
   修复前同一命令（`.recon/S7c-raw-sdk-before.txt`）：`10 pass / 5 fail / 2 errors / 52 expect() calls / Ran 15 tests across 3 files. [5.20s]`
2. 全量：`bun test` → `252 pass / 2 fail / 2058 expect() calls / Ran 254 tests across 34 files. [1401.00ms]`
   2 条失败均为 `tests/wasm-acp-integration.test.ts` 的 `error: sqld exited (1)`（本机缺 `sqld`，切片前即存在，非本轮引入）。
3. 类型检查：`bun run --cwd npm-packages/@peri-sdk typecheck` → `TYPECHECK EXIT=0`。
4. 未执行：`bun run --cwd npm-packages/@peri-sdk build`（Emscripten release 构建，非本轮所需；`dist/` 已被 `.gitignore:204` 忽略，`tests/public-view.test.ts` 从 `dist` 导入但未涉及新导出）。

### 6.3 TUI 自跑（第二次通过，首次为并发编辑阻塞）

1. 命令：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib`。
2. **第一次（03:58 发起）**：退出码 **101**，原文 `error: could not compile \`peri-tui\` (lib test) due to 7 previous errors`，全部位于
   `peri-tui/src/kit/message_area/scroll.rs`（`error[E0364]: \`project_viewport_after_pending\` is private, and cannot be re-exported`）与
   `peri-tui/src/kit/message_area/scroll/event.rs`（`error[E0061]: this function takes 4 arguments but 3 arguments were supplied`，缺 `&ReactiveHandle<ScrollThrottle, SingleWaker>`）。
   日志：`.recon/S7c-raw-tui-first-attempt-stderr.log`（stdout 为空）。
   归因证据：这两个文件当时正被**并发切片**编辑（`event.rs` mtime `03:58`、`scroll.rs` mtime `03:57`、`throttle.rs` mtime `03:59`；`git status` 显示 `peri-tui/src/kit/**` 大量未提交改动与新增 `history_page_*.rs`）。本切片未修改任何 Rust 文件（改动清单见 §6.1）。
3. **第二次（等并发编辑稳定后重跑）**：`test result: ok. 1801 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 23.67s`，包装脚本输出 `[04:02:53] TUI TEST EXIT=0`。
   stdout 日志：`.recon/S7c-raw-tui-pass-tail.txt`（stderr：`Finished \`test\` profile [unoptimized + debuginfo] target(s) in 2.73s`）。
4. 结论：TUI 侧无回归；首次失败是工作区里**并发切片**的半成品编译状态，不是 SDK 修复或本切片引入。

---

## 7. 公开面变化

1. 新增导出（`src/sdk/index.ts`）：`AWAITING_RESULT_STATUS`（`"awaiting_result"`）、`COMPACTION_STATUS`（`running`/`completed`/`failed`/`cancelled`）、类型 `CompactionStatus`。
2. 取值扩展：`HistoryPageUnavailableError.reason` 新增 `"exhausted"`（类型 `HistoryPageUnavailableReason` 同步），`compactionStatus` 新增 `failed`/`cancelled` 两个终态。
3. 结构扩展：`EntryView` 新增可选字段 `historyOversize?: boolean`。
4. 行为变化（对外可见的内部身份）：历史文本块 `EntryBlockView.blockId` 由页路径的 `h:{position}`（回放为 `h:{messageId}:{type}:{n}`）统一为 `h:{messageId}:{type}:{slot}`；同一条目经页与回放两条路径投递不再产生重复块。
5. 行为还原：`session.stream()` 恢复投递首屏回放（相对先前切片是修复；相对更早基线是还原）。
6. README 章节：`npm-packages/@peri-sdk/README.md` 的 docs/投影段（还原暂存语义）与「Paging earlier history」段（reason 三取值、块身份、`awaiting_result`、`oversize`/`historyOversize`、Compact 终态）；`docs/code-index/peri-ts-sdk.md` 表格「会话与输入」「实时 Yjs 投影」「前端会话读模型」「验证」四行。

## 8. 既有断言/文件变更说明（逐条理由）

1. `tests/history-pagination.test.ts:185` **新增** `rejects.toMatchObject({ reason: "exhausted" })`（184 行原 `toBeInstanceOf` 断言保留，无删除、无放宽）。理由：原断言只校验错误类型，无法区分 A15 与未协商，正是本切片 S4 的目标。
2. `tests/session-docs.test.ts` 既有用例 `compact failure only updates the failure display` **追加**两条断言（`compactionStatus`/`compaction` 收敛为 failed），原有断言（消息区、工具状态、`activeTurnStatus`）全部保留。理由：该用例原本把 `agent_execution_failed` 当 Compact 失败且不断言收敛，补齐 S1 目标；语义未变。
3. `npm-packages/@peri-sdk/tests/zz-verification-repro.test.ts` 删除（未跟踪文件）。理由：verification 产物，`.recon/S7-sdk-verification.md:115,148-150` 已声明其不属于切片并建议复核后删除或并入；删除前复跑留证 `.recon/S7c-raw-zz-before-delete.txt`（`3 pass / 2 fail`，两条失败分别是「旧行为断言被修复打破」与「`test.failing` 已通过」）。
