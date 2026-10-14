# S12 三处验收缺口侦察报告（A14 / sessionWorkspaceV1 门控 / 「拒绝不得触发 initial load」）

- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination` @ `3ccf75864`（工作树干净，侦察期未改任何代码）
- 需求原文：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`（§9 = A01~A25）
- 侦察方式：**只读**。静态定位 + 运行既有测试（`./scripts/cargo-rmcp-patched.sh`），未新增/修改任何测试或实现。
- 前序记录：`.recon/A-acceptance-report.md:80`（A14 记为「未验证」）、`.recon/final-adversarial-audit.md:303`、`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`（D11/D12）

---

## 0. 结论摘要

1. **A14 = GAP（部分覆盖）**：判据三句里前两句（「原文仍可由存储访问」「功能不再依赖核心内存持有全量历史」）有直接用例；**第三句「父收缩不改变子快照或分页内容」无任何直接用例**。仅存在**方向相反**的等价断言（子 Full 不改父），且**分页内容面在两个方向上都未测**。实现层按结构看成立（行按 `thread_id` 隔离 + fork 重新分配 message id），缺的是断言而非行为。
2. **`peri.sessionWorkspaceV1` 无 D12 同形偏差**：5 处判定全部读 `SessionManager::effective_host_caps()`（**进程级** `pending_caps`，最后写入者胜），**不读**会话级 `caps_registry`，因此不存在「连接 A 写过的会话注册表泄漏给连接 B」这一形状。但它与 `peri.historyPagination`（连接级、失败关闭）和 `peri.replay`（连接级）**不同源**，构成另一处可在**单条连接内**复现的偏差（重复 `initialize` 时三门互相矛盾）。
3. **「拒绝不得触发 initial load」= 结构成立、断言只有 1/11 条拒绝路径落到 wire，且落到的这条没有断言通知面与响应面**。现有唯一 wire 级拒绝用例只断言「错误码 + 热态 history 条数不变」；9 类拒绝（含 A25 点名的非法 cursor／跨会话 cursor／setup 参数）只有 `classify` 纯函数级断言，没有任何一条断言「零回放通知」。

---

## 1. A14：既有 fork／冻结子上下文，父会话后续 Full

### 1.1 判据原文（逐字抄录）

`/Users/konghayao/Desktop/peri-transcript-history-requirements.md:273`：

```
| A14 | 既有 fork／冻结子上下文，父会话后续 Full | 原文仍可由存储访问，功能不再依赖核心内存持有全量历史；父收缩不改变子快照或分页内容 |
```

（需求 §9 前置语，同文件 `:256`：「以下为交付目标，不是已完成的性能测量。」）

判据拆成三个可判定的断言：

- **A14-a** 原文仍可由存储访问
- **A14-b** 功能不再依赖核心内存持有全量历史
- **A14-c** 父收缩（父会话后续 Full）不改变**子快照**或**分页内容**

### 1.2 逐句核对

| 判据 | 覆盖 | 证据 |
| --- | --- | --- |
| A14-a 原文仍可由存储访问 | **有直接用例** | `peri-acp/src/dispatch/session_fork_test.rs:202 fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage`：source 先 Full（2 条原文 `excluded`）→ fork → 目标存储 `load_session_history` 仍 3 条且含 `"released question"` |
| A14-b 功能不依赖内存持有全量 | **有直接用例** | 同一用例：`published.len() == 1`（只发布有效视图）、`load_active_snapshot(&fork_id).payloads == published`；实现上 `load_fork_source` 从存储（`load_session_snapshot`）读，不从内存读 |
| A14-c 父收缩不改变子快照 | **无直接用例**；只有**反方向**等价断言 | `session_fork_test.rs:74 forked_payloads_have_independent_ids_flags_and_compaction_lifecycle`：对 **fork 目标**做 Full，断言 **source** 的 flags 不变（`:193-196`）。方向与 A14-c 相反 |
| A14-c 父收缩不改变**分页内容** | **两个方向都无用例** | 分页 wire 测试文件里 `fork` 只出现在注释（`history_page_wire_test.rs:1340`）；无任何用例同时触及 fork 与 `load_history_page` |

