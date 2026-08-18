# 多个 peri TUI 后台轮询 SQLite 慢查询，导致 CPU / 磁盘资源风暴

**状态**：已实现轻量查询（待运行时验证）
**优先级**：高
**类型**：性能 / SQLite / 多进程放大
**创建日期**：2026-08-18
**来源**：本机运行时事故；用户报告“所有 peri 进程占用 99% 以上 CPU”
**最后核查**：2026-08-18 10:23（Asia/Shanghai）

## Problem Statement

同一用户同时运行多个 `peri` TUI 时，每个进程都会周期性查询共享的 `~/.peri/threads/threads.db`。TUI 的 service snapshot 每 2 秒调用一次 `ThreadStore::list_threads()`；SQLite 实现会为所有可见 thread 计算：

```sql
SELECT
    t.id,
    t.title,
    t.cwd,
    t.created_at,
    t.updated_at,
    t.message_count,
    (
        SELECT COALESCE(SUM(LENGTH(m.content)), 0)
        FROM messages m
        WHERE m.thread_id = t.id
    ) AS content_size,
    ...
FROM threads t
WHERE t.hidden = 0
ORDER BY t.updated_at DESC;
```

该查询使用 `idx_messages_thread_id`，并非对每个 thread 都无索引扫描整张 `messages` 表；但它仍会针对每个可见 thread 读取并遍历对应消息内容，以计算 `SUM(LENGTH(content))`。当前 TUI snapshot 随后只使用 `id`、`title`、`cwd`、`message_count` 和 `updated_at`，并不使用本次查询计算出的 `content_size`。

数据库规模较大时，单进程已经会产生秒级查询。多个 TUI 进程独立执行同一个 2 秒轮询后，昂贵读查询在共享数据库上叠加，造成 SQLite 页读取、页缓存、CPU 和磁盘 I/O 竞争，最终表现为多个进程同时或轮流占用接近一个 CPU core。

## Incident Summary

### 现场现象

- 观察到约 9 个 `peri` TUI 实例，分布在不同项目目录。
- 受影响版本包含 `agent-v3.8.5` 和 `agent-v3.8.6`，不是单一版本二进制损坏。
- 初始检查时多个 TUI 进程持续处于 `R+`，CPU 约为 97%–106%。
- 后续 CPU 会暂时降到低位，但又在不同进程间轮流升至 30%–70% 以上；因此“降下来了”只是查询周期错开后的低谷，不代表问题消失。
- 采样受影响进程时，热点位于 `sqlx-sqlite-worker`，调用链大量停留在 SQLite 页读取对应的 `pread`。
- 同期 `disk0` 系统级采样出现大量小 I/O；最高一次观察到约 2,412 tps / 104 MB/s。该数据是整盘指标，不能全部归因于 peri，但与进程采样和慢 SQL 时间窗口一致。

### 慢 SQL 时间窗口

共享 TUI 日志：

```text
/private/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/agent-tui.2026-08-18
```

截至最后核查：

- 第一条同类慢 SQL：2026-08-18 08:19:18（Asia/Shanghai）
- 最后一条已观察到的慢 SQL：2026-08-18 10:23:12
- 同类慢 SQL：2,143 次
- 平均耗时：约 2.38 秒
- 最慢耗时：约 3.91 秒
- 超过 2 秒：1,708 次（10:17 时的统计快照）
- SQLx slow threshold：1 秒

## Root Cause Assessment

### 已确认的主因：多进程并发昂贵读查询

因果链为：

```text
共享的大型 threads.db
  × 每个 TUI 独立的 2 秒 service snapshot
  × list_threads 对所有可见 thread 计算 content_size
  × TUI 在查询后才按 cwd / message_count 过滤
  × 多个 TUI 进程同时运行
  → 重复读取大量 message content
  → SQLite 页读取与页缓存竞争
  → CPU / 磁盘 I/O 风暴
```

关键代码事实：

- `peri-tui/src/kit/service_snapshot.rs`
  - `spawn_service_snapshot()` 每 2 秒 tick。
  - `tick_once()` 每 2 秒调用 `src.thread_store.list_threads()`。
  - 查询完成后才在 Rust 中过滤 `hidden`、`message_count > 0` 和 `cwd == src.cwd`。
  - 构造 `ThreadSummary` 时不读取 `ThreadMeta::content_size`。
- `peri-resources/src/sessions/sqlite_store.rs`
  - `THREAD_META_COLUMNS` 包含相关子查询 `SUM(LENGTH(m.content))`。
  - `SqliteThreadStore::list_threads()` 对全部 `hidden = 0` thread 执行该投影。
  - 每个进程创建自己的 `SqlitePool`，`max_connections(5)`；9 个进程理论上最多可持有 45 个连接，但本次事故并不表示这些连接始终全部活跃。
