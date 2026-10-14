# S7a TUI 侧独立复核报告（verification）

- 日期：2026-10-14
- 复核对象：worktree `peri-transcript-history`（分支 `feat/transcript-shrink-history-pagination`，未提交工作区）
- 复核范围：只审 `peri-tui/`；ACP/Actor/存储/SDK 侧形状仅作为**消费方契约**核对
- 需求文档：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`（§5.1、§5.2、§7 REQ-C01~C03、§9 A11/A12/A20/A23/A24）
- 侦察底稿：`.recon/E-tui-sdk.md`（仅作线索）

> **实现者报告缺失**：`.recon/S7a-tui.md` 不存在（`.recon/` 下无此文件，`grep -rl "S7a"` 只命中 workflow 脚本与 spec issue）。本报告完全从 git diff 与实测出发，未参考任何实现者自述。

## 0. 结论摘要

| 条目 | 判定 | 依据 |
| --- | --- | --- |
| REQ-C02（Compact 不再强制完整 reload） | **PASS**（TUI 侧） | 实测（隔离运行既有用例 + 临时用例走真实 `CompactCompleted(manual)` 路径）+ 静态调用者清点 |
| REQ-C01-1（滚动触发 + 稀疏首屏边界） | **FAIL**（触发接线不生效） | 静态（高置信）：触发只挂在 `use_effect` 的 7 个 deps 上，滚动本身不改任一 deps |
| REQ-C01-2（并发合并 / 重试去重） | **PASS** | 实测 3 个用例 |
| REQ-C01-3（区间合并 / 头插条件 / 锚点） | **PARTIAL** | 合并与头插实测 PASS；锚点用「总行数增量」近似，机制与需求字面不同 |
| REQ-C01-4（翻页 loading 与执行状态分离 / 失败可重试） | **PASS（机制）/ 重试受 D1 限制** | 分离与失败保留实测 PASS；「可重试」缺触发源（同 D1） |
| REQ-C01-5（切会话 / rewind 迟到页丢弃） | **PARTIAL** | 切会话实测 PASS；rewind 依赖服务端错误码，客户端本地不失效浏览状态 |
| REQ-C01-6（不触碰 Todo/plan/审批/输入队列/当前 turn） | **PASS** | 实测隔离用例 + 静态：历史页不走实时 handler |
| REQ-C01-7（历史投影模式） | **PASS** | 静态 |
| REQ-C03（不扩大前端架构改造） | **PASS** | 静态：无 Yjs、无持久化缓存、无虚拟化改造 |
| 既有行为回归 | **PASS（本切片 lib 全量绿）** | 实测 1798 passed / 0 failed / 6 ignored；无既有断言放宽，1 处测试能力收缩 |

**必须处理的一条**：REQ-C01-1 的「向上滚动触发 cursor load」在静止会话里不可达（见 §5 D1）。其余为残余风险与文档债务。

## 1. 范围与基线

- 工作区 git 状态：20 个 `peri-tui` 已跟踪文件被修改，7 个新文件未跟踪；全部改动均未提交（`git log -1` = `5be107603`）。
- 本报告不含 git 写操作；仅读 `git status/diff`。
- 未做基线（`main`）对照测试运行：仓库纪律禁止 `checkout`，因此「回归」只报告本切片全量绿，不宣称与基线逐一对照。

## 2. 改动文件清单（peri-tui 侧，`git diff --stat`）

新增（未跟踪）：

1. `src/kit/history_paging.rs`（559 行）— 浏览状态、页响应解码、区间索引与合并、触发判定。
2. `src/kit/history_page_consumer.rs`（243 行）— `HISTORY_PAGES_TX` → `session/load`（cursor）→ `LOCAL_EVENT_TX`。
3. `src/acp_client/client/history_page.rs`（35 行）— 历史页 RPC（`sessionId`/`cursor`/`limit`）。
4. `src/kit/acp_events/history_page.rs`（253 行）— 页 → 展示单元 → 并入 `committed`（不写执行状态）。
5. `src/kit/history_paging_test.rs`、`src/kit/history_page_consumer_test.rs`、`src/kit/acp_events_test/history_page_test.rs` — 共 23 个新用例。

修改：

| 文件 | 改动要点 |
| --- | --- |
| `src/kit/acp_events/turn.rs` | 删 `THREAD_LOAD_TX` 依赖与 compact reload；抽出 `committed_assistant_unit`（无状态构造函数） |
| `src/kit/acp_events/tool.rs` | 抽出 `replay_tool_started_unit` / `completed_tool_card`（无状态构造函数，历史页共用） |
| `src/kit/acp_events/mod.rs` | 新增 `HistoryPageLoaded` / `ReplayEntryMarked` 分派；记录条目区间槽位；`SessionReplayDone` 关闭区间 |
| `src/kit/acp_events/system.rs` | **未改**（`handle_command_feedback` 的 PENDING_COMPACT_NOTE 去重保持） |
| `src/kit/acp_events/compact.rs` | **未改**（`inject_system_note` 直注当前视图） |
| `src/kit/acp_notifier.rs` | 新增 `decode_history_sub_item`；回放通知里提取条目戳并先注入桥接 |
| `src/kit/acp_notifier/session_update.rs` | 新增 `decode_entry_marks`；`decode_stream_update_with(force_replay)` |
| `src/kit/atoms.rs` | `HISTORY_PAGES_TX` / `HISTORY_PAGING` / `HISTORY_PREPEND_EPOCH` |
| `src/kit/entry.rs` | 装配 `history_page_consumer` |
| `src/kit/session_boundary.rs` | 会话边界清空浏览状态与区间索引 |
| `src/kit/message_area/{mod,scroll,scroll/auto_follow}.rs` | 触发与锚点补偿接入 `run_auto_follow` |
| `src/acp_client/client/session.rs` | load 成功后写入浏览身份（`snapshotToken`/`nextCursor`） |
| `src/kit/thread_load_consumer.rs` | 删除 `#[cfg(test)] ThreadLoadRequest::thread_id()` |
| 测试文件 | `acp_events_test.rs` 注册新模块；`session_events_test.rs` 反转 compact reload 断言；`acp_bridge_test.rs` / `acp_bridge_cancel_test.rs` 适配 fixture 签名（含 `EntryMarks` 参数） |
| `src/kit/mod.rs` | 注册两个新模块 |