### 1.3 证据

#### (1) 判据第三句的检索证据（零命中）

```bash
$ cd .worktrees/peri-transcript-history
$ grep -rn "apply_compaction\|CompactionChange" --include=*_test.rs peri-acp/src
peri-acp/src/host/requests_unnegotiated_replay_test.rs:141:    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();   # 门面委托宏
peri-acp/src/host/requests_rewind_cases_test.rs:214:    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();   # 门面委托宏
peri-acp/src/host/history_page_wire_test.rs:61 / 1374 / 1509            # 无 fork
peri-acp/src/host/requests_history_lifecycle_test.rs:213               # cold_load_publishes_effective_history_only，无 fork
peri-acp/src/host/requests_legacy_test.rs:971                          # 门面委托宏
peri-acp/src/host/compact_history_fixture_test.rs:213                   # 门面委托
peri-acp/src/dispatch/session_fork_test.rs:176 / 224                   # ★ 唯一 fork+Full 同现：Full 打在 fork 目标上

$ grep -rn "fork" peri-acp/src/host/history_page_wire_test.rs
1340:// ─── A15 回归：own 区首条是不可展示条目（subagent / fork / bg 子会话的真实形状）──────
# 仅注释命中；分页 wire 测试没有 fork 夹具
```

`session_fork_test.rs:193-196`（唯一的 fork×Full 断言，方向为「子 Full 不改父」）：

```rust
    let source_snapshot = resources.load_session_snapshot(&source_id).await.unwrap();
    assert!(source_snapshot.flags[&source_tool_id].truncated);
    assert!(!source_snapshot.flags[&source_tool_id].excluded);
```

#### (2) 实跑：fork 相关既有用例全绿

```bash
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- fork
running 10 tests
test dispatch::session_fork_test::fork_rejects_source_with_incomplete_tool_calls ... ok
test dispatch::session_fork_test::fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage ... ok
test dispatch::session_fork_test::forked_payloads_have_independent_ids_flags_and_compaction_lifecycle ... ok
...
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 877 filtered out; finished in 2.81s

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- fork
running 11 tests
test messages::tool_pairing::tests::fork_and_compaction_keep_visible_ancestor_gap_without_reviving_excluded_pair ... ok
test session::subagent::tests::child_wire_capture_test::fork_child_request_keeps_single_identity_with_parent_ancestors ... ok
...
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 1078 filtered out; finished in 0.42s
    # 注：`.recon/final-adversarial-audit.md:303` 的「fork 系列 11 passed」对应的是这条
    #     `-p peri-agent --lib -- fork` 过滤，不是 child_wire_capture_test 文件本身（该文件 7 个测试）

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- fork
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 517 filtered out

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- fork
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 573 filtered out

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage
running 1 test
test dispatch::session_fork_test::fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 886 filtered out; finished in 0.17s
```

**全绿不等于 A14 达成**：以上没有任何一条用例构造「先 fork，后对**父/源**会话执行下一次 Full，再读子快照/子分页」。

#### (3) 实现层为什么「看起来成立」（静态依据，未加断言）

1. fork 把源行**复制成新行、新 message id**，写进目标 thread：

```rust
// peri-acp/src/dispatch/session_fork.rs:75-80
let forked = remap_fork_history(&source.payloads, &source.flags, MessageId::new);
let published = forked.active_payloads();      // 只发布有效视图
resources.save_fork(&ForkSnapshot { target: NewSession { thread_id: target_id.clone(), ... }, ... })
```

```rust
// peri-acp-types/src/store/history.rs:43-54  —— 复制结果保留完整 canonical，只有发布面收缩
pub fn active_payloads(&self) -> Vec<PersistedPayload> {
    self.payloads.iter().filter(|p| !self.flags.get(&p.id()).is_some_and(|f| f.excluded)).cloned().collect()
}
```

2. 父会话后续 Full 的 flags 写入**按 thread 限定**，物理上碰不到子行：

