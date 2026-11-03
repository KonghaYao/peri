# P1：`session/rewind-candidates` 全量读取历史并深复制正文

**状态**：已修复，待 PR 合并。
**实现状态**：修复已实现、本地验证并提交 PR（验证记录见文末）。
**优先级**：P1。
**类型**：性能/内存。
**创建日期**：2026-10-24。
**来源**：代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `e598d178`）。

## 确认证据

### 调用链（基线 `e598d178`）

1. `peri-acp/src/host/requests/rewind.rs:67`：`handle_rewind_candidates` 调
   `dispatch::load_canonical_session_payloads(&cfg.controller, session_id)`。
2. `peri-acp/src/dispatch/session_load.rs:179-188`：该读取转调
   `SessionResources::load_session_history`（本地/SQLite 均返回继承区 + 自有区**全部**
   payload）。
3. 本地实现 `peri-resources/src/sessions/sqlite_store/context.rs:128`：
   `SELECT message_id, content FROM messages WHERE thread_id = ?1 ORDER BY rowid`，
   **无 LIMIT、无 role 过滤**；远端 `remote/session_sql.rs:140-142`
   （`SELECT_MESSAGES_SQL`）同形。Ai/Tool 正文随行全部解码为 `PersistedPayload`。
4. `peri-acp/src/dispatch/rewind_candidates.rs:29-33`（基线）：
   `rewind_persisted_candidates` 把全部消息 `.cloned()` 成 owned
   `Vec<BaseMessage>`——这是逐条深复制，不是共享。
5. 同一文件 `:41-59`（基线）此时才只保留 `Human`、截断 200 字符、`.take(64)`；
   `Human` 之外的全部正文已经付过读取与复制成本。
6. 该 handler 在全局 `sessions` mutex 内执行：`peri-acp/src/host/server_loop.rs:606-616`
   （`let mut sessions = sessions.lock().await;` 后调 `handle_request_for_connection`）。

### 量化

1,000 条 × 128 KiB 工具结果时，存储读取产生的 `PersistedPayload` 与 `.cloned()` 后的
`BaseMessage` 两份正文同时存活，下界约 `1000 × 128 KiB × 2 = 250 MiB`；这些正文随后
全部被丢弃。TUI 只需要 `{id, preview}`（`peri-tui/src/kit/rewind_candidates.rs:14-45`）。

## 根因链

1. 领域层把「回退候选」当成「完整历史的一个视图」，先取全量再在内存里过滤。
2. 存储端口缺少「按 role 有界读取 + 按 id 取正文」的能力，handler 只能拿完整历史。
3. ACP 层为了适配纯函数签名，把全量消息深复制成 owned 集合，进一步放大峰值。
4. 候选查询在全局 `sessions` 锁内执行，峰值内存与读取时延同时阻塞其他会话请求。

## 影响面

- 用户影响：长会话（尤其包含大量工具结果）打开回退弹窗时出现高内存峰值、卡顿，
  低内存环境可能 OOM；与工具正文大小线性相关。
- 受影响模块：`peri-acp` rewind handler、`peri-resources` 会话存储读取、`peri-acp-types`
  会话资源契约。
- 可规避性：无用户侧规避手段（弹窗是唯一入口）。

## 修复方案

1. `peri-acp-types` 新增 `RewindCandidate { message_id, text }` 与共享资格规则
   `rewind_candidate` / `rewind_candidate_text`（Human 且剥离 legacy
   `<system-reminder>` 后仍有文本），并提供 `SessionResources::load_rewind_candidates(id, limit)`
   的默认回退实现（`load_session_history` + 过滤，仅供测试替身/未实现后端）。
2. `SessionDataPort` 新增 `load_rewind_candidates`，两个生产 adapter 实现有界读取：
   - SQLite（`session_data/rewind_candidates.rs`）分两阶段 + keyset 分批：
     阶段一 `SELECT message_id, rowid ... WHERE thread_id = ?1 AND role = 'user'
     AND (?2 IS NULL OR rowid < ?2) ORDER BY rowid DESC LIMIT ?3`（每批 128）；
     阶段二只对命中 id 取 `content`。纯 reminder 不占名额，累计到 `limit` 或耗尽即停。
     **不加 `excluded = 0`**：归档 user 消息仍是合法回退目标。
   - 远端（`remote/session_sql.rs` + `remote/session_read.rs`）同形语句与批次；
     自有区不足时再读 `threads.inherited_context` 并只投影 Human。