## 3. 改动前 vs 改动后（行为）

| # | 场景 | 改动前 | 改动后 | 对应 |
| --- | --- | --- | --- | --- |
| B1 | 手动 `/compact` 完成 → `TurnDone` | `turn.rs` 检测 `compact_just_completed` 后向 `THREAD_LOAD_TX` 发 `session_id`，触发完整 `session/load` 重放：`BRIDGE_RESET_COUNTER` reset → `committed` 清空 → 重放铺满 → 滚动定位底部 | 只清标志与 `PENDING_COMPACT_NOTE`，**不发任何 load**；`committed`、`HISTORY_PAGING`（会话/快照/游标）、区间索引原样保留 | REQ-C02 / A12 |
| B2 | 手动 compact 的完成提示 | 提示写 `PENDING_COMPACT_NOTE`，跨同会话 replay reset 在 `apply_bridge_reset` 里重建 | `CompactCompleted` 注入的 `TuiSystemNote` 由本次 `TurnDone` 的 `flush_current_turn` 并入 `committed`；`PENDING_COMPACT_NOTE` 在 `TurnDone` 清空 | REQ-C02 / A12 |
| B3 | 向上滚到已载入窗口顶端 | 无任何历史加载动作（窗口即全部） | 期望：派发 cursor `session/load` 并头部并入更早历史；**实测未生效**（D1） | REQ-C01-1 |
| B4 | 回放期间 | 每条回放 `session/update` 只产生一个展示事件 | 同一条通知若带 `_meta.periEntryId`/`periPosition`，额外先注入 `ReplayEntryMarked`（只登记区间，不产生展示单元） | REQ-H01-3/6 |
| B5 | 会话切换 / `/clear` / 重新 load | 清 `committed`、输入、弹窗等 | 追加清空分页浏览状态与区间索引 | REQ-C01-5 / A11 |

