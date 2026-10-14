# S9c 独立复核报告：回放标记的门改按「当前连接」协商值（§5.3 / A25）

- 复核者：独立 verification（不采信实现者报告；每条结论都从代码与实测重新得出）
- 日期：2026-10-14
- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（**未提交**，`HEAD = 5be107603`）
- 需求依据：`~/Desktop/peri-transcript-history-requirements.md` §5.3（「能力版本以**当前连接**的协商结果为准，不能继承会话内保存的旧客户端能力」）、A25（重连能力变化）
- 复核者改动（唯一）：在 `peri-acp/src/host/history_page_wire_test.rs` 末尾追加 3 个用例 + 5 个私有 helper（`:1052-1340`，段首标注「独立复核（S9c-verification…）」）；另做一次**临时**负面控制（改 `session_restore.rs` 一行，已按 SHA-256 逐字节还原，见 §7）。**未 commit、未动 git 状态、未改生产语义、未新增依赖。**
- 指纹核对：实现者报告的 8 个 SHA-256 中 **7 个逐字符一致**；第 8 个（`history_page_wire_test.rs`）在**剔除复核者追加段**后等于 `4b74f158…c519e18`（字节级重建验证，见 §2.3）。

## 0. 判定摘要

| # | 判据 | 判定 | 依据 |
|---|------|------|------|
| 1 | 回放标记的门确为当前连接；链路完整；三类连接语义正确 | **PASS** | 静态（链路）+ 实测（V1/V2/V3） |
| 2a | 未协商连接（真实 `initialize`，注册表已被写过）零私有字段 + 完整 canonical | **PASS** | 实测 |
| 2b | 同场景协商连接仍收到 `periLoadGeneration` 与分页字段（门没被改死） | **PASS** | 实测 |
| 2c | 未 `initialize` 的进程内路径保持历史全能力语义 | **PASS** | 实测 |
| 3 | 协商路径断言未被放宽/删除；分页准入（失败关闭）与 §5.1 模式分流未变 | **PASS** | 静态（diff/逐条）+ 实测（13/34/全量） |
| 4 | 「修一类」成立：全部生产发射点门控来源正确 | **PASS**（六字段范围内 0 漏点；另有 1 个**无门控**的第 7 个私有字段，见 §4.3） | 静态（穷举）+ 实测（负面控制） |
| 5 | 无 DDL、无新依赖 | **PASS** | 实测 |
| 6 | 全量 `-p peri-acp --lib` 1 次 | **PASS**（884 例：首跑 883 passed / 1 failed 为**既有 flake**，单跑绿、二次全量 884 passed） | 实测 |

**结论：S9c 的目标达成。** §5.3「不能继承会话内保存的旧客户端能力」对**六个点名的 Peri 私有回放 / 分页字段全部成立，无残留**（详见 §9）。

---

## 1. 判据 1：门的来源与完整链路（静态）

### 1.1 本连接 caps 从哪来

1. `initialize` 请求 → `session_lifecycle::handle_initialize`（`requests/session_lifecycle.rs:124-139`）→ `initialize_peri_caps(params)`（`:145-152`，**唯一解析点**）：读 `clientCapabilities._meta` 的 `peri.*` 键；**没有 `_meta` ⇒ `PeriCaps::default()`（全 false）**。
2. 同一份值两处消费：进程级暂存 `set_pending_caps`（供会话能力投影）+ **连接级** `connection.lock().await.commit_initialize(caps)`（`host/server_loop.rs:525-530` → `host/connection.rs:42-47`，只认**第一次**、`Open` 生命周期内）。
3. 请求级快照：`dispatch_request` 开头 `connection.lock().await.facts()`（`server_loop.rs:376`，短锁、不跨派发持锁）→ `ConnectionFacts { history_pagination: self.history_pagination(), caps: self.caps() }`（`connection.rs:145-150`）。
4. 两个字段**来源不同**：
   - `history_pagination()`（`connection.rs:72-78`）= `initialized && negotiated_caps.history_pagination` ⇒ **失败关闭**：未 `initialize` 一律 false；
   - `caps()`（`connection.rs:54-58`）= `negotiated_caps` 或 `PeriCaps::all_enabled()` 兜底 ⇒ 未 `initialize` 的进程内路径保持全能力。