- SQLite `EXPLAIN QUERY PLAN`：
  - 扫描 `threads`；
  - 对每个结果执行 correlated scalar subquery；
  - 子查询通过 `idx_messages_thread_id` 查找 messages；
  - `ORDER BY updated_at` 使用临时 B-tree。

### “并发写太高导致锁争抢”不是当前主因

“peri 进程太多”判断了一半：**进程数量确实是严重放大器，但现有证据不支持主要问题是并发写锁争抢。**

| 并发写争抢的预期证据 | 本次实际观察 |
| --- | --- |
| `SQLITE_BUSY`、`database is locked` 或 busy timeout | 未发现 |
| 写入失败、事务回滚、WAL checkpoint 错误 | 未发现 |
| 事故窗口持续高速增加 messages / WAL | 点查仅见低速或零增长；没有证明事故期间存在高写入吞吐 |
| 热点集中在写入、`pwrite`、`fsync` 或锁等待 | 进程采样热点是 `sqlx-sqlite-worker` 的 `pread` |
| 慢语句以 INSERT / UPDATE / DELETE 为主 | 2,143 条明确慢语句均为 `list_threads()` 的 SELECT |

写入仍可能通过 WAL、checkpoint 和缓存失效增加额外成本，但它至多是次要影响，当前不能认定为事故主因。即使没有业务写入，多个空闲 TUI 仍会保持 2 秒轮询，因而具备持续触发同类竞争的机制；空闲多实例的资源曲线仍需通过受控复现实验量化。

更准确的结论是：

> 单进程昂贵轮询查询是潜在缺陷；多进程数量将其乘法放大，最终越过资源风暴阈值。问题不是“用户不应多开 peri”，而是空闲 TUI 不应周期性读取并聚合与当前视图无关的大量消息内容。

## Impact

### 已确认影响

- 本机多个 peri TUI 长时间占用接近一个 CPU core。
- SQLite 查询和系统磁盘 I/O 显著升高。
- 会话列表刷新耗时 1–4 秒，可能连带造成输入、渲染和事件显示延迟。
- 多项目同时运行时相互影响，因为所有进程共享 `~/.peri/threads/threads.db`。
- 问题在 CPU 暂时下降后仍可周期性复发。

### 未发现的影响

截至事故后只读健康检查：

- `PRAGMA quick_check(1)` 返回 `ok`。
- 9,408 个 threads、417,493 条 messages。
- 0 条孤儿 message。
- 0 个 `threads.message_count` 不一致。
- 0 个重复 `message_id`。
- 最近 2,000 条 message content 均为有效 JSON，且没有空 content。
- 日志未发现 `SQLITE_BUSY`、`database is locked`、`database disk image is malformed`、disk I/O error、corrupt 或 panic。
- 2026-08-18 没有发现新的 peri crash report。
- 8 秒静态观察窗口内，主库、WAL 和 message 数均未增长。

因此目前没有数据库损坏、明显会话丢失或持久化失败的证据。由于没有事故前逐条快照，不能据此绝对证明不存在单条未写入消息。

### 需要独立跟进但尚未证明有关联

事故日志另有约 1,797 条：

```text
subagent tool start NOT ROUTED to SubAgentGroup
```

这更像 subagent 工具事件没有显示到预期 TUI 分组，不代表工具未执行。它与慢 SQL 时间窗口重叠，但当前没有证据证明二者存在因果关系，不应混入本 issue 的根因修复。

## Data Size Context

事故后采样：

- `threads.db`：约 1.86 GB
- `threads.db-wal`：约 11 MB
- `~/.peri/threads`（含 backup）：约 2.1 GB
- message content 总量：约 1.24 GB
- 可见 threads：约 3,992
- hidden threads：约 5,416
- 磁盘剩余空间：约 388 GB

注意：`threads.db` 文件实际创建于 2026-06-05；2026-08-15 是当前 WAL 文件的创建时间。不能将 1.86 GB 误解为三天内全部产生。

数据库较大是触发查询成本的条件，但数据量本身不是唯一根因。合理实现应允许保留历史会话，同时避免空闲状态反复读取 message content。

## Remediation Plan

### P0：移除 TUI snapshot 的无用内容聚合

优先为 service snapshot 提供轻量查询，只返回当前 UI 真正需要的字段：

- `id`
- `title`
- `cwd`
- `message_count`
- `updated_at`

并在 SQL 层完成：

- `hidden = 0`
- `message_count > 0`
- `cwd = ?`

