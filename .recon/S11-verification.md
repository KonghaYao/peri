# S11 复核报告 — `initial_history_cursor` 锚点可展示性修复（P1）

复核者：独立对抗性复核（不做实现、只做证伪）。复核对象 = `peri-acp/src/dispatch/session_load.rs`
的 `initial_history_cursor` 锚点规则 + `peri-acp/src/host/history_page_wire_test.rs` 的两个回归用例
+ 删除 `peri-resources/tests/zz_audit_history_anchor_probe.rs`。

## 0. 结论表

| # | 判据 | 结论 |
| --- | --- | --- |
| 1 | 无漏（可展示 canonical 行不得既不在首屏、又不被任何一页交付） | **PASS**（前提不变量已独立验证；非生产形状列边界） |
| 2 | 无重（页不得重复交付首屏已展示的可展示条目） | **PASS** |
| 3 | 兜底分支 `or_else(last)` 不谎报、不重不漏 | **PASS** |
| 4 | 判据同源（锚点与分页出口同一条规则；无第三条判据） | **PASS** |
| 5 | 两个新用例非空转（旧实现应失败） | **PARTIAL**（用例 1 是证伪器；用例 2 在旧实现上同样通过 = 不具证伪力） |
| 6 | 同类残留（别处是否用未过滤下标当锚点/已交付边界） | **PASS**（唯一生产实现；其余为位置推进指针） |
| 7 | 边界不变（无 DDL/迁移/依赖；`nextCursor` 形状、错误码、caps 门控不变） | **PASS** |

**总判定：可以交付。** 唯一非阻断建议见 §10。

### 复核期间的临时改动（全部字节级还原，sha256 取证）

| 文件 | 还原前 sha256 | 还原后校验 |
| --- | --- | --- |
| `peri-acp/src/dispatch/session_load.rs` | `6ef50e00e831dfe0503e98414f2efa5e4580ff36dce990983c31f8451b8391ad` | `shasum -a 256 -c` = OK |
| `peri-acp/src/host/history_page_wire_test.rs` | `55755bd42e9603366e2d6c010854da24fe930ca5b3685ff05ad17cabd95f75f7` | `shasum -a 256 -c` = OK |

复核做完后 `git status --porcelain` 仍是 171 行，与本修复相关的只有：

```
 M peri-acp/src/dispatch/session_load.rs
?? peri-acp/src/host/history_page_wire_test.rs
```

（`zz_audit_history_anchor_probe.rs` 已不在 `peri-resources/tests/` — 见 §9 判据 7 的检查。）
三次生产实现改动都是「改回 `payloads.first()` 取证 → `cp` 回备份 → sha256 校验」，未写任何 git。
探针源码留档：`.recon/S11-verify-probe.rs.txt`、`.recon/S11-verify-probe-batch2.rs.txt`。

---

## 1. 通用基线（构建 / 测试）

### 1.1 workspace 构建

```
$ cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history && ./scripts/cargo-rmcp-patched.sh build --locked --workspace
   Compiling peri-mcp-cron v0.2.0 (...)
   Compiling peri-middlewares v0.2.0 (...)
   Compiling peri-acp v0.2.0 (...)
   Compiling peri-tui v0.2.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 33s
```

**Result: PASS**

### 1.2 `peri-acp --lib`（交付物声明的 886）

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
test result: ok. 886 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 68.10s
```

（带探针时同一命令 = `892 passed; 0 failed`，即探针 6 个全绿且不干扰既有用例。）

### 1.3 `peri-resources --lib`（交付物声明的 497/0/26）—— **有抖动，见下**

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
test result: ok. 497 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 9.10s   # run A
test result: ok. 497 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 8.77s   # run B
test result: ok. 497 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 8.29s   # run C
test result: ok. 497 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 10.02s  # run D
test result: FAILED. 496 passed; 1 failed; ... finished in 10.17s                                   # run E
test result: FAILED. 495 passed; 2 failed; ... finished in 10.05s                                  # run F
```

抖动用例与原因（run F 原文）：

```
---- sessions::sqlite_store::connection::open_tests::canonical_database_and_symlink_aliases_share_one_lock_inode stdout ----
thread '...' panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:436:10:
called `Result::unwrap()` on an `Err` value: session database initialization is busy

---- sessions::sqlite_store::connection::open_tests::incompatible_schema_is_rejected_without_wal_or_database_changes stdout ----
thread '...' panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:377:66:
called `Result::unwrap()` on an `Err` value: session database initialization is busy
```