### 1.2 如何流到 replay_meta 与响应戳

1. `session/load` 分派：`requests.rs:55-62`（`handle_request_for_connection`，收 `ConnectionFacts`）→ `:122-124` → `session_lifecycle::handle_load(..., connection)`。
2. `handle_load`（`requests/session_restore.rs:514-662`）：
   - 分流与位置戳读 `connection.history_pagination`（`:558`、`:580`）；
   - `let connection_caps = &connection.caps;`（`:537`）→ 两条回放路径**都**传它（`:586-592` 协商、`:596-603` 未协商）；
   - 回放通知：`dispatch::replay_meta(caps, marks)`（`dispatch/session_replay.rs:591-609`）——`caps.replay` 决定 `periReplay` / `periLoadGeneration`；`periEntryId` / `periPosition` 由 `marks.merge`（= 协商过分页）决定（`write_entry_marks`，`:168-182`）；
   - reminder 出口：`send_projected`（`:426-444`）把同一份 `caps` 交给 `TuiReplaySender::send_system_reminder`（`:794-846`），事件名 `system-reminder`/`-fallback`（`:814-828`）与代次戳（`:836-841`）同源；
   - 响应戳：`if connection_caps.replay { response["_meta"]["periLoadGeneration"] = … }`（`:658-660`）。
3. 命令表广播**刻意**仍用会话级：`session_caps = ensure_session_caps(req_session_id)`（`:541`）只喂 `send_available_commands_update`（`:637-643`）；`session/resume` 同（`:683-696`）。
4. 旁路（同源，非本切片）：历史页出口 `handle_page` 的 `caps` 取自 `connection.lock().await.caps()`（`server_loop.rs:386`），而进入该分支已先过 `connection_facts.history_pagination`（`:387`，失败关闭）——未协商连接**到不了**这里；`initialized` 一旦为真，`caps()` 就等于协商值（`commit_initialize` 不会二次改写），故此处不存在兜底泄漏。

### 1.3 三类连接各拿到什么

| 连接类 | `history_pagination()` | `caps()` | 本次 load 的结果 |
|---|---|---|---|
| 未 `initialize`（进程内 TUI/MPSC、`handle_request` 旧入口） | false | `all_enabled` 兜底 | 完整 canonical 流式回放；**无**分页字段；`periReplay`/`periLoadGeneration` **照旧** |
| stdio/外部，`initialize` 未声明 peri 扩展 | false | `PeriCaps::default()`（全 false） | 完整 canonical 流式回放；**零** Peri 私有字段 |
| stdio/外部，`initialize` 声明 `peri.replay`（±`peri.historyPagination`） | 声明了才 true | 协商值 | 有效集合回放 + 代次戳 + 条目戳（协商分页时）+ 首游标（协商分页时） |

补注（影响判读，非缺陷）：TUI 生产路径会在首个 `session/new` 前上送 `initialize`，基座是 `PeriCaps::all_enabled()`（`peri-tui/src/acp_client/client/requests.rs:63-91`），因此 TUI 属第三类；「未 `initialize` 的兜底」只在握手失败与单测路径生效。两条路都给出全能力语义，TUI 观感不变。

---

## 2. 判据 2：三类连接的 wire 字段实测