## 4. 需求与验收映射（逐条判定）

### REQ-C02（PASS，实测+静态）

1. **「手动 Compact 完成后必须再触发完整 `session/load`」依赖已删除** — `turn.rs:38-56` 只剩清标志与日志；`THREAD_LOAD_TX` 的生产调用者清点后只剩：`panels/thread_browser.rs:174,348`、`popups/confirm_popup.rs:150`、`entry.rs:358`（装配）、`launch.rs:84`（启动恢复入口）。**没有任何 compact 路径**。
2. **已显示历史 / 已加载窗口在 Compact 完成或失败后保留** — 实测（临时用例，见 §5 实测 3）：真实 `CompactCompleted(manual)` → `TurnDone` 后 `committed == [旧消息, SystemNote]`，长度 2 不变。
3. **cursor 进度保留** — 同一实测断言 `HISTORY_PAGING.{session_id, snapshot_token, next_cursor}` 与区间索引在 Compact 后不变。
4. **Compact 提示仍可见** — 路径证据：`compact.rs:81-84`（写 `PENDING_COMPACT_NOTE` 并 `inject_system_note`）→ `turn.rs:17`（`flush_current_turn`）→ `committed`；实测断言窗口内存在 `TuiSystemNote`。
5. **「窗口已由翻页扩大，新 Compact 不能整体替换成摘要，也不能把 cursor 跳回」** — 删除 reload 后架构上不可能替换；实测 H3 断言通过。
6. **「Compact 失败只更新失败展示，不清空消息区」** — 静态：TUI 内 `committed` 清空点只有两处（`acp_bridge.rs:405` 会话边界 reset、`acp_events/system.rs:497` rewind 完成），二者都不由 compact 触发。**未实测**。
7. **auto compact 语义不变** — `compact.rs:35-36` 仍只在 `trigger == "manual"` 置标志；`acp_events/mod.rs:331-350` 的流事件清标志逻辑未改。

> 判据强度说明：`test_compact_turndone_keeps_window_without_reload` 用 `rx_a.try_recv().is_err()` 断言「无 load 派发」。该断言依赖本用例的 `THREAD_LOAD_TX.set(...)` 成功；若同进程其它用例先占用 `OnceLock`，断言会退化为真空。已用 `--exact` 单测隔离运行消除该疑虑（§5 实测 2）。

### REQ-C01（核心机制 PASS，接线 FAIL）

1. **C01-1 触发边界**：**FAIL（静态）**。见 D1。稀疏首屏缺口与「顶端贴边」共用同一触发点（`auto_follow.rs:269-276`）设计上正确，但触发点本身只在 effect deps 变化时执行。
2. **C01-2 并发合并 / 重试去重**：**PASS（实测）**。触发侧「读—判—写在同一把锁内认领」（`history_paging.rs:461-508`）；消费侧二次校验（`history_page_consumer.rs:170-181`）；页合并按 `entryId` 去重（`history_paging.rs:190-192`、`258-293`）。用例：`test_request_older_page_if_due_merges_concurrent_triggers`、`test_is_duplicate_of_merges_only_same_cursor_and_limit`、`test_history_page_dedupes_repeated_page`、`test_merge_page_orders_by_position_and_dedupes`。
3. **C01-3 区间合并 / 头插条件 / 锚点**：**PARTIAL**。
   - 按 `position` 逐条插入，页内乱序也规范化（实测 `test_merge_page_inserts_into_middle_interval`）；
   - 「只有整页都早于当前最早条目才整体头插」被实现为更细粒度的逐条定位（严格强于需求）；
   - 继承前缀不越位：合并前先 `close_own_entry()` 收口首屏区间（`acp_events/history_page.rs:54-59`），实测两例（含「生产无 `SessionReplayDone`」形状）；
   - **锚点机制与需求字面不同**：需求要求「以稳定子项 ID + 子项内偏移保持阅读锚点」，实现是「插入导致的总行数增量整体下移 offset」（`history_paging.rs:300-311` + `auto_follow.rs:238-268`）。插入点位于视口下方（中间缺口填补）时会误补偿（D9）。
