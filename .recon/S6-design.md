# S6 详细实现方案：ACP 历史分页（REQ-H01~H04）

前置：S1-S4 完成（有效范围读取 + 热态收缩）。基线行号取自 `7f0de56e8`。

## 1. 契约（peri-acp-types）

```rust
/// 浏览快照身份：同一次浏览期间固定（REQ-H02）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshotToken(String);   // 服务端生成的不透明值

/// 历史分页游标。客户端视为不透明字符串（序列化后）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub thread_id: ThreadId,
    /// 下一读取位置（更早方向）。
    pub anchor: MessageId,
    /// 浏览快照身份 + 投影/字节预算版本。
    pub snapshot_token: String,
    /// 结构版本：rewind / 删除等破坏范围的操作递增，使旧 cursor 失效。
    pub structure_version: u64,
}

#[derive(Clone, Debug)]
pub struct HistoryPageItem {
    pub id: MessageId,
    /// 稳定逻辑展示位置（排序键），用于前端区间合并。
    pub position: u64,
    pub payload: PersistedPayload,
    pub oversized: bool,
}

#[derive(Clone, Debug)]
pub struct HistoryPage {
    /// 页内历史正序（更早 → 更晚）。
    pub items: Vec<HistoryPageItem>,
    /// None = 没有更早的可展示内容。
    pub next_cursor: Option<String>,
}
```

trait 方法（默认实现返回 `Unsupported`，未实现者 fail closed）：

```rust
async fn load_history_page(
    &self,
    id: &ThreadId,
    anchor: Option<&MessageId>,   // None = 首屏边界（从有效视图与压缩覆盖边界派生）
    limit: usize,
    byte_budget: u32,
) -> SessionResourceResult<HistoryPage>;
```

错误种类（`SessionResourceErrorKind` 新增，可区分，不得用空页掩盖）：

- `CursorInvalid`（损坏/类型错误）
- `CursorCrossSession`（thread_id 不匹配）
- `CursorExpired`（structure_version 落后于当前）

## 2. 存储实现（peri-resources）

- SQLite keyset（正向遍历用 `rowid <`，返回后反转）：
  ```sql
  SELECT message_id, content FROM messages
  WHERE thread_id = ?1
    AND rowid < COALESCE((SELECT rowid FROM messages WHERE message_id = ?2),
                         (SELECT MAX(rowid) + 1 FROM messages WHERE thread_id = ?1))
  ORDER BY rowid DESC LIMIT ?3
  ```
  - **不过滤 excluded**：归档区正是被 Full 压缩的原文（REQ-H01 第 4 点）。
  - 过滤不可展示条目（System、非展示受众 reminder）时**位置仍须推进**（REQ-H01 第 7 点）：读出后过滤，用未过滤行数判断是否还有更早内容，只有确实没有才 `next_cursor = None`。
  - 前端方向：首屏是最新部分；分页向"更早"读取，页内最终按正序返回。
- Remote：新静态 SQL 常量（`session_sql.rs`），与 SQLite 同语义；沿用 fail-closed 的"回复不完整即错误"。
- `structure_version`：来源为 threads 表的既有版本事实；若无，可由 Compact 提交与 rewind 维护的最小元数据承载（**不新增表**，复用 `threads` 行的更新时间或补充最小列 — 若需新列须走 DDL 审批）。

## 3. ACP 分流（peri-acp）

在 `dispatch_request`（`server_loop.rs:367`）中、生命周期锁分支（`:376`）之前解析模式：

```rust
enum SessionLoadMode {
    Initial,                                   // 无 cursor 字段
    HistoryPage { cursor: String, limit: u8 }, // cursor 存在（合法非空）
}
```

- 解析在能力检查之后：未协商分页能力的连接携带 `cursor`/`limit` → 明确拒绝（`-32602` 或专用码），**不降级为 Initial**（REQ §5.2 参数规则唯一）。
- `Initial` 走现有路径（但初始 cursor 需在响应中返回，从有效视图最早 own 条目/压缩覆盖边界派生，不取首屏首条 ID）。
- `HistoryPage` 复用 `session_io` 的「短锁 prepare + 锁外 handle」模式（`server_loop.rs:440-454`）：
  - 短锁内只读取权限与会话存在性（不绑定、不 attach、不广播）；
  - 锁外执行 `load_history_page`（不持全局 sessions 锁跨 I/O）；
  - 不取执行生命周期锁 → 可与进行中的 turn 并行。
- 初始回放：携带 `loadGeneration`（复用现有请求隔离标识），客户端暂存至回放完整后一次安装；回放失败必须使 load 失败（现状 `session_restore.rs:525` 的 warn-and-continue 必须改）。

## 4. 能力协商（连接级）

- `PeriCaps` 新增 `history_pagination: bool`（`peri.historyPagination`），四处同步：struct 字段、`from_client_meta`、`to_agent_meta`、`all_enabled`。
- 现状问题：`pending_caps` 是进程级单例（`session/caps.rs:22`），多连接互相覆盖；`caps_registry` 是 per-session，重连会话会继承旧连接协商值（违反 REQ §5.3）。
- 改造：`ConnectionContext`（`peri-acp/src/host/connection.rs`）持有本连接协商的 `PeriCaps`；`handle_load` 的分页分流使用**当前连接**的 caps，不读 `caps_registry` 的会话旧值。

## 5. 前端（S7）

- TUI：向上滚动靠近向前边界触发 cursor load；按稳定逻辑位置区间合并（不整段替换、不全局头插）；翻页 loading 与执行状态分离；旧页不启动工具动画/审批/Todo。
- SDK：`chat-projection.ts` 的 `replaceHistory`（全量清空重建）改为按 id 合并；相同 cursor 并发合并 + 重试去重；Compact 与 rewind 分流。
- TUI 删除「手动 Compact 后强制 `session/load`」依赖（`acp_events/turn.rs`），改用局部更新。

## 6. 验收映射

| 验收 | 覆盖点 |
| --- | --- |
| A06 | 每页 ≤50 完整封装、无大 OFFSET（keyset）、页内正序、连续非展示条目不假终点 |
| A07 | oversize 独占一页、cursor 前进、有界读取（流式/长度预检） |
| A08 | 同 cursor 同 limit 重试等价；不同 limit 只改页长 |
| A09 | 追加消息 + 再次 Full 后旧页等价（不按新 flags 改成员） |
| A21 | HistoryPage 不取执行锁、不持全局锁跨 I/O |
| A25 | 未协商/非法参数明确拒绝，不降级 |