### 2.1 命令与结果（真实 `ServerLoop` + MPSC transport + 真实 SQLite 门面，夹具含 Full 归档原文）

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire
running 13 tests
  s9c_verification_an_initialize_without_peri_extensions_yields_no_private_fields ... ok   ← 2a（复核者新增）
  s9c_verification_a_never_initialized_in_process_connection_keeps_the_replay_marks ... ok ← 2c（复核者新增）
  s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field ... ok      ← 2b（复核者新增）
  s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection ... ok（实现者转正的回归）
  s9_recheck_a_negotiated_connection_still_receives_the_replay_marks ... ok
  s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields ... ok
  （其余 7 例含 A21/A25 与「回放失败不给成功 cursor」）... ok
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 871 filtered out; finished in 16.53s
```

### 2.2 复核者新增的 3 个用例（标注）

统一走**真实请求接线**：`client.send_request("initialize", …)` → `handle_initialize` → `commit_initialize` → `facts()` → `handle_load`；`spawn_loop(…, None)` 保证连接侧的协商值**只能**由那条 `initialize` 建立。通知读取用 `s9c_drain_notifications`（`biased` + `ready(())`）：服务端通知与响应走同一无界通道、由同一 pump 顺序转发，响应到达时「响应之前的全部通知」已入队，因此**不用任何等待时限**即可取全（也是它比既有 `select!` 版更不易 flake 的原因）。

1. `s9c_verification_an_initialize_without_peri_extensions_yields_no_private_fields`（`:1160`，判据 2a）
   - 断言：会话级注册表**预先写入** `{replay:true, historyPagination:true}`（模拟「这条会话此前被 peri 连接写过」）；连接 `initialize` **不带** peri 扩展；响应 + **全部**通知递归键集合里六个字段（`periReplay`/`periLoadGeneration`/`nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`）**一个都不出现**；回放正文序列**逐条等于** `load_session_history` 的 canonical（含被 Full 归档的原文）。
   - 为什么不重复：既有 `s9_recheck_*` 把 caps 直接 `commit_initialize` 进 `ConnectionContext`，**绕过 `initialize` 请求**；本用例覆盖的是「请求 → 解析 → 落连接」这段接线（若 `initialize_peri_caps` 读错位置或 `commit_initialize` 未被调用，只有本用例会红）。
2. `s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field`（`:1218`，判据 2b）
   - 断言：`initialize` 声明 `peri.replay` + `peri.historyPagination`，而注册表**刻意**留 `PeriCaps::default()`；响应必须带 `periLoadGeneration` 与 `nextCursor`/`snapshotToken`；每条回放通知带 `periReplay=true`、代次与响应**同源**、`periEntryId` 照旧。
   - 为什么不重复：既有正向用例（`s9_recheck_a_negotiated_connection_still_receives_the_replay_marks`）同样绕过 `initialize` 请求；本用例同时把「注册表比连接更保守」这一方向钉住（门若回读注册表，标记会消失）。
3. `s9c_verification_a_never_initialized_in_process_connection_keeps_the_replay_marks`（`:1282`，判据 2c）
   - 断言：**从不** `initialize`，注册表为 `PeriCaps::default()`；响应与通知仍带 `periReplay`/`periLoadGeneration`（同源代次），正文是完整 canonical；同时**没有** `nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`（分页仍失败关闭）。
   - 为什么不重复：既有用例的连接侧要么是 `Some(PeriCaps::default())`（= 已 `initialize` 但未声明）要么是 `Some(pagination_caps())`，「从未 `initialize` 的 `all_enabled` 兜底」这一支在 wire 级没有用例覆盖。

### 2.3 指纹核对（复核者自证：只增了上面那 3 条用例）

```
shasum -a 256 peri-acp/src/host/connection.rs …（实现者报告的 8 条路径）
→ 7 条与实现者 §4 逐字符一致；history_page_wire_test.rs = 38cfd949… （含复核者追加段）
→ 从该文件剔除「独立复核（S9c-verification」段后重算 SHA-256 = 4b74f1580ab46b297646b3ec6440da2e81c0f7a086b4e74ddeecb4330c519e18
  （= 实现者报告的指纹，逐字符相等 ⇒ 该文件除复核者追加段外零改动）