4. **C01-4 loading 与执行状态分离**：**PASS（机制）**。消费者不写 `ACP_STATE`/`LOADING_EPOCH`/submit/cancel；不建立/结束 turn（`history_page_consumer.rs:1-13` 与实测 `test_history_page_does_not_disturb_execution_state`）。失败只记 `last_error` 并保留 `next_cursor`（`history_paging.rs:535-538`）。**但重试同样只能靠 effect 重跑** → 受 D1 影响。
5. **C01-5 迟到页丢弃**：**PARTIAL**。
   - 切会话：`project_session_boundary` → `reset_browsing_state()`（`session_boundary.rs:15-22`）；消费侧 RPC 前后两次核对（`history_page_consumer.rs:182-201`）；桥接侧再核对（`acp_events/history_page.rs:89-96`）；两层独立。实测 `test_history_page_drops_page_from_stale_snapshot`、`test_settle_request_drops_response_for_stale_identity`、`test_handle_request_drops_stale_browsing_before_rpc`。
   - 同会话 rewind：客户端**没有**本地失效（`acp_events/system.rs:493-520` 只替换 `committed`），依赖服务端 `-32012` / `snapshotToken` 变化（见 D2）。
6. **C01-6 不触碰执行/审批/Todo/关键状态**：**PASS（实测+静态）**。`build_entries` 只构造 `TuiRenderUnit`：不复用 `handle_replay_tool_started`（其 `record_todo_started` 会写执行侧基线）、不复用 `handle_system_reminder`（会 flush `current_turn`）、不复用 `handle_local_user_bubble`（会改 `turn_generation`/`last_submitted_text`）。实测断言 `phase`/`turn_generation`/`current_turn.text`/`INPUT_BUFFER`/`HITL_PENDING`/`POPUP_KIND`/`todo_call_inputs`/`last_successful_todos` 全部不变，且历史内容仍然并入。
7. **C01-7 历史投影模式**：**PASS（静态）**。页子项强制走回放分支（`acp_notifier.rs:31-38` → `decode_stream_update_with(force_replay=true)`），摘要/assistant/tool 无需 Human 前导即可成单元；不伪造 user 输入或 active turn（`TuiUserBubble::new` 为纯构造）。
8. **A23 子项身份**：**PARTIAL**。`entryId` 用于条目去重、子项各自成单元（`messageId` 不参与折叠键），因此「共享 messageId 的子项不互相覆盖」成立；但 §5.2「传输适配层不得丢弃已有 messageId」未满足——非流式投影（首屏回放与历史页）都把 `message_id` 置 `None`（`turn.rs:455-461`）。**这是沿用既有 replay 行为，本切片未加重**。

### REQ-C03（PASS，静态）

1. 无 Yjs 相关改动（`grep -rn "yjs" peri-tui/src` 无命中）；未新增依赖（`peri-tui/Cargo.toml` 无 diff）。
2. 无新的前端持久化缓存：4 个新文件内无 `std::fs`/`tokio::fs`/文件写入；状态全为进程内（`HISTORY_PAGING` atom、`HISTORY_SPANS`/`OPEN_OWN_ENTRY`/`PENDING_TOOL_ENDS` `parking_lot::Mutex`）。
3. 无全历史虚拟化改造（`message_area` 渲染与 slot 缓存未改）。
4. 历史投影未走实时 handler：`acp_events/history_page.rs` 只 push/update `committed`，不碰 `current_turn`/`turn_generation`/`INPUT_BUFFER`/`STEERS`/审批弹窗/任务队列；reminder 走 `TuiSystemReminder::from_wire`（纯构造 + 受众过滤），不走 ownership-gated 的 `from_trusted_structured`。

### A11 / A12 / A20 / A23 / A24 对应