```sql
-- peri-resources/src/sessions/sqlite_store/session_data.rs:866-868（apply_message_projections）
UPDATE messages SET truncated = ?1, excluded = ?2, projection = ?3
 WHERE message_id = ?4 AND thread_id = ?5
```

3. 分页读取同样按 thread 限定（`peri-resources/src/sessions/sqlite_store/session_data/history.rs:29-42`）：

```sql
HISTORY_PAGE_ROWS_SQL: SELECT message_id, rowid, length(CAST(content AS BLOB)) FROM messages
  WHERE thread_id = ?1 AND (?2 IS NULL OR rowid < (SELECT m.rowid FROM messages m WHERE m.thread_id = ?1 AND m.message_id = ?2))
  ORDER BY rowid DESC LIMIT ?3
ACTIVE_ENTRY_POSITIONS_SQL: SELECT message_id, rowid FROM messages WHERE thread_id = ?1 AND excluded = 0 ...
```

4. 子侧「冻结子上下文」（祖先区）在收缩时被显式保护：

```rust
// peri-agent/src/session/transcript.rs:802-812（shrink_after_full_commit）
fn shrink_after_full_commit(&mut self) {
    if self.ancestor_len >= self.entries.len() { return; }
    let mut keep_count = self.ancestor_len;   // 继承/冻结区整体不动
    ...
}
```

**结论：A14-c 是「实现有、断言无」。** 缺口层级 = **只有间接覆盖（方向相反）+ 分页面零覆盖**，不是实现缺失。

### 1.4 A14 最小补齐方案（新增 1 条用例即可闭环）

- 落点：`peri-acp/src/dispatch/session_fork_test.rs`（复用现成夹具 `open_facade` / `create_source`，已在 `:25-56`）。
- 用例名建议：`a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact`
- 步骤与断言（全部在门面层，等价于 ACP 使用的存储路径）：
  1. `create_source(SOURCE)` → `append_history` 3 条 → `load_fork_source` + `fork_bound_session` 得 `fork_id`；
  2. 记录 `before = load_session_snapshot(&fork_id)`（payload ids / flags / 内容）与 `before_page = load_history_page(&fork_id, None, 50, PAGE_BYTE_BUDGET)`（items 身份 + `next_cursor`）；
  3. 对 **SOURCE** 执行新的 `apply_compaction`（把尚未排除的源条目 `excluded: true` + 追加新摘要）；
  4. 断言 `load_session_snapshot(&fork_id)` 与 `before` **逐条等价**（ids、flags、内容），`load_history_page(&fork_id, None, …)` 与 `before_page` 等价；
  5. 反向对照：`load_session_snapshot(&SOURCE)` 的新排除事实**只出现在源**（子行 `excluded` 不变）。
- 若要求覆盖 wire 面，可另加一条 `history_page_wire_test.rs` 用例（该文件夹具无 fork，需先 `session_resources.save_fork` 建子 thread 再发 `session/load{ cursor }`）；**最小闭环不必须**。

---

## 2. `peri.sessionWorkspaceV1` 判定来源与偏差判定

### 2.1 判定来源（文件:行号）

解析点（唯一）：

```rust
// peri-acp-types/src/peri_caps.rs:103（PeriCaps::from_client_meta）
session_workspace_v1: meta_bool(meta, "peri.sessionWorkspaceV1"),
// 字段定义 :65；反向回显 :168（to_agent_meta）
```

写入点（**进程级**）：

```rust
// peri-acp/src/host/requests/session_lifecycle.rs:135（handle_initialize）
cfg.session_manager.set_pending_caps(peri_caps.clone());
```

```rust
// peri-acp/src/session/caps.rs:11-14 / :29-37
pub fn set_pending_caps(&self, caps: PeriCaps) { *self.inner.pending_caps.lock() = Some(caps); }
pub fn effective_host_caps(&self) -> PeriCaps {
    self.inner.pending_caps.lock().clone().unwrap_or_else(PeriCaps::all_enabled)
}
```