```

---

## 3. 判据 3：回归与「未被顺带改动」

### 3.1 协商路径的既有断言（逐条）

1. `git diff` 对 **tracked** 测试文件的**删行**只有签名适配，没有断言删除：
   - `host/connection_test.rs`：`commit_initialize()` → `commit_initialize(PeriCaps::…)`（3 处），并**新增**更严的断言（第一次 initialize 生效：`assert_eq!(connection.caps(), PeriCaps::default())`；`caps()` 与 `history_pagination()` 的兜底分歧；`snapshot_for_request().caps()` 一致）。
   - `dispatch/session_replay_test.rs`：`replay_session_history(..., &caps)` → `(..., &caps, &marks)`（引入位置戳参数），断言只增不减。
2. **untracked** 测试文件（`history_page_wire_test.rs`、`requests_unnegotiated_replay_test.rs`、`history_page_test.rs` 等）在 HEAD 没有基线，`git diff` 无法隔离 S9c；改用两条独立证据：
   - 与 `.recon/S9b-acp-streaming-replay.md` §4.2 记录的 12 条用例及其断言**逐条对照**：现文件 15 条 = 12 条（名字、断言口径一致，第 12 条 `negotiated_initial_load_keeps_the_effective_set_and_the_entry_marks` 的 `periReplay`/`periLoadGeneration`/`periEntryId`/`periPosition` 断言原样保留）+ S9 复核者的 3 条；**无删减、无放宽**。
   - 负面控制（§7）证明：把门改回「不看连接」时，5 条标记相关用例**全红**——既有断言仍在场（不是空转）。
3. `#[ignore]` 反例探针已转正常用例：`history_page_wire` 与全量均为 `0 ignored`（S9 轮为 `1 ignored`）。
4. R8 一条**诊断日志断言**被删除（`capture_logs` / `LogBuffer`）：有 doc 说明与记录（`:733-739`），产品断言保留并改为更强的**触发条件**断言（`segment_calls().len()==1`、`calls[0].error` 含 `"behavior is unsupported here"`、`full_reads()==1`、交付 == canonical）；生产侧 `tracing::warn!`（`dispatch/session_replay.rs:338`）仍在。判定：**未被放宽**。

### 3.2 分页准入与 §5.1 模式分流未被顺带改动