| 验收 | 判定 | 证据 |
| --- | --- | --- |
| A11 切会话/rewind 后旧页迟到 | PARTIAL | 切会话实测；rewind 依赖服务端（D2） |
| A12 手动/自动 Compact 无强制 reload，窗口/cursor/输出/未决交互保留 | PASS（TUI 侧实测） | 临时用例 + 隔离运行的既有用例；未决交互面（HITL/popup）由「无 load → 无 `apply_bridge_reset`」保证 |
| A20 稀疏首屏逐页合并 + 不越过继承前缀 + 锚点不跳 | PARTIAL | 合并顺序实测 PASS；锚点近似补偿（D9）；单页更早整段头插的端到端未验证 |
| A23 无 Human 前导、不伪造 active turn、子项不互相覆盖 | PARTIAL | 单元构造与隔离实测 PASS；`messageId` 未保留 |
| A24 翻旧 TodoWrite 不改当前 Todo/plan/审批/输入/实时 turn | PASS（实测） | `test_history_page_does_not_disturb_execution_state` |

## 5. 实测证据

命令均在 `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&` 下执行，Rust 侧只用 `./scripts/cargo-rmcp-patched.sh`。

1. **编译**：`./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui`
   → `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 2.74s`；0 warning。
2. **既有 compact reload 反转用例（隔离运行，消除 `OnceLock` 真空）**：
   `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- kit::acp_events::acp_events_test::session_events_test::test_compact_turndone_keeps_window_without_reload --exact`
   → `test result: ok. 1 passed; 0 failed; 0 ignored; 1803 filtered out`。
3. **临时复核用例（本次新增，跑完已删除并还原）**：走真实 `CompactCompleted(manual)` → `TurnDone`（原仓库用例用的是 `CommandFeedback` 代理，未覆盖真实路径）：
   `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- temp_verify`
   → `1 passed; 1804 filtered out`。断言全过：窗口 `len == 2`（旧消息 + `TuiSystemNote`）、旧消息仍在首位、`compact_just_completed == false`、`PENDING_COMPACT_NOTE == None`、`HISTORY_PAGING` 的 `session_id/snapshot_token/next_cursor` 与区间索引原样。
   用例文件 `peri-tui/src/kit/acp_events_test/zz_temp_verify_compact_test.rs` 与 `acp_events_test.rs` 注册行**已删除/还原**（`git diff` 哈希回到实现者版本 `96a22ea21`，不保留）。用例主体见附录 A。
4. **新增用例**：`-- history_page` → `16 passed; 0 failed`（含 8 个纯函数/状态用例 + 8 个消费侧用例）；`-- history` 过滤 → `66 passed; 0 failed`。
5. **全量 lib 回归**：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib`
   → `test result: ok. 1798 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 24.16s`；0 warning；**无新增 `#[ignore]`**（`git diff | grep ignore` 无命中，3 个新测试文件也无 `#[ignore]`）。
6. **wire 形状核对（消费方契约，只读 ACP 侧）**：
   - 回放条目戳 `_meta.periEntryId` / `_meta.periPosition`（`peri-acp/src/dispatch/session_replay.rs:163-186`；reminder 走 `data` 戳，`peri-acp/src/host/requests/session_restore.rs:833-843`）；
   - 页响应 `{sessionId, snapshotToken, nextCursor, items[{entryId, position, oversize, subItems[{position, update|systemReminder}]}]}`（`peri-acp/src/host/history_page.rs:219-233, 322-337`）；
   - 初始 load 顶层 `snapshotToken` + 版本化 `nextCursor` envelope（`peri-acp/src/host/history_page.rs:265-312`），与客户端 `decode_session_cursor`（`history_paging.rs:141-151`）读取位置一致；页响应回传的 token 与初始响应同源（`browsing_snapshot_token`），客户端 `fetch_page` 的 token 比对因此可成立；
   - 错误码 `-32011` / `-32012`（`peri-acp/src/dispatch/session_load.rs:136,142`）与客户端 `matches!(error.code, -32011 | -32012)` 一致。

## 6. 既有测试断言变更（逐条）