`busy` 来自冷开锁的 5 秒截止（`peri-resources/src/sessions/sqlite_store/connection.rs:17`
`SCHEMA_OPEN_LOCK_TIMEOUT = Duration::from_secs(5)`，:285 超时即 `"session database initialization is busy"`）。
归属判定证据：

```
$ git status --porcelain peri-resources/src/sessions/sqlite_store/connection.rs peri-resources/src/sessions/sqlite_store/connection_open_test.rs
(空 — 这两个文件未被任何未提交改动触碰)

$ grep -A3 '\[dependencies\]' peri-resources/Cargo.toml | grep peri-acp
peri-acp-types = { path = "../peri-acp-types" }      # 没有 peri-acp 依赖
```

**Result: PARTIAL（环境性）** — 该抖动与我复核的对象无因果关系（`peri-resources` 不依赖 `peri-acp`，
这两个文件也不在本轮任何未提交改动里；6 次里 4 次全绿）。交付方声明的 497/0/26 可复现，
但整条 `peri-resources` 测试链在并行负载下**预先存在**冷开锁超时抖动；建议单独立项跟踪，不作为本次修复的阻断项。

---

## 2. 判据 1 —— 无漏

**判据原文**：「锚点选择不会让任何**可展示**的 canonical 行既不在首屏、又永远不被任何一页交付。
请主动构造反例，尤其：①被排除行比"最旧可展示条目"**新**；②rewind 之后被恢复的可见行与仍被排除的行交错；
③继承 / 祖先前缀（不能让锚点落在祖先条目上）；④两轮以上 Full；⑤空页链（一页全是不可展示行）。」

**先给推理前提（可证伪的不变量）**：页链只覆盖「严格早于锚点」的行，所以「无漏」等价于
**全部被排除的可展示行都必须早于锚点**。锚点是有效视图里最旧的可展示行，因此需要生产不变量：

> I1：不存在「被排除的可展示行」比某个「未被排除的可展示行」更晚（排除集只覆盖最旧那一段）。

I1 的来源（静态证据，全仓 grep）：

```
$ grep -rn "excluded: true" --include=*.rs peri-agent/src peri-acp/src peri-resources/src | grep -v "_test.rs"
peri-agent/src/agent/compact_v2/full.rs:96:                    excluded: true,
peri-acp/src/host/requests/rewind.rs:405:            excluded: true,      # 该文件 337 行起是 #[cfg(test)] mod tests
peri-acp/src/host/requests/rewind.rs:443:                excluded: true,  # 同上（测试 helper）

$ grep -rn "set_excluded(" --include=*.rs . | grep -v "/target/" | grep -v "_test.rs"
（无生产调用者）
$ grep -rn "BaseMessage::system(" --include=*.rs peri-agent/src | grep -v "_test.rs"
peri-agent/src/session/subagent/factory/spawn.rs:351:        guard.append(BaseMessage::system(identity));   # 唯一 append 进 transcript 的生产点
（其余命中为 model adapter / model_bridge / executor 的**请求消息列表**，不进 transcript，已逐个读过：
  prediction.rs:55 `let mut messages: Vec<BaseMessage>`；intercept.rs:96 `result.messages.push(...)`）
```

`full.rs:86-126` 的 flag 选择是「全部未排除的自有 non-System 条目」→ 排除集恒等于「最新摘要之前那一段」，
且 `full.rs:124` 每次 Full **必然**追加摘要（`build_summary_message` = `BaseMessage::human`，
`full.rs:450-456`，可展示）。`rewind` 的 `rewind_clear_excluded_ids`（`store/history.rs:201-229`）在
「摘要已死」时清空全部排除、在「摘要存活」时只留最旧那一段 → 不能产生交错。