读取点（**5 处，全部走 `effective_host_caps()`**）：

| # | 位置 | 用途 |
| --- | --- | --- |
| 1 | `peri-acp/src/host/requests/session_restore.rs:326-330`（`response_identity`） | load/resume/fork/new 响应是否附 `_meta.peri.sessionWorkspaceV1`（装配在 `:315`） |
| 2 | `peri-acp/src/host/requests/session_restore.rs:393-394`（`handle_context`） | `peri/session_context` 准入 |
| 3 | `peri-acp/src/host/requests/session_restore.rs:428-429`（`handle_metadata`） | `session/metadata`、`peri/session_history` 准入 |
| 4 | `peri-acp/src/host/requests/session_lifecycle.rs:570-571`（`handle_list`） | `session/list` 带 `_meta.peri.sessionWorkspaceV1.scope` 的扩展分支 |
| 5 | `peri-acp/src/host/requests/storage_v2.rs:40-41`（`archive_session`） | `peri/session/archive` 准入 |

**判定口径 = `SessionManager.pending_caps`（进程级、最后写入者胜）；既不是连接级，也不是会话级注册表。**

### 2.2 与 `peri.historyPagination` / `peri.replay` 的关系

三者同一个解析入口（`initialize_peri_caps`，`session_lifecycle.rs:145-155`）、同一个 `PeriCaps` 结构，但**存回与读出的位置不同**：

| 能力 | 存回位置 | 读出位置 | 未 `initialize` 时 |
| --- | --- | --- | --- |
| `peri.historyPagination` | `ConnectionContext.negotiated_caps`（`host/connection.rs:42-47 commit_initialize`，**只记第一次**） | `ConnectionContext::history_pagination()`（`connection.rs:72-78`）→ `ConnectionFacts.history_pagination`（`:145-150`） | **false（失败关闭）** |
| `peri.replay`（含 `periLoadGeneration`） | 同上 | `ConnectionContext::caps()`（`connection.rs:54-58`）→ `ConnectionFacts.caps` | `all_enabled()`（兜底，进程内 TUI 语义） |
| `peri.sessionWorkspaceV1` | `SessionManager.pending_caps`（**进程级**，`caps.rs:12`） | `SessionManager::effective_host_caps()`（`caps.rs:31-37`） | `all_enabled()`（兜底） |

连接级与进程级两条写入路径在同一次 `initialize` 里同时发生：

```rust
// peri-acp/src/host/server_loop.rs:526-529
if method == "initialize" && result.is_ok() {
    let caps = requests::session_lifecycle::initialize_peri_caps(&params);
    connection.lock().await.commit_initialize(caps);   // 只记第一次（connection.rs:43 `if !self.initialized`）
}
```

### 2.3 偏差判定

**判定：不存在 D12 同形偏差。** D12 的形状是「**会话级** `caps_registry` 被先前连接写过，重连的同一会话被旧值继承」（`session_restore.rs:541`、`A-acceptance-report.md` §5.6）。而 `sessionWorkspaceV1` 的 5 处读取**从不读 `caps_registry`**（不是 D12 那一类偏差）。也不存在「连接 A 写过 → 连接 B 拿到不该拿的字段」在**会话级**的实现路径。

**但存在同类家族的第三处偏差：进程级「最后写入者胜」。** 具体两条可达场景：

**构造 A（单连接即可复现，无需多连接宿主）**：同一条连接发两次 `initialize`
1. `initialize` #1：`_meta = {"peri.sessionWorkspaceV1": true}` → `set_pending_caps(workspace=true)`；`commit_initialize` 记下连接级 = {workspace=true}
2. `initialize` #2：`clientCapabilities` 不带任何 peri 扩展 → `set_pending_caps(默认全 false)`；`commit_initialize` 被 `if !self.initialized` 挡掉（`connection.rs:43`）
3. 结果：同一连接上 `session/load` 的 workspace 门（进程级）= **false**（该连接声明过的 workspace 身份被吞掉），而分页门（连接级）= **true**（仍在发 `nextCursor`/`snapshotToken`）。**两个门对同一次协商给出相反答案。**
4. 顺序反过来（先裸后带）则得到相反组合：workspace 门 true、分页门 false。
5. 仓库无「第二次 `initialize` 必须拒绝」的约束：`handle_initialize`（`session_lifecycle.rs:124-139`）没有任何 `initialized` 检查，`grep -rn "already initialized"` 在全仓只命中 `peri-acp/src/session/access.rs:89`、`mcp-packages/config/src/lib.rs:26` 两处无关注释/报文。