1. 准入仍是 `connection_facts.history_pagination`（失败关闭）：`server_loop.rs:387`（`history_page::classify(&params, …)`）、`:395`（首游标附加）、`session_restore.rs:558`/`:580`（位置戳与回放分支）。
2. 实测：`pagination_fields_from_an_unnegotiated_connection_are_rejected`（A25：未协商带 `cursor` ⇒ `-32601`，不降级）、`a_page_request_is_answered_while_the_session_holds_the_execution_lock`（A21）、`host::history_page` 10 例、`dispatch::session_replay` 12 例、`host::connection` 4 例全绿：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::history_page     → 10 passed; 0 failed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- dispatch::session_replay → 12 passed; 0 failed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::connection       → 4 passed; 0 failed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- replay                 → 34 passed; 0 failed; 0 ignored（6.33s）
```

---

## 4. 判据 4：「修一类」是否成立

### 4.1 穷举命令

```
grep -rn "periReplay\|periLoadGeneration" --include=*.rs . | grep -v /target/ | grep -v _test.rs
```

### 4.2 生产发射点（3 处，全部吃连接 caps）

| 发射点 | 字段 | 门 | 门来源 | 判定 |
|---|---|---|---|---|
| `dispatch/session_replay.rs:596-603`（`replay_meta`，content chunk / tool call / tool update 共用） | `periReplay` + `periLoadGeneration` | `caps.replay` | 调用方 `caps`：`handle_load` 的两条回放路径都传 `connection.caps`（`session_restore.rs:590`、`:600`）；页出口传 `connection.caps`（`server_loop.rs:386`，且页分支先过失败关闭准入） | 正确 |
| `host/requests/session_restore.rs:836-841`（reminder 代次戳） | `periLoadGeneration` | `caps.replay` | 同一条 `send_projected` 链（`session_replay.rs:440`）传下的 `connection.caps` | 正确 |
| `host/requests/session_restore.rs:658-660`（响应 `_meta`） | `periLoadGeneration` | `connection_caps.replay` | `handle_load:537` 的本连接 caps | 正确 |

无关路径（已确认不打回放标记）：`session/event_sink.rs:157-205` live reminder（`replay:false`）；`dispatch/session_replay.rs:406` 的 `replay_session_history` **无生产调用点**（仅测试）。

**漏点：六字段范围内 0 个。**

### 4.3 边界（不在六个字段内，但复核者必须点名）

1. `_meta.peri.subagentFailure`（`event/tool_projection.rs:33-37`）在**回放与 live 两条路都无条件发射**：既不看连接也不看会话 caps（测试 `test_replay_tool_failure_keeps_safe_subagent_diagnostic_meta` 明确断言它**不随 `caps.replay` 变化**）。它是「无门控」，不是「继承会话旧能力」；触发需被回放的历史里存在 failed 子 Agent 工具结果。**前序切片既有行为，非 S9c 引入，不在本任务点名的 6 字段内。**
2. 命令表广播面仍会话级：`send_available_commands_update` 用 `ensure_session_caps`（`session_restore.rs:541`/`:637-643`），其条目 `_meta.periKind/periLevel/…`（`dispatch/commands.rs:126-155`）与 update 级 `skillNames`（`caps.skill_names`）因此仍可继承会话旧值；`session/resume` 同（`:683`）。实现者已按「另一功能面」记录，复核者同意这是**范围外**，但它确实是 §5.3 同类形状（继承旧客户端能力）的另一处，**不属于回放字段**。
3. live 通道的 reminder 形状仍读**会话级**注册表（`session/event_sink.rs:180-197`）：未协商连接在「该会话被 peri 连接注册过」时仍会收到结构化 `system-reminder`（而非 `-fallback`）。回放出口已在 S9c 改为连接 caps（`session_restore.rs:814`），live 出口未改。**同属范围外，且不是回放字段。**

---

## 5. 判据 5：无 DDL、无新依赖（实测）

```
git status --porcelain | grep -E "Cargo\.(toml|lock)" | wc -l                      → 0
git diff --name-only | grep -E "Cargo\.(toml|lock)" | wc -l                        → 0
git diff -U0 | grep -ciE "create table|alter table|drop table|create index|add column|drop column" → 0
git status --porcelain | grep -iE "\.sql|migration|schema"                          → 仅 2 个 *测试* 文件（remote/schema_*_test.rs，切片既有）
git status --porcelain | wc -l / grep -c "^??"                                       → 115 / 13（与复核前一致：复核者未新增文件）
shasum -a 256 peri-acp/src/host/requests/session_restore.rs → f15566da…（负面控制后已还原，diff 与备份逐字节相同）
```

---

## 6. 判据 6：全量 `-p peri-acp --lib`

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
第 1 次：test result: FAILED. 883 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 262.87s
        失败：session::command::compact::tests::report_tests::test_compact_report_manual_full_and_cold_reload
              panicked at peri-acp/src/session/command/compact_report_test.rs:128: called `Result::unwrap()` on an `Err` value: DatabaseUnreadable
单跑对照：./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- session::command::compact::tests::report_tests::test_compact_report_manual_full_and_cold_reload
        → test result: ok. 1 passed; 0 failed; finished in 1.37s
第 2 次（同命令）：test result: ok. 884 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 268.66s
```

1. 该失败是**既有并行负载 flake**，非 S9c 引入：同一条用例、同一处 panic 在 S9c 之前已被两轮记录（`.recon/S9b-acp-streaming-replay.md` §4.3、`.recon/S9-verification.md` §5.5），且失败面在 `session/command/` 的 compact 报告测试（与回放门无关）。
2. 计数核对（自洽）：S9 轮 `879 passed + 1 ignored = 880`；S9c 后实现者 `881 passed + 0 ignored`（= 880 + 1 条新正向用例）；复核者再加 3 条 ⇒ **884**，与两次全量的总数一致。
3. `-- replay`（34 例）在本轮与 wire 过滤（13 例）各跑 1 次全绿，未复现 R8 的日志断言 flake（该断言已按 §3.1 第 4 条移除）。

---

## 7. 反例 / 负面控制（复核者自查「断言不是空转」）

- 做法：临时把 `session_restore.rs:537` 的 `let connection_caps = &connection.caps;` 换成 `&cfg.session_manager.ensure_session_caps(req_session_id)`（**逐字复刻 S9c 之前的缺陷形状**：回放标记门读会话级注册表），跑 §2.1 同一命令。
- 结果（EXIT=101）：