不要为了构造 TUI `ThreadSummary` 计算 `content_size`，也不要先加载所有项目的 thread 后再由 Rust 过滤。

实现时需先确认 `ThreadStore::list_threads()` 的既有 `content_size` 契约。若其他调用者依赖该字段，应该增加窄的 summary/list API，而不是在全局 `list_threads()` 中静默返回错误的 `content_size = 0`。

### P0：降低或消除固定轮询

- 首选：thread 创建、标题变化、message append、删除和会话切换时显式失效 snapshot，由事件触发刷新。
- 保底：将无变化时的 thread scan 从 2 秒提高到较低频率，例如 30 秒。
- 保持 `MissedTickBehavior::Delay`，避免慢查询结束后追赶漏掉的 tick。

### P1：增加性能回归测试

构造接近现场规模的数据集，并验证：

1. TUI snapshot 查询不读取 `messages.content`。
2. 当前 cwd 过滤发生在 SQL 层。
3. 1、5、10 个空闲 TUI 并行时，CPU 和磁盘读取不会随进程数线性放大到不可用。
4. 连续运行至少 5 分钟不产生该 `list_threads()` slow statement。
5. 会话列表仍能在定义的刷新窗口内看到创建、重命名、追加消息和删除结果。

### P1：增加可观测性

- 为 thread snapshot 记录结构化耗时、返回行数和触发原因，但不得记录消息内容。
- 区分 read query、write transaction、busy/lock wait 和 WAL checkpoint 指标。
- 对同类慢查询做限频日志，避免性能事故期间日志本身进一步放大 I/O。

### P2：数据库体积治理

这是独立于 P0 的长期项：

- 统计 visible / hidden / subagent thread 的消息和内容占比。
- 评估 hidden subagent 历史的保留、归档和清理策略。
- 如果产品确实需要频繁展示 `content_size`，将其作为 `threads` 的增量维护字段，避免查询时重新遍历 message content。

数据库清理不能替代查询修复；只清库会暂时降低成本，数据重新增长后仍会复发。

## Implementation Record

2026-08-18 已完成 P0 轻量查询实现，尚未进行多实例运行时关闭验证：

- `peri-acp-types` 新增 `ThreadListEntry`，并在 `ThreadStore` 增加 `list_thread_entries(cwd)`；默认实现保留测试替身兼容。
- `SqliteThreadStore` 使用五字段查询，并在 SQL 层完成 `cwd`、`hidden = 0`、`message_count > 0` 过滤，不访问 `messages` 表。
- `FilesystemThreadStore` 从 index 直接生成轻量列表，不读取 message 文件 metadata。
- TUI `service_snapshot` 已切换到轻量 API；2 秒刷新频率和完整 `list_threads()` 的 `content_size` 契约保持不变。
- 未增加数据库索引或 schema migration；测试库基准表明无索引查询已经从约 545ms 降至 1–3ms。
- 新增 SQLite 过滤/排序测试和 TUI 当前 cwd 列表行为测试；`peri-resources` 全部 48 个 lib tests、`peri-tui` 全部 1,282 个 lib tests、相关 doc tests 与目标 clippy 均通过。

## Acceptance Criteria

- [ ] 空闲 TUI 的周期快照路径不执行 `SUM(LENGTH(messages.content))`。
- [ ] 当前 cwd、hidden 和空 thread 过滤在 SQL 层完成。
- [ ] 多个 TUI 共享同一数据库时，不出现持续接近 100% CPU 的空闲进程。
- [ ] 现场规模数据库上连续 5 分钟无同类 SQLx slow query。
- [ ] 会话列表创建、重命名、消息追加、删除和切换行为保持正确。
- [ ] SQLite 健康检查、现有 thread store tests 和 TUI service snapshot tests 通过。
- [ ] 不改变消息持久化、context cache、compaction 和 session replay 语义。

## Verification Commands

```bash
cargo test -p peri-resources --lib
cargo test -p peri-tui --lib
cargo clippy -p peri-resources -p peri-tui --all-targets -- -D warnings
git diff --check
```

运行时验证应另开多个 TUI 实例，并联合观察：

```bash
ps -p "$(pgrep -x peri | paste -sd, -)" -o pid,%cpu,%mem,state,etime,time,command
iostat -c 5 -w 2 -d disk0
```

## Non-goals

- 本 issue 不处理 `subagent tool start NOT ROUTED` 的事件路由问题。
- 本 issue 不以删除用户历史会话作为首要修复。
- 本 issue 不禁止用户同时运行多个 peri 实例。
- 本 issue 不在缺乏证据时将问题归因于 SQLite 并发写锁。