**构造 B（需同进程多连接）**：
1. 连接 A 真实发 `initialize`（声明 `peri.sessionWorkspaceV1`）→ 进程级 `pending_caps` 被写上；
2. 连接 B **从不** `initialize` → `effective_host_caps()` 返回 A 的协商值 → B 的 `session/load` 响应出现 `_meta.peri.sessionWorkspaceV1`（含 `workspace`/`binding`/`title`/`execution_environment_id`，装配见 `session_restore.rs:315`）——B 从未协商过该能力；
3. 但仓库生产接线只有**每进程一个** `ConnectionContext`（`host/mod.rs:381`），部署形态是 stdio 每进程一条连接、TUI 进程内 MPSC 一条连接，**该形状目前只能在测试夹具里构造**（`history_page_wire_test.rs::spawn_loop*` 支持同一 `cfg` 起多个 loop）；
4. 注意区别：构造 B 里 B 的 pagination 门仍失败关闭（不泄漏分页字段），拿到的只是 workspace 私有块。这属于 D12 复核点名的「范围外同类形状」清单（`spec/issues/2026-10-13-transcript-shrink-history-pagination.md` D12 第 3 条列了 `subagentFailure`、命令广播面、live reminder 三处），**本分支范围外**。

**结论**：workspace 门不需要按 D12 那样改；若要与本分支的门控口径统一，最小改动是让这 5 处改读**连接级** `ConnectionFacts`（`sessionWorkspaceV1` 属于**准入**判定 ⇒ 应同 `historyPagination` 一样失败关闭），代价是 TUI 进程内路径（从未 `initialize`）会拿不到 workspace 字段——那是行为变更，须单独批准。

---

## 3. 「拒绝不得触发 initial load」

### 3.1 代码路径（全部拒绝点 + 出口）

出口（**在任何生命周期路由、装配、广播之前**）：

```rust
// peri-acp/src/host/server_loop.rs:383-400（dispatch_request，早于 lifecycle_lock）
let load_initial_cursor = if method == "session/load" {
    let caps = connection.lock().await.caps();
    match history_page::classify(&params, connection_facts.history_pagination) {
        Ok(SessionLoadMode::HistoryPage(request)) => { /* 只读一页 */ ...; return; }
        Ok(SessionLoadMode::InitialLoad) => connection_facts.history_pagination,
        Err(error) => { let _ = diagnostics.send(transport.as_ref(), Err(error)).await; return; }  // ← 拒绝：直接返回
    }
} else { false };
```

`classify` 的拒绝点（`peri-acp/src/host/history_page.rs`）：

| # | 触发条件 | 位置 | 错误 |
| --- | --- | --- | --- |
| R1 | 无 `cursor` 字段但带 `limit` | `:127-131` | `-32602` reason=`limit_without_cursor` |
| R2 | 带 `cursor` 但本连接未协商分页 | `:138-143` | `-32601` reason=`pagination_not_negotiated` |
| R3 | `cursor` 非字符串 / 空串 | `:145-156` | `-32602` reason=`invalid_cursor` |
| R4 | 历史页带 `cwd` / `mcpServers` | `:157` → `reject_setup_params` `:369-379` | `-32602` reason=`setup_param_on_history_page` |
| R5 | 历史页带 `loadGeneration` | `:158-165` | `-32602` reason=`load_generation_on_history_page` |
| R6 | 缺 `sessionId` | `:166` → `session_id` `:359-367` | `-32602`（无 reason） |
| R7 | `cursor` 解不开 / 版本不支持 / 属于别的会话 | `:167-168` → `cursor_error` `:398-400` | `-32602` reason=`malformed_cursor`；`-32011` reason=`cross_session_cursor`（`dispatch/session_load.rs:144-163`） |
| R8 | `limit` 非整数或不在 `1..=100` | `:169` → `parse_limit` `:381-396` | `-32602` reason=`invalid_limit` |
| R9 | 会话不存在 | `handle_page` `:195-197` | `-32602` |
| R10 | 会话 closing | `handle_page` `:198-200` | `-32010` |
| R11 | `structure_version` 已过期（rewind/精确删除后） | `handle_page` `:201-204` | `-32012` reason=`expired_cursor` |