1. **`test_compact_turndone_reload` → `test_compact_turndone_keeps_window_without_reload`**（`acp_events_test/session_events_test.rs:766-940`）：断言由「`THREAD_LOAD_TX` 应收到 `test-session`」**反转**为「`rx_a.try_recv()` 必须为空」；并新增 4 条更强断言（旧消息仍在首位、窗口长度 2、Compact 提示仍在窗口、`PENDING_COMPACT_NOTE` 已清）。**性质：需求驱动的反转，不是放宽**；唯一瑕疵是「无请求」断言在 `OnceLock` 被占时可能真空（§4 REQ-C02 判据强度说明，已用隔离运行缓解）。
2. **`thread_load_consumer.rs`：删除 `#[cfg(test)] ThreadLoadRequest::thread_id()`**：原仅被上面那条测试使用（已确认无其它调用者，编译通过）。**性质：测试能力小幅收缩**，随断言反转必然是合理的；但若将来需要断言「派发到哪个会话」，需重新引入等价访问器。
3. **`acp_bridge_test.rs` / `acp_bridge_cancel_test.rs`**：仅 import 顺序与 fixture 签名适配（`ReplaySender` 新增 `_marks: &EntryMarks<'_>` 参数、`ReplayMarks::without_entry_marks`、`LoadGeneration::from_client`），**断言未改**（该 fixture 演标准客户端，不带戳，与 REQ §5.3 一致）。
4. **`acp_client/client/session.rs`**：`load_session` 的错误分支由 `if let Err` 改为 `match`（等价重构），并新增 `decode_session_cursor` + `note_initial_cursor`。行为变化：**解码失败不影响 load 成败**（只降级浏览能力），不影响既有断言。
5. **全仓 `git diff -- peri-tui` 中删除的 `assert` 行只有 1 处**（即第 1 条的旧断言），其余删除行均为代码搬移（`tool.rs`/`turn.rs` 的函数抽取、`session_update.rs` 的标志折叠）。→ **未发现被静默删除或放宽的其它断言**。

## 7. 缺陷与残余风险（不自行修复，仅给最小复现与影响面）

### D1（P1，静态判断·高置信）滚动触发接线不生效 → REQ-C01-1 FAIL

- 位置：`peri-tui/src/kit/message_area/mod.rs:415-423`（effect deps）与 `scroll/auto_follow.rs:269-276`（唯一触发点）。
- 事实链：
  1. 生产环境唯一调用 `request_older_page_if_due` 的地方是 `run_auto_follow`（`grep -rn request_older_page_if_due` 只有 `auto_follow.rs:275` 与测试）；
  2. `run_auto_follow` 只在 `message_area/mod.rs:388` 的 `hooks.use_effect(...)` 内被调用；
  3. vendored 框架实现（`~/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/ratatui-kit-0.10.3/src/hooks/use_effect.rs:66-73`）为 `if hook.deps.as_ref() != Some(&deps) { f(); }` —— **deps 相等就不执行**；
  4. deps 元组 = `(items_len, vm_generation, is_loading, total_visual_rows, vis_height, loading_epoch, bridge_reset_counter)`：**不含滚动偏移、不含 `follow_bottom`、不含 `HISTORY_PREPEND_EPOCH`**；滚动只写 `scroll_state`/`follow_bottom`（`auto_follow.rs:24-28`），不改任一 deps。
- 影响：空闲会话（无 turn 在跑、无内容变化）里用户滚到已载入窗口顶端**不会加载更早历史**；翻页失败后的「重试」同理无触发源。运行中的 turn（流式持续改 `vm_generation`/`items_len`）掩盖了这个缺陷，因此像是「能翻页」。
- 最小复现（建议实现方执行）：长历史、协商连接、空闲会话中把视口滚到顶；`RUST_LOG=peri_tui::kit::history_page_consumer=debug` 观察是否有 `session/load`（cursor）请求与 `history_page: older page merged`；预期无任何输出。
- 修复方向（供参考，未实施）：把触发从渲染 effect 挪到滚动事件处理（`register_scroll_events`）或用「视口贴边」作为 deps 输入（例如把 `scroll_y`/`follow_bottom` 提升为 effect deps，或滚动手势内按节流直接调用触发）。
- 未验证：未做端到端滚动复现（`e2e/` 与 `docs/verification/tui-integration-cases/` 内**无**翻页用例）。

### D2（P2，静态）同会话 rewind 后客户端不失效浏览状态

