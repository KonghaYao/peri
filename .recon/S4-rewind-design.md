# S4（rewind 有效视图重建）实现方案 —— REQ-T04

需求原文要点（逐条对照，勿自行扩大）：
1. fork / rewind 的候选查询、预览与执行**不得依赖 Host 持有完整历史**；可以按目标从存储读取所需归档原文，
   不适用「零归档正文读取」限制。
2. rewind 沿用既有保留／移除目标边界语义，**原子**建立该边界可继续执行的有效 payload 与 flags。
3. 必须「确定该边界仍成立的 Compact 结果，移除包含未来信息的摘要，恢复应重新有效的原文」，
   使热态及随后冷恢复使用同一有效视图。
4. **不能只删除后置摘要**，**也不能把保留前缀的所有 `excluded` 清零**（那会同时恢复旧摘要和它覆盖的原文）。
5. 允许补充识别 Compact 批次／摘要／覆盖范围所需的**最小元数据**，与 Compact 提交一致维护；
   不引入完整历史 Checkpoint。
6. 破坏历史范围的操作同时推进**结构版本**，使旧分页结果失效。

## 0. 现状（基线事实）

- 三条 rewind 路径都吃 Host 内存快照：
  - `peri-acp/src/host/requests/rewind.rs:20` `handle_rewind_candidates`（`s.history.clone()`，同步函数）；
  - `:41` `handle_rewind_preview`（`s.history.clone()` → `dispatch::rewind_preview`）；
  - `:104` `handle_rewind`（`s.history.clone()` 找目标 → `apply_canonical_rewind:53` 裁剪 `state.history_payloads`）。
- 存储侧已有可用的 durable 原语：`SessionResources::rewind_history(id, RewindBoundary)`
  （`peri-acp-types/src/session_resources.rs:824`，SQLite 实现 `sqlite_store/session_data.rs:859`），
  边界语义 `RemoveFrom` / `KeepThrough` 由纯函数 `store::history::rewind_keep_len` 定义。
- 全量读取入口已存在：`SessionResources::load_session_history`（`dispatch/session_load.rs:18` 是它的 ACP 包装）。
- Full 摘要消息形如 `BaseMessage::human(CONTINUATION_HINT + "\n\n" + summary)`（`compact_v2/full.rs:450`）。
- `commit_compaction_lifecycle` 唯一生产调用者是 Full（已核实）。

## 1. 关键不变量：被排除集合是 own 区的**前缀**

每次 Full 排除「当前所有未排除的 own 非 System 条目」，然后**在尾部追加**摘要（摘要本身不被排除）。
因此任意时刻恒成立：

> own 区（`entries[ancestor_len..]`）里，按顺序（跳过 System）存在一个分界 k：
> 前 k 个非 System 条目的 `excluded = true`，其后的非 System 条目 `excluded = false`。

推论（**本切片的核心算法，不需要任何新元数据**）：设 rewind 后保留的前缀为 P，
取 P 中「最长的 excluded 前缀」（跳过 System）长度 k：

- 若 P 在位置 k 存在一个**非 excluded** 的 own 非 System 条目 → 该条目就是仍然成立的那次 Full 的摘要，
  前缀 k 的排除**仍然有效**，其余位置上的 `excluded`（只可能出现在 k 之后，属于被移除摘要的批次）必须清零。
- 若 P 在位置 k 不存在这样的条目（摘要被本次 rewind 删掉，或 P 本身就是纯 excluded）→ 前缀的排除**失去依据**，
  P 中**所有** `excluded` 必须清零。

两条都满足需求第 3、4 点：既不「只删后置摘要」，也不「把前缀所有 excluded 清零」——
有存活摘要时前缀保持失效，无存活摘要时才是整体恢复。建议把该算法写成**领域纯函数**
（与 `store::history::apply_rewind` 同层，`peri-acp-types/src/store/history.rs`），
输入 `&[(MessageId, MessageFlags, bool /*is_system*/)]` + 保留长度，输出 `Vec<(MessageId, MessageFlags)>`。

**注意**：不要用 `CONTINUATION_HINT` 内容前缀去识别摘要。上述规则只依赖 `excluded` 的前缀结构与
「System 不参与排除」两条既有事实，属结构判定而非字符串约定（原则 3）。

## 2. 分步实现

### 2.1 读取（不依赖 Host 快照）

- `handle_rewind_candidates` / `handle_rewind_preview` / `handle_rewind` 的历史来源改为
  `load_session_history`（全量归档读取，REQ-T04 明确允许），不再读 `state.history`；
  `candidates` 当前是同步函数且签名吃 `&mut HashMap<...>`，需要改为 async 或把读取提到
  `requests.rs` 的调用点（保持 `-32603 session history load failed` 的错误形态与既有码一致）。