**实证：I1 在真实形状下成立**（探针断言，`.recon/S11-verify-probe.rs.txt` 的
`s11_exclusion_invariant_violations` + 四个形状用例）：

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- s11_probe
running 6 tests
test ...::s11_probe_child_session_delivers_every_displayable_own_row_exactly_once ... ok
test ...::s11_probe_two_full_rounds_keep_every_archived_row_reachable ... ok
test ...::s11_probe_an_empty_page_does_not_end_the_walk_early ... ok
test ...::s11_probe_fallback_anchor_keeps_an_archived_row_reachable ... ok
test ...::s11_probe_non_prefix_exclusion_is_out_of_the_anchor_rule_scope ... ok
test ...::s11_probe_rewind_keeps_anchor_and_first_screen_on_the_same_facts ... ok
test result: ok. 6 passed; 0 failed
```

覆盖：③继承前缀 + own 首条身份 System（`save_child` 真入口）；④两轮 Full（第一个摘要也进归档）；
⑤整页不可展示的空页链；②真实 `session/rewind` API；①非前缀排除（边界构造）。

**① 的边界（唯一无法满足判据的形状，且非生产可达）**：
`own = [h0*, h1, h2*, h3]`（h0 是前缀归档、h2 是**更晚**的归档行）→ 锚点 = h1，页只覆盖 h0，
**h2 既不在首屏也不在任何页**：

```
test ...::s11_probe_non_prefix_exclusion_is_out_of_the_anchor_rule_scope ... ok
   断言：delivered == [h0]；h2 ∉ 首屏 ∧ h2 ∉ 页  ← 「边界事实：比锚点更晚的归档行 h2 既不在首屏、
   也不被任何一页交付（漏项）」（探针把这个已知边界钉成可复现事实）
```

该形状**只能**用 `apply_compaction`（存储门面公开 API）直接构造：生产里唯一的排除写入者 `full.rs`
的选择规则不会产生非前缀排除，`rewind` 的清零规则还会把非前缀排除**修复**成前缀。因此判据 1 在
生产可达形状上成立；非前缀排除若将来由新调用方引入，锚点规则（以及整套「页严格更早于锚点」的分页
契约）都会漏项 —— 这是**分页设计层面的边界，不是本次修复引入或加重的**（旧实现同形同样漏，见 §10 表）。

**另外，判据 1 隐含的另一个前提我也验证了**：锚点来源（存储有效视图）与首屏来源（热态
`SessionState::history_payloads`）在 rewind 之后仍是同一份事实（探针
`s11_probe_rewind_keeps_anchor_and_first_screen_on_the_same_facts`，断言 `hot == effective`）。

**Result: PASS**（生产可达形状）＋已记录非生产边界。

---

## 3. 判据 2 —— 无重

**判据原文**：「没有任何一页会重复交付首屏已经展示过的可展示条目。」

论证：页读取条件恒为 `rowid < anchor.rowid`（`sqlite_store/session_data/history.rs:35-40`
`HISTORY_PAGE_ROWS_SQL`），锚点 = 有效视图里**最旧**的可展示行 ⇒ 任何可展示有效行都 ≥ 锚点
⇒ 不会出现在页里；页链自身按「上页最旧推进行」继续（`history_page.rs:600-632` `HistoryPagePlan::plan`
的 `anchor = entries.last()`），页内 DESC→正序，链内不重叠。兜底分支（判据 3）另证。

断言级取证（探针 + 既有 wire 用例，全部 `ok`）：
- `s11_probe_child_session_delivers_every_displayable_own_row_exactly_once`：`delivered == [h1,h2]`，
  且「首屏 ∪ 页 = 全部可展示自有行（去重后逐条相等）」；
- `s11_probe_two_full_rounds_keep_every_archived_row_reachable`：`!delivered.contains(&summary2)`；
- `s11_probe_rewind_keeps_anchor_and_first_screen_on_the_same_facts`：`!ids.contains(&h2)`；
- 既有 `initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen` /
  `a_negotiated_initial_load_marks_the_first_screen_with_page_comparable_identities`（26 个
  `history_page` 用例全绿，见 §1）。

**Result: PASS**

---

## 4. 判据 3 —— 兜底分支不谎报

**判据原文**：「`or_else(last)` 只在"有效视图里没有可展示条目"时生效；这种形状下它不会编造"没有更早历史"，
也不会制造重复或漏项。请用真实 API 构造至少一个该形状的用例（例如只有身份 System 条目；或 System + model-only reminder）。」

**实现读法**（`session_load.rs:126-131`）：`find(displayable).or_else(|| effective.payloads.last())`
⇒ 兜底只在 `find` 为 `None`（有效视图**无任何可展示行**）时生效 ⇒ 此时不存在「更旧的可展示有效行」，
故无重；锚点取**最新**的有效行，故全部归档行都严格早于锚点，故无漏（前提 I1）。

两个形状我都用真实 API 构造了（`apply_compaction` + `append_history`，sesssion 走 `create_bound_fixture`）：

1. 只有身份 System（交付方用例 2 同形）→ `nextCursor = null`（锚点 = System = 范围起点，确实没有更早行）。
2. `System + model-only reminder`（探针 `s11_probe_system_plus_model_only_reminder_yields_empty_page_not_a_lie`）
   → `nextCursor = Some(锚点 = reminder)`；下一页 `items = []`（System 被过滤）、`nextCursor = null`，
   整个链没有可展示丢失、没有重复。
3. 兜底分支的**真正证伪形状**（探针 `s11_probe_fallback_anchor_keeps_an_archived_row_reachable`）：
   `own = [身份 System, h1(归档,可展示), model-only reminder]`，有效视图 = `[System, reminder]`（无一条可展示）。
   兜底锚点 = reminder（最新有效行）⇒ 页链交付 `[h1]`：

```
test ...::s11_probe_fallback_anchor_keeps_an_archived_row_reachable ... ok
   断言：cursor.anchor == reminder.id()（不是有效视图第一条 System）; delivered == [h1]