- `acp_events/system.rs:493-520` 的 `handle_rewind_completed` 只替换 `committed`，不调用 `history_paging::reset_browsing_state()`，也不 bump `BRIDGE_RESET_COUNTER`。
- 影响：rewind 后 `HISTORY_SPANS` 仍指向旧窗口槽位；正常路径由服务端 `-32012`/结构版本变化兜住（`fetch_page` 的 token 比对、`settle_request` 身份比对），但「rewind 时刻已在途、之后才返回的页」在客户端看会话与 token 都匹配 → 会按旧索引并入，插入位置可能错乱（A11/A19）。
- 影响面：需在页 RPC 窗口内完成一次 rewind，窗口窄；一旦命中表现为历史页插错区间。
- 最小复现（未执行）：并发触发一次翻页 RPC 后立刻 rewind，断言页不被并入（当前会并入）。

### D3（P3，静态）首屏自有 reminder 条目不登记区间
 
- 回放路径的 reminder 戳写在 `peri/unstable_event` 的 `data`（ACP 侧 `session_restore.rs:833-843`），而 TUI 的 `UnstableEvent` 解码（`acp_notifier.rs:139-150`）不提取 `periEntryId`/`periPosition`，只有 `session/update` 路径提取（`acp_notifier.rs:297-313`）。
- 影响：若首屏「自有条目只有 reminder」（极端形状），区间索引为空，`insert_slot_for` 兜底返回槽位 0（`history_paging.rs:207-211`），更早历史会被插到继承前缀之前，违反 REQ-H01-6 / A20。
- 未实测；正常形状（首屏含 assistant/user/摘要）不受影响。

### D4（P3，静态）未配对工具终点可能错配到已 flush 的实时卡片

- `acp_events/history_page.rs:195-227` 的 `apply_pending_tool_ends` 在**整个 `committed`** 里按 `tool_id` 找卡片；实时 turn 的卡片在 `TurnDone` 后也落进 `committed`。
- 影响：历史页「结果先到、起点未到」的暂存结果，可能被补到同 `tool_id` 的实时卡片上（A10 要求「不挂到实时 turn」）。需要同 id 重名 + 起点页始终未并进来才命中；上限 256 条后整表清空（不越界增长）。
- 未实测。

### D5（P3，静态）页 RPC 无超时/取消 → 在途认领可能永久占用

- `AcpTransport::send_request` 明确「Connected silence has no built-in timeout」（`peri-acp/src/transport/mod.rs:33-39`），`history_page_consumer` 又串行 `await`。
- 影响：服务端静默时 `HISTORY_PAGING.in_flight` 永久占用，该会话翻页彻底停滞（不崩、不越界，shutdown 会结束消费者任务）。

### D6（P3，机制差异）锚点用行数增量近似（REQ-C01-3 字面未满足）

- 见 §4 C01-3 与 `prepend_scroll_delta`（`history_paging.rs:300-311`）。插入点位于视口下方时（中间缺口先于上方历史被并进来）会多补偿 `插入高度`，画面下移——与 A20「阅读锚点不跳动」不符。实现未按「稳定子项 ID + 子项内偏移」定位。

### D7（P3，文档/维护上下文）注释与 code-index 未同步

1. `kit/acp_events/mod.rs:144`「`/compact` 命令刚刚完成，TurnDone 时需触发 session/load 重放」；
2. `kit/acp_events/mod.rs:335`「标志保持到 TurnDone 触发 session/load 重放」；
3. `kit/acp_bridge.rs:390`「A manual compact schedules a same-session replay」（英文，已不成立）；
4. `kit/thread_load_consumer.rs:37`「shared by browser, confirm popup, and compact replay」；
5. `kit/atoms.rs:640`「跨 session/load replay 存活的 compact 完成提示」（现在 `TurnDone` 即清空）；
6. `docs/code-index/peri-tui.md` **未更新**（`git diff` 无该文件）：第 81 行仍描述「manual 提示在统一 reset 边界跨同会话 replay 保留」，且全文 0 次提及 `history_paging` / `history_page_consumer`（违反仓内 DOC-UPDATE-001 的索引同步要求；按路由，TUI 侧改动应同步 `docs/code-index/peri-tui.md`）。