- `state.history` 仍用于**展示投影**（保持既有响应形状），但不得作为「目标是否存在」的判据。
- 目标定位统一用 `MessageId`（wire 是 uuid 字符串），在**存储返回的全量 payload**上定位。

### 2.2 原子边界建立

顺序（与 Compact 提交同样的「先 durable、后内存」）：

1. 在存储全量 payload 上按目标 id + 边界语义算出 `keep_len`（复用 `rewind_keep_len`，不要另写一份规则）。
2. 计算第 1 节的 flags 结果（保留前缀中需要清零的 id 集合）。
3. **一次** durable 提交：删除边界之后的行 + 清零集合的 flags。
   - 现状 `rewind_history` 只做删除；若没有一个 API 能同时表达删除与 flags 清理，
     应在 `SessionDataPort`/`SessionResources` 上扩展**一个**原子入口（如 `rewind_history_with_flags`），
     而不是让调用方分两次调用（分两次就破坏「原子建立」）。
   - SQLite 侧在**同一事务**内 `DELETE ... WHERE rowid >= ...` 与 `UPDATE messages SET excluded = 0 WHERE message_id IN (...)`；
     远端侧在同一 `read_batch`/批内同语义。**不新增列/表**（避免 DDL 审批）。
4. 内存更新：`state.history_payloads` / `state.history` 替换为「保留前缀 + 清零后的 flags」的有效视图；
   被删除行不得留在任何 Host 容器里。
5. 结构版本 +1（见 §2.4）。

失败即整体失败：durable 失败不得改内存（沿用 Compact 的提交契约）。

### 2.3 热态与冷恢复一致

- 冷恢复（`session/load` / 下一次 prompt）走存储，天然看到 §2.2 的结果；前提是 S4/F5 已让
  `compact_pipeline` 与 `load_active_snapshot` 使用有效视图。
- 热态（运行中的 `MessageTranscript`）：rewind 由 ACP Host 发起，agent 侧的 transcript 归下一次
  prompt 的 `restore_committed_context` / `compact_pipeline` 重建。需要核实并补一条**回归测试**：
  rewind 后同会话继续 prompt，模型请求里的历史等于冷恢复后的有效视图。
  若存在跨 turn 未被重建、直接复用旧 `active_payloads` 的路径，那才是必须修的破口。

### 2.4 结构版本

- 用途：使旧分页 cursor 失效（分页切片消费）。
- 建议形态：会话级（`SessionState`）单调计数器，rewind 成功即 +1；cursor 里带 `thread_id` + 版本，
  版本落后 → `CursorExpired`（fail closed，不降级为「重新从最新开始」）。
- **不新增列/表**。cursor 的锚点 `MessageId` 在存储里已不存在时也必须 fail closed：
  `sqlite` 的 rowid 在删除尾部行后可能被复用，靠 rowid 兜底（`COALESCE(..., MAX(rowid)+1)`）
  会把「锚点已消失」误判成「从最新开始」——分页实现里必须把这条路堵掉。
- Full Compact **不**推进结构版本（A09：追加消息 + 再次 Full 后旧页仍须等价）。

## 3. 验收映射

| 需求点 | 验证方式 |
| --- | --- |
| 候选/预览/执行不依赖 Host 快照 | 单测：构造 Host 只持有有效集合、存储持有归档的场景，候选仍列出旧目标；预览与执行成功 |
| 不能只删摘要 | 单测：Full 后 rewind 到被排除区内 → 保留前缀的 `excluded` 全部清零（原文恢复有效） |
| 不能全清 | 单测：两次 Full 后 rewind 到第二次 Full 之后 → 第一次 Full 覆盖的前缀保持 excluded，第二次覆盖的尾部恢复 |
| 原子性 | 注入 durable 失败 → 内存与存储都保持 rewind 前状态 |
| 热/冷一致 | 集成测试：rewind 后继续 prompt 的模型历史 == 重新 load 后的有效视图 |
| 结构版本 | 单测：rewind 后旧 cursor → `CursorExpired`（与分页切片联测） |

## 4. 边界与不做的事

- 不改 `RewindBoundary` 语义（KeepThrough/RemoveFrom 原样）。
- 不做历史迁移/兼容：`excluded` 之外没有需要转换的旧格式。
- 不引入 Compact 批次表或新列；第 1 节算法已足以判定覆盖范围。
- 文件回退（`revert_files`）与 `preview_fingerprint` 校验属于既有 rewind 执行体，不在本切片改动范围。