```

**Result: PASS**

---

## 5. 判据 4 —— 判据同源

**判据原文**：「端到端比较 `initial_history_cursor` 的锚点选择与分页出口 `assemble_history_page`
（`peri-acp-types/src/history_page.rs`）的 `items` 过滤是否真的用同一条规则；有没有第三条"可展示性"
判据散落在 peri-tui / peri-sdk / peri-acp 的其他地方，与它不一致。」

**同一条规则（同一函数）**，两端都调用 `PersistedPayload::displayable_to_client()`：

```
# 锚点
$ sed -n '126,131p' peri-acp/src/dispatch/session_load.rs
    let anchor = effective
        .payloads
        .iter()
        .find(|payload| payload.displayable_to_client())
        .or_else(|| effective.payloads.last())
        .map(PersistedPayload::id);

# 分页出口装配 items
$ sed -n '660,666p' peri-acp-types/src/history_page.rs
            let payload = decode_row(row)?;
            if payload.displayable_to_client() {
                items.push(HistoryPageItem {

# 规则本体（唯一权威）
$ sed -n '66,76p' peri-acp-types/src/store/mod.rs
    pub fn displayable_to_client(&self) -> bool {
        match self {
            Self::Message(message) => !message.is_system(),
            Self::SystemReminder { reminder, .. } => reminder_egress_allowed(
                reminder.as_reminder(),
                crate::system_reminder::ReminderAudience::Tui,
            ),
        }
    }
```

**没有第三条判据**：所有相关出口都归约到同一对谓词（`!is_system()` / `reminder_egress_allowed(_, Tui)`）：

| 落点 | 谓词 | 与锚点规则关系 |
| --- | --- | --- |
| `peri-acp/src/dispatch/session_replay.rs:455-457`（回放出口，System 不产生 update） | `msg.is_system()` | 与 `!is_system()` 同一判据 |
| `peri-acp/src/dispatch/session_replay.rs:238-240` `reminder_visible_to_client` | `reminder_egress_allowed(_, Tui)` | **同一函数**（`displayable_to_client` 直接调用它） |
| `peri-acp/src/host/requests/session_restore.rs:804-813`（回放发送端） | `reminder_visible_to_client` | 同一判据 |
| `peri-acp/src/host/history_page.rs:349-355` `sub_item`（页子项装配） | `reminder_visible_to_client` | 同一判据 |
| `peri-acp/src/host/prediction_projection.rs:15` | `!is_system()` | 同一判据（另一功能面） |
| `peri-tui/src/kit/tui_render_unit/reminder.rs:87-96` | `audiences.contains(Tui)` + DiagnosticOnly + Security 严重度 | **客户端渲染守卫**，只会丢弃服务端已投递的东西，不参与「已交付集合」定义 |

端到端一致性反证：客户端合并只按 `entryId` 去重 + `position` 排序（`peri-tui/src/kit/history_paging.rs:273-301`
`merge_page`、`npm-packages/@peri-sdk/src/state/history-merge.ts:124-175`），**不**从首屏下标推导
「已交付边界」；因此锚点的选择不会与客户端边界判定分叉（另见判据 6 的扫描）。

**Result: PASS**

---

## 6. 判据 5 —— 用例非空转

**判据原文**：「两个新用例确实会因缺陷复现而失败（你可自行临时改回旧实现复现，复现后**必须恢复**并说明）；
用例断言的强度是否够（它们是否真的覆盖"原文可达"而不是只断言游标非空）。」

### 证伪复现（改回旧实现 `payloads.first()`，两段，均已 sha256 校验还原）

```
$ python3 - <<'PY'   # 把 session_load.rs 的 5 行锚点改回 payloads.first()
... OLD impl active (payloads.first())
$ shasum -a 256 -c /tmp/s11_session_load.sha256   # 复现后恢复
peri-acp/src/dispatch/session_load.rs: OK

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- initial_cursor_anchors_past_non_displayable_own_rows
test ...::the_initial_cursor_anchors_past_non_displayable_own_rows ... FAILED
panicked at peri-acp/src/host/history_page_wire_test.rs:1419:10:
存储里有压缩原文，不得谎称没有更早历史
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 885 filtered out

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- an_all_non_displayable_own_region_reports_no_earlier_page
test ...::an_all_non_displayable_own_region_reports_no_earlier_page ... ok      ← ★ 旧实现也通过
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 885 filtered out
```

- **用例 1（`the_initial_cursor_anchors_past_non_displayable_own_rows`）= 真正的证伪器**：旧实现
  FAILED，新实现 ok；断言强度也够——它不只断言游标非空，还走完整页链断言
  `delivered == vec![h1.id(), h2.id()]`、`next.is_none()`、且摘要不重复（"原文可达"而非"游标非空"）。
- **用例 2（`an_all_non_displayable_own_region_reports_no_earlier_page`）= 空转**：形状只有一行
  （`own = [System]`），`first()`、`find`+`last()`、甚至写死 System 三种实现给出同一个锚点，
  所以它在修复前后都通过；它确实**进入**了兜底分支（`find` 返回 `None`），但没有钉住兜底的**选择**
  （退回第一条 vs 退回最新一条），断言只有 `next_cursor.is_none()`，强度弱。
  ⇒ 若将来有人把兜底改回 `first()`，交付的两个用例都不会红（我的探针 3 会红，见 §4/§10）。

**Result: PARTIAL**（用例 1 合格；用例 2 不具证伪力 —— 非阻断，建议见 §11）

---

## 7. 判据 6 —— 同类残留

**判据原文**：「全仓（含 `peri-tui`、`peri-sdk`、`peri-acp/tests/`）是否还有别处用"有效视图第一条 /
`payloads.first()`"这类未经可展示性过滤的下标当**游标锚点或已交付边界**。给出文件:行号清单与判定。」

```
$ grep -rn "payloads.first()\|payloads\[0\]\|snapshot.payloads\[0\]" --include=*.rs peri-acp peri-acp-types peri-resources peri-tui peri-controller
peri-acp/src/dispatch/session_load.rs:126   ← 已修复处（唯一生产锚点）
其余全部命中在 *_test.rs / tests/（夹具取第 0 条做断言，非游标锚点）：
  peri-acp/src/host/requests_history_lifecycle_test.rs:209
  peri-acp/src/host/compact_command_test.rs:83 / compact_report_test.rs:131
  peri-acp/src/host/requests_unnegotiated_replay_test.rs:199
  peri-acp-types/src/store/history_test.rs:59,100
  peri-resources/... （均为测试）

$ grep -rn "HistoryCursor::new(" --include=*.rs . | grep -v /target/ 
peri-acp/src/dispatch/session_load.rs:134            ← 首屏锚点（本修复）
peri-acp-types/src/history_page.rs:677               ← 页链推进指针：entries.last()（本页最旧**推进**行）
peri-acp-types/src/history_page_test.rs ×3, peri-resources/.../session_history_page_test.rs:347,
peri-acp/src/host/history_page_test.rs:16            ← 测试

$ grep -rn "range_head" --include=*.rs . | grep -v /target/ | grep -v _test
peri-acp-types/src/history_page.rs:419,430,441-452,500-510   ← 契约类型与 resolve_browsing_range
peri-acp/src/dispatch/session_load.rs:133                     ← 收尾判据（本修复）

$ grep -rn "\.get(0)\|\[0\]" peri-tui/src/kit/history_paging.rs peri-tui/src/kit/history_page_consumer.rs
（空）
$ grep -rn "\[0\]\|\.first()" npm-packages/@peri-sdk/src/state/history-merge.ts \
        npm-packages/@peri-sdk/src/state/session-docs.ts npm-packages/@peri-sdk/src/state/chat-projection.ts
（空）
$ grep -rn "anchor\|nextCursor" --include=*.rs peri-acp/tests
（空 — 集成测试目录里没有锚点规则的副本）
```

判定：
- **唯一的生产「首屏已交付边界」实现**就是被修复的这一处 ⇒ 没有第二份会漂移的副本；
  被删除的 `peri-resources/tests/zz_audit_history_anchor_probe.rs` 原本是一份复刻判据的探针，
  删掉它减少了副本而不是减少覆盖（ACP 侧已有真用例）。
- `history_page.rs:677` 的页内锚点**不是**可展示性边界，而是「位置推进指针」——它取本页最旧的
  **推进行**（含不可展示行，位置照常推进），下一页读 `rowid <` 它；把不可展示行也算进推进是契约要求
  （`history_page.rs` 模块文档：「它们不进入页，但**位置照常推进**」），不构成漏/重。
- TUI / SDK 的合并只按 `entryId` 去重 + `position` 排序，不从首屏下标推导边界 ⇒ 无同类残留。

**Result: PASS**

---

## 8. 判据 7 —— 边界不变

**判据原文**：「该修复没有引入新的 DDL / 迁移 / 依赖；没有改变 `nextCursor` 的 JSON 形状、错误码、caps 门控语义。」

```
$ grep -n "CREATE TABLE\|ALTER TABLE\|migration\|sqlx::query" peri-acp/src/dispatch/session_load.rs
(空 — 无 DDL)

$ git status --porcelain | grep -E "Cargo.toml|Cargo.lock|\.sql$|migrations"
(空 — 本轮 171 个未提交文件里没有清单/锁文件/SQL 迁移)

$ ls peri-resources/tests/ | grep -i zz_audit
(空 — 审计遗留探针文件已按说明删除，exit=1)
```

`nextCursor` 形状：修复只改**喂给** `attach_initial_cursor` 的锚点 id 取值，未改响应装配
（`peri-acp/src/host/history_page.rs:271-320`，不在本次修复改动范围内）。取证：

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- s11_probe_rewind_keeps_anchor_and_first_screen_on_the_same_facts
test ... ok        # 该探针用真实 HistoryCursorEnvelope::decode + handle_page 走 wire 形态并逐页断言 entryId/items

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page
test result: ok. 26 passed; 0 failed
（含 a_negotiated_initial_load_marks_the_first_screen_with_page_comparable_identities、
  pagination_fields_from_an_unnegotiated_connection_are_rejected、malformed_cursor…、
  a_cursor_from_another_session_is_its_own_error、
  host::requests::tests::rewind_cases::test_rewind_expires_previously_issued_history_cursors = ok
  → -32012 / -32011 / -32602 映射与 caps 门控原样可用）
```

caps 门控语义：新读（`load_active_snapshot`）只在 `attach_initial_cursor` 内发生，而它只由
`load_initial_cursor = connection_facts.history_pagination` 触发
（`peri-acp/src/host/server_loop.rs:383-397`、:499-508，`history_page::classify` 同源）⇒ 未协商连接
行为完全不变（`s9_recheck_*` 与未协商用例全绿）。

**Result: PASS**

---

## 9. 我构造的反例（证伪结果一览）

| 探针（源码见 `.recon/S11-verify-probe.rs.txt` / `-batch2.rs.txt`） | 形状 | 修复后 | 旧实现 | 说明 |
| --- | --- | --- | --- | --- |
| `s11_probe_child_session_delivers_every_displayable_own_row_exactly_once` | `save_child` 真继承前缀 + own 首条身份 System + 一次 Full | ok | **FAILED**「存储里有压缩原文，不得谎称没有更早历史」 | 判据 1③、2；覆盖真实子会话启动形状 |
| `s11_probe_two_full_rounds_keep_every_archived_row_reachable` | 两轮 Full（第一个摘要也归档）+ 尾部追加 | ok | **FAILED**「归档区存在时必须给游标」 | 判据 1④、2 |
| `s11_probe_an_empty_page_does_not_end_the_walk_early` | 有效视图 = `[m1(仅模型), m2(仅模型), 摘要]`，limit=2 → 空页 + 归档页 | ok | **FAILED**（锚点落到 System，`left != right`） | 判据 1⑤、2 |
| `s11_probe_fallback_anchor_keeps_an_archived_row_reachable` | 有效视图无任何可展示行 + 更老的归档可展示行 | ok | **FAILED**「存储里还有一条更老的可展示行，不得谎称没有更早历史」 | 判据 3（兜底分支的证伪器） |
| `s11_probe_system_plus_model_only_reminder_yields_empty_page_not_a_lie` | `[System, 仅模型 reminder]` | ok | **FAILED**（旧实现 `nextCursor = null`） | 判据 3；对用户可见结果无差别，仅钉住分支选择 |
| `s11_probe_rewind_keeps_anchor_and_first_screen_on_the_same_facts` | 真实 `session/rewind`：归档前缀存活 + 摘要存活 | ok | 未单独复现（旧实现在同形下锚点=System→null） | 判据 1②、2；同时证明热态==存储有效视图 |
| `s11_probe_non_prefix_exclusion_is_out_of_the_anchor_rule_scope` | 非前缀排除（h2 比锚点更晚被排除） | ok（**断言漏项存在**） | 同形（锚点同样 = h1） | 判据 1①：记录**已知边界**，非生产可达 |

即：**在旧实现下实测 5 个探针全红**（batch1 的 4 个 + `s11_probe_system_plus_model_only_reminder_yields_empty_page_not_a_lie`；另两个探针 `…non_prefix_exclusion…` / `…rewind_keeps_anchor_and_first_screen…` 未对旧实现复跑，它们分别用于钉边界与钉热态/存储一致性），**修复后 7/7 绿**；唯一不能推翻的形状（非前缀排除）是**设计层面**的已知边界，
不是本次修复的回退，生产里也没有调用者能造出它（§2 的 I1 证据）。

未能推翻的构造尝试与原因：
1. 「被排除行比最旧可展示条目新」——只有绕过 `full.rs` 直接调 `apply_compaction` 才能造出；
   生产写入者唯一（`full.rs:96`），其选择规则让排除集恒为「最新摘要之前」那一段。
2. 「rewind 之后恢复行与仍排除行交错」——`rewind_clear_excluded_ids` 只保留最旧那一段，
   其余全清；交错形状不可达（探针的 I1 断言在 rewind 后仍成立）。
3. 「锚点落在祖先条目上」——锚点取 `effective.payloads`（自有区），祖先在 `inherited` 里；
   探针断言锚点 ∉ 继承区 id，且页里从不出现继承 id。
4. 「空页链提前收尾」——`history_page.rs` 的 `has_older` 由未过滤候选行数判定，空页仍带游标；
   探针证明空页后仍能取到归档行。
5. 「首屏与锚点来源分叉」——rewind 之后热态与存储有效视图逐条相等（探针断言）。

---

## 10. 需要修订的点（非阻断建议）

1. **补一个兜底分支的证伪用例**（唯一实质建议）。交付的用例 2 在旧实现上同样通过，兜底分支的
   *选择*（`last()` vs `first()`）没有任何用例钉住。可直接采用我的探针 3 形状
   （有效视图无任何可展示行 + 更老的归档可展示行 ⇒ 归档行必须仍可翻出），
   源码见 `.recon/S11-verify-probe.rs.txt` 的
   `s11_probe_fallback_anchor_keeps_an_archived_row_reachable`（复核时已从交付文件删除，
   文件字节级还原，sha256 见 §0）。
2. **文档措辞**（可选）：`session_load.rs` 的兜底注释写「退到**最新**一条自有 payload」，
   实现是「最新一条**有效**（未排除）自有 payload」。生产不变量下两者等价
   （`max(排除集) < max(有效集)`），但措辞按实现写会更准确（例如「最新一条有效自有 payload」）。
3. **残余风险（与本修复无关）**：`peri-resources` 冷开锁 5s 抖动（§1.3），建议单独立项。
4. **边界登记（与本修复无关，建议入档）**：若将来有调用方写出**非前缀排除**
   （例：只归档一条中间消息并追加摘要），则「页严格更早于锚点」的整套分页契约会漏掉比锚点更晚的归档行
   （我构造的 §2 边界形状）。当前唯一写入者 `full.rs` + `rewind` 清零规则保证了不变量 I1。

---

## 11. 结论

**该修复可以交付**：判据 1/2/3/4/6/7 PASS，判据 5 PARTIAL（用例 2 不具证伪力，非阻断）；
无生产可达的反例；旧实现下 5 个独立探针全部红，修复后 7 个探针全绿，全部改动已字节级还原。