另：`classify` 只在 `ServerLoop` 路径被调用；测试用的无连接入口 `handle_request`（`requests.rs:36-58`）是 `#[cfg(test)]`，生产只有 `handle_request_for_connection` 一条路由 ⇒ 不存在「绕过 classify 的 `session/load`」。

### 3.2 现有断言盘点（逐条核对「拒绝时不得附带 initial load 的全量通知与响应字段」）

**wire 级（真实 `ServerLoop` + MPSC）只有 1 条**——`peri-acp/src/host/history_page_wire_test.rs:226 pagination_fields_from_an_unnegotiated_connection_are_rejected`（对应 R2）：

```rust
    let error = client.send_request("session/load", json!({ "sessionId": sid, "cursor": cursor }))
        .await.expect_err("未协商能力时携带 cursor 必须拒绝");
    assert_eq!(error.code, -32601, "能力门控与既有 rewind/oauth 同码");
    assert_eq!(sessions.lock().await.get(&sid).unwrap().history.len(), before,
        "拒绝不得触发一次初始 load");
    assert_eq!(archived.len(), 1);
    task.abort();
```

它**没有**做的事：
1. 没有排空/断言**通知队列**（`client.recv()` 从未调用；通知走独立 `incoming_rx`，不会被 `send_request` 消费，见 `transport/mpsc.rs:129-144`）⇒ 未断言「零条 `session/update` 全量回放」；
2. 没有断言**响应面**（实际拿到的是 `AcpError`，但 `error.data.reason` 无断言；`nextCursor`/`snapshotToken`/`modes`/`configOptions`/`_meta.periLoadGeneration`/`_meta.peri.sessionWorkspaceV1` 未逐项核对）；
3. 只覆盖 R2；R1/R3/R4/R5/R6/R7/R8/R9/R10/R11 **全部没有 wire 级用例**。

**纯函数级**（`peri-acp/src/host/history_page_test.rs`，8 条，`classify_page()` 在降级时 `panic!("request degraded into an initial load")`，`:28`）：断言了错误码，但**没有断言 `data.reason`**（`grep` 证据：`limit_without_cursor` / `pagination_not_negotiated` / `invalid_cursor` / `setup_param_on_history_page` / `load_generation_on_history_page` / `invalid_limit` 六个 reason 字面量在整个仓库**只**出现在 `peri-acp/src/host/history_page.rs` 实现里，零测试引用；`malformed_cursor` / `cross_session_cursor` / `expired_cursor` 只在 `dispatch/session_load.rs:144-163` 实现里）。

**直调下游函数级**：R11 在 `peri-acp/src/host/requests_rewind_cases_test.rs:590` 直调 `handle_page` 断言 `-32012`——不经 `ServerLoop`，故不覆盖「拒绝路径是否发出回放通知」。

实跑（全绿，但不能替代上面的盘点）：

```bash
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page
running 27 tests
test host::history_page::tests::absent_cursor_field_is_an_initial_load_and_limit_alone_is_rejected ... ok
test host::history_page::tests::a_cursor_without_the_negotiated_capability_is_rejected ... ok
test host::history_page::tests::malformed_cursor_forms_are_rejected_not_downgraded ... ok
test host::history_page::tests::a_cursor_from_another_session_is_its_own_error ... ok
test host::history_page::tests::a_history_page_request_must_not_carry_session_setup_parameters ... ok
test host::history_page::tests::limits_outside_one_to_hundred_and_non_integers_are_rejected ... ok
test host::requests::tests::history_page_wire_tests::pagination_fields_from_an_unnegotiated_connection_are_rejected ... ok
...
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 860 filtered out; finished in 1.24s
```