### D8（P3，测试强度）「无 reload」断言可能真空

- 见 §4 REQ-C02 判据强度说明。建议在用例内断言 `THREAD_LOAD_TX.set(...)` 返回 `Ok`（或用局部替身注入）后再断言 `try_recv` 为空。

## 8. 未完成与未验证项

1. **未实测**：D1 的端到端滚动复现（无 e2e / 集成用例覆盖翻页；`grep -rl "history_page|分页|翻页" e2e docs/verification` 无命中）。
2. **未实测**：Compact **失败**分支（静态判断不清屏；无 compact 失败用例）。
3. **未实测**：D2（rewind 与在途页交错）、D3（首屏 reminder-only）、D4（实时同 id 卡片）。
4. **未验证**：`HistorySpans::contains` 为线性扫描、每页对每个条目一次，长历史（数十页 × 50 条）下的合并成本未做基准；内存/性能验收（A20 不属于本切片性能项）未测。
5. **未验证**：`oversize` 标记客户端只解码不断言行为（设计如此），A07 属服务端验收。
6. **不在本报告范围**：SDK 侧（`npm-packages/@peri-sdk`，REQ-C02 的「SDK 分开 Compact 与 rewind」、REQ-H04 的 `loadGeneration` 暂存/丢弃）与 ACP/存储侧形状；本报告只核对了 TUI 消费所需的 wire 字段与错误码。
7. **未做基线对照**：禁止 `checkout`，故未在 `main@7f0de56` 上跑同一套 lib 测试做逐项对照。

## 9. 风险汇总（按处置优先级）

1. **阻断级**：D1 —— REQ-C01-1 未达成（滚动不触发加载/不触发重试）。建议在合并前修复或显式记录为「本切片已知缺口 + 后续任务号」。
2. **需评估**：D2（rewind 交错窗口）、D7（code-index 与注释过期，属交付纪律）。
3. **可记录**：D3/D4/D5/D6/D8。

## 附录 A：临时复核用例（已删除，留档可复现）

```rust
// 文件：peri-tui/src/kit/acp_events_test/zz_temp_verify_compact_test.rs（跑完已删）
use super::*;
use crate::kit::acp_types::AcpEventData;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};
use serial_test::serial;

#[test]
#[serial]
fn temp_verify_manual_compact_keeps_window_note_and_browsing() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s-verify".to_string();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s-verify".to_string();
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "已加载的旧消息".into(),
    )));
    crate::kit::history_paging::reset_browsing_state();
    crate::kit::history_paging::note_initial_cursor(
        "s-verify",
        Some("tok".to_string()),
        Some("cursor-1".to_string()),
    );

    dispatch_for_bridge(&mut state, &AcpEventData::CompactCompleted {
        summary: "compact summary".into(),
        messages_json: String::new(),
        trigger: "manual".into(),
        strategy: "full".into(),
        affected_count: 2,
        estimated_tokens_saved: 512,
        files: vec![],
        skills: vec![],
    });
    assert!(state.compact_just_completed);

    dispatch_for_bridge(&mut state, &AcpEventData::TurnDone);

    assert_eq!(state.committed.len(), 2);            // 旧消息 + Compact 提示
    assert!(matches!(&state.committed[0],
        TuiRenderUnit::TuiUserBubble(b) if b.text.contains("已加载的旧消息")));
    assert!(state.committed.iter().any(|u| matches!(u, TuiRenderUnit::TuiSystemNote(_))));
    assert!(!state.compact_just_completed);
    assert!(crate::kit::atoms::PENDING_COMPACT_NOTE.state().read().is_none());
    let paging = crate::kit::atoms::HISTORY_PAGING.state().read().clone();
    assert_eq!(paging.session_id, "s-verify");
    assert_eq!(paging.snapshot_token.as_deref(), Some("tok"));
    assert_eq!(paging.next_cursor.as_deref(), Some("cursor-1"));
    assert_eq!(crate::kit::history_paging::with_spans(|s| format!("{s:?}")),
        "HistorySpans { spans: {} }");
}
```