```
test result: FAILED. 8 passed; 5 failed; 0 ignored; 0 measured; 871 filtered out; finished in 14.96s
failures:
  s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection   → 泄漏 ["periReplay x4", "periLoadGeneration x5"]
  s9c_verification_an_initialize_without_peri_extensions_yields_no_private_fields         → 同样泄漏（复核者新增，反向可红）
  s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field            → 标记消失（复核者新增，正向可红）
  s9c_verification_a_never_initialized_in_process_connection_keeps_the_replay_marks       → 标记消失（复核者新增，兜底支可红）
  s9_recheck_a_negotiated_connection_still_receives_the_replay_marks                      → 标记消失
  （8 条与分页/模式分流相关的用例仍全绿 ⇒ 失败面精确收敛在回放标记门）
```

- 还原：改回 `&connection.caps`，`shasum -a 256` 回到 `f15566da…`，`diff` 与备份逐字节相同，工作区文件计数仍 115（无残留）；复跑 §2.1 命令 13 passed / 0 failed。
- 结论：门被钉在**两个方向**上（未协商不得泄漏、协商不得丢失），既有与新增用例都不是空转。

---

## 8. 未验证与不确定项

1. **真实外部 stdio 客户端端到端**未跑：实测走 MPSC 真实 `ServerLoop` + 真实 `initialize` 请求，但未起真实 stdio 会话/第三方客户端（stdio 与 MPSC 共用同一 `ServerLoop`/`commit_initialize` 路径，`host/lifecycle.rs` 单一连接上下文）。
2. **Remote 后端的 ACP 层端到端**未跑（存储侧由 `peri-resources` 用例覆盖）。
3. **多连接时序**未做：负面控制固化的是**状态**（注册表被写过 + 当前连接未协商），不是两个真实连接先后/并发接入。静态上不构成缺口：`commit_initialize` 只认第一次、`dispatch_request` 逐请求串行、`facts()` 与页分支的 `caps()` 之间不可能插入另一次 `initialize`（`run` 循环 `await` 派发，无 spawn）。
4. **响应之后到达的异步通知**未被 V1/V2/V3 的排空覆盖（排空只覆盖「响应之前入队」的通知）。静态判定：响应后的异步发射点只有命令表重广播（`periKind`/`skillNames` 等命令面字段）与 session-info，**不是**六个回放字段的发射点；未加等待窗口是刻意避免引入时限依赖。
5. **`_meta.peri.subagentFailure` 无门控**（§4.3 第 1 条）与 live reminder / 命令广播的会话级继承（§4.3 第 2、3 条）：复核者只做静态定位，未写用例、未评估修复代价（范围外）。
6. 未跑 doctest 与集成测试目标（`--lib` 之外）；未重测性能预算（`perf-evidence.md` 管辖）。

---

## 9. 结论

1. **§5.3「不能继承会话内保存的旧客户端能力」对六个 Peri 私有回放 / 分页字段全部成立，无残留**：`periReplay`、`periLoadGeneration`（含响应 `_meta` 与 reminder 戳）全部改吃**本连接** `initialize` 的协商值（`ConnectionFacts::caps`）；`nextCursor`、`snapshotToken`、`periEntryId`、`periPosition` 本来（S9b 起）就只由连接事实 `history_pagination`（失败关闭）决定。§2 三类连接实测 + §7 负面控制共同支撑；分页准入方向未被顺带放松。
2. **「修一类」成立但有明确边界**：生产发射点共 3 处（§4.2），全部门控正确、无漏点；`send_available_commands_update` 与 `session/resume` 的**命令广播面**、live 通道 reminder 形状、以及 `_meta.peri.subagentFailure` 仍属会话级/无门控——三条都**不是回放字段**，实现者已按范围外记录（前两条），复核者同意其判读（§4.3）。
3. 无 DDL、无新依赖、无 git 状态改动；工作区保持未提交，唯一改动是复核者 3 条用例（可用 §2.3 的指纹重建逐字节核销）。
4. 唯一非绿记录是全量首跑的 1 条**既有并行 flake**（`test_compact_report_manual_full_and_cold_reload`，`DatabaseUnreadable`；单跑绿、二次全量 884/0），与 S9c 的门控改动无关。