**判定：实现层成立（`Err` 分支 `return`，结构上不可能发出回放与装配）。断言层缺**：覆盖到 wire 的拒绝路径 1/11，且这 1 条不含「零通知 + 无初始 load 响应字段」的断言。

### 3.3 需要补的断言清单

统一前置：`history_page_wire_test.rs` 已有非阻塞排空助手 `s9c_drain_notifications`（`:1082-1106`，`biased` + `ready(())`，不需要等待窗口；服务端先发通知后发响应，响应到达即代表先前通知已入队）与真实 loop 夹具 `PageFixture` + `spawn_loop`（`:25-170`）。

1. **加强既有用例** `peri-acp/src/host/history_page_wire_test.rs::pagination_fields_from_an_unnegotiated_connection_are_rejected`
   - (a) 通知面：`assert!(s9c_drain_notifications(&client).await.is_empty(), "拒绝不得发出任何回放通知")`
   - (b) 响应面：`assert_eq!(error.data, Some(json!({ "reason": "pagination_not_negotiated" })))`
   - (c) 状态面：把 `history.len()` 单值断言换成 `(history.len(), history_payloads.len(), structure_version)` 三元组不变，另加 `environment.is_none()`
   - (d) 存储面：`cfg.session_resources.load_session_history(&sid).await.unwrap().len()` 不变
2. **新增** `peri-acp/src/host/history_page_wire_test.rs::a_rejected_page_request_never_runs_an_initial_load`（表驱动，连接协商 `pagination_caps()`；每行一个坏请求）
   - 行 1 `{sessionId, limit: 10}` → R1，`-32602` / `limit_without_cursor`
   - 行 2 `{sessionId, cursor: ""}` → R3，`-32602` / `invalid_cursor`
   - 行 3 `{sessionId, cursor: 42}` → R3，`-32602` / `invalid_cursor`
   - 行 4 `{sessionId: A, cursor: <来自 B 的游标>}` → R7，`-32011` / `cross_session_cursor`
   - 行 5 `{sessionId, cursor, cwd}` → R4，`-32602` / `setup_param_on_history_page`
   - 行 6 `{sessionId, cursor, loadGeneration: "gen"}` → R5，`-32602` / `load_generation_on_history_page`
   - 行 7 `{sessionId, cursor, limit: 0}` → R8，`-32602` / `invalid_limit`
   - 每行四条断言：① 错误码 + `data.reason` 精确匹配；② `s9c_drain_notifications(&client).await.is_empty()`；③ 会话三元组与 `environment.is_none()` 不变；④ 错误对象不得携带初始 load 字段（对 `error.data` 断言不含 `nextCursor`/`snapshotToken`/`modes`/`configOptions`/`periLoadGeneration`/`peri.sessionWorkspaceV1`——由于 `send_request` 返回 `AcpError` 结构，这条以「`data` 只有 `reason` 一个键」表达）
3. **新增** `peri-acp/src/host/history_page_wire_test.rs::an_expired_cursor_is_rejected_without_an_initial_load`（R11 的 wire 面）
   - 取页成功 → 走 `session/rewind` → 同一 cursor 重取，断言 `-32012` / `expired_cursor` + 上述 ②③
   - 现有 `requests_rewind_cases_test.rs:590` 直调 `handle_page` 的覆盖保留（判据不是同一层）
4. **（可选）** `a_page_request_for_a_missing_or_closing_session_is_rejected`：R9（`-32602`）/ R10（`-32010`）目前无任何用例，`handle_page` 的这两条分支只有静态证据。
5. **（可选，纯函数层）** 在 `history_page_test.rs` 的 8 条用例里补 `data.reason` 断言（`reject()` 的 reason 串是客户端分类依据，`requests_rewind_cases_test.rs:784-790` 已有同类先例）。