3. ACP handler 改调 `cfg.controller.sessions().load_rewind_candidates(...)`，删除
   `load_canonical_session_payloads` 调用与 `rewind_persisted_candidates` 的全量
   `.cloned()`。预览清洗（`strip_system_reminders`、200 字符）仍在 ACP 领域层
   （`rewind_candidate_response`）；`.take(64)` 保留为防御性上限。
4. **不新增数据库索引、不改表结构**（DDL 变更需用户批准）。
5. **不改变候选集合语义**：继承区 Human 消息仍可出现，仍排在自有条目之后；执行侧拒绝
   继承区目标的行为不变。

## 验收标准

- [x] `role != 'user'` 的行不进入解码；工具正文不产生候选。
- [x] 返回最多 64 条有效候选、最新在前；归档（`excluded = 1`）user 仍可见。
- [x] 尾部纯 reminder（含跨扫描批，>128 条）不挤掉有效候选。
- [x] preview 剥离注入块并截断 200 字符；顺序不变。
- [x] handler 不再调用 `load_session_history`（spy 断言 0 次）。
- [x] 继承区 Human 仍出现且排在自有条目之后。
- [x] 不修改数据库 schema；查询计划不出现 `messages` 全表扫描。
- [x] 相关测试与 `check` 通过（命令与结果见下）。

## 验证记录

环境：macOS，worktree `rewind-candidates-bounded`，基线 `e598d178`。

1. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- rewind`
   → `79 passed; 0 failed; 0 ignored; 991 filtered out`。
2. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- session`
   → `592 passed; 0 failed; 34 ignored; 12 filtered out`。
3. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- rewind_candidate`
   → `9 passed; 0 failed; 0 ignored; 629 filtered out`（新增 SQLite/远端/形状测试）。
4. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp -p peri-resources -p peri-acp-types`
   → `Finished dev profile ... in 7.66s`，exit 0。
5. `cargo fmt --all -- --check` → exit 0；`git diff --check` → exit 0。
6. 计划证据（`candidate_rows_sql_is_user_filtered_bounded_and_index_located --nocapture`）：

   ```text
   REWIND_CANDIDATE_ROWS_SQL: SELECT message_id, rowid
       FROM messages
       WHERE thread_id = ?1 AND role = 'user' AND (?2 IS NULL OR rowid < ?2)
       ORDER BY rowid DESC LIMIT ?3
   EXPLAIN QUERY PLAN: ["SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)"]
   ```

   结论：按 `idx_messages_thread_id` 定位线程区间，`role` 在行上过滤，无全表扫描；
   但索引不含 `role`，极端情况下（线程内大量非 user 行夹杂在尾部）仍需扫描线程内的
   非 user 行才能凑满一批。本 PR 按约束不新增索引，结论仅记录；若后续要消除这段
   线程内扫描，可评估 `messages(thread_id, role)` 索引（DDL 需用户批准）。

## 相关但本 PR 不处理

- **全局 `sessions` 锁范围**：候选读取仍在 `server_loop.rs:606-616` 的全局 mutex 内；
  本次只降低单次读取的内存与 I/O，锁架构另立任务。
- **继承候选不可执行**：候选集合包含继承区 Human，而执行侧拒绝继承区目标
  （`host/requests/rewind.rs` 的 own-region 校验）。语义不一致按既有口径保留，另立任务。
- **继承区快照整块解码**：自有区不足时读取 `threads.inherited_context` 会整体反序列化
  继承快照（含其中工具正文），再只投影 Human。继承快照在 fork 时冻结、规模有界，
  本次不改为增量解析。
- **`rewind-preview` 路径**：仍使用完整历史做文件影响计算（需要 Ai/Tool 正文），
  行为不变。
- **远端真实云端验证**：新增远端测试使用 `SqliteTransport` 离线夹具，未跑真实
  Turso Cloud（默认 `#[ignore]` 的云端用例不在本 PR 范围）。