**边界说明**：`attach_initial_cursor` 失败导致「初始 load 已跑完但仍报错」是**反方向**的既有路径（`server_loop.rs:496-510`、用例 `a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor`），不属于本判据，不要混入上述断言。

---

## 4. 未验证项与局限

1. **未做运行时验证**：本轮遵守「只做侦察与判定，不改代码、不补测试」，因此 §3 的「拒绝不发出回放通知」是**代码路径静态证明**（`Err` 分支直接 `return`，早于 `lifecycle_lock`）＋现有用例未反驳，**没有**新增探针实测通知通道为零。§1.4、§3.3 的补齐方案也未执行。
2. **A14-c 的「实现成立」是静态推断**：依据是「fork 复制新 id 行 + flags 更新按 `thread_id` 限定 + 分页 SQL 按 `thread_id` 限定」，未跑端到端反例（例如手动在 fork 后对源执行 Full 再取页）。
3. **构造 B（进程级 workspace 门跨连接）未实测**：仓库生产接线每进程一个 `ConnectionContext`，多连接形状只能在测试夹具里搭；本轮未搭。构造 A（重复 `initialize`）同样只有静态推理，未跑探针。
4. **未覆盖的侧翼**：`_meta.peri.subagentFailure`、命令广播面（`session_restore.rs:633-643` 仍用会话级 caps）、live reminder 形状（`session/event_sink.rs:180-197`）三处属 D12 已记录的范围外同类形状，本轮未复核；`peri.sessionWorkspaceV1` 应作为**第四处同类形状**补记。
5. **A14 的「冻结子上下文」只覆盖了 `ancestor_len` 保护与 subagent fork 的 wire 形状**（`peri-agent/src/session/subagent/child_wire_capture_test.rs` 4 条），没有对「父 Full 之后冻结字节/子快照是否漂移」的断言。
6. **A14 的原文数字澄清**：`.recon/A-acceptance-report.md:80` 引用的「fork 装配测试 11 passed」对应 `-p peri-agent --lib -- fork`（本轮实测确认 11 passed），与 `child_wire_capture_test.rs`（7 个测试属性）不是同一口径；`.recon/A-acceptance-report-audit.md:177` 的质疑成立，但数字本身可复核。
7. **环境观察（非本任务改动）**：侦察期间工作树出现一处**不属于本任务**的未提交改动 `peri-tui/src/kit/acp_events/history_page.rs`（1 行 `AcpEventData::LocalUserBubble { text, .. }` 模式补全，mtime 09:09），同目录另有并行产出的 `.recon/S12-a05-recovery-scan.md`（09:21）——判断为并发的另一条 S12 侦察线所致。本轮未触碰该文件；本节全部测试命令只覆盖 `peri-acp` / `peri-agent` / `peri-resources` / `peri-acp-types` 四个 crate，均不依赖 `peri-tui`，证据不受影响。

## 5. 最小补齐方案（摘要）

1. **A14（决定 VERDICT 的那一项）**：在 `peri-acp/src/dispatch/session_fork_test.rs` 新增 1 条用例 `a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact` —— fork 后先记录子 `load_session_snapshot` 与子首页（`load_history_page`），再对**源**执行一次新的 Full，断言子快照逐条等价、子首页 items/`next_cursor` 等价，且源的新排除事实只出现在源。详细步骤见 §1.4。
2. **§2 的门控偏差**：本轮不需要改（不是 D12 同形）；若要统一到「准入按当前连接」，把 5 处 `effective_host_caps()` 改为读连接级 `ConnectionFacts`（§2.3），属行为变更，另开切片。
3. **§3 的断言缺口**：0 个实现改动；按 §3.3 加 2 条 wire 用例（表驱动 1 条 + 失效游标 1 条）并加强既有 1 条（补「零通知」「`data.reason`」「三元组不变」三类断言）。

---

VERDICT: A14-GAP
