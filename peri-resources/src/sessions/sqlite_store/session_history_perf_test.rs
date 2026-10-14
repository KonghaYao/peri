//! 有界读取的性能证据（需求 §9：A04 / A05 / A06 / A07）。
//!
//! 除末尾的 `history_segment_statements_are_index_located`（只看计划、不建夹具，因此默认
//! 运行）外，全部用例 `#[ignore]`：默认测试运行既不生成大夹具，也不计入时长。命令、原始
//! 输出与数字见 `.recon/perf-evidence.md`：
//!
//! ```text
//! ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
//!     -- history_perf_tests --ignored --nocapture
//! ```
//!
//! 环境变量：`PERI_PERF_ROWS`（默认 `10000,100000,1000000`）、`PERI_PERF_ITERS`（默认 20）、
//! `PERI_PERF_PAGES`（默认 20）。
//!
//! 测量口径（逐项标注在输出与报告里，不发明毫秒承诺）：
//!
//! 1. 夹具复用生产 schema 与 [`serialize_persisted_payload`] 编码：会话由门面
//!    `create_session` 建立，历史行用**全绑定**的批量 INSERT 写进同一张 `messages` 表
//!    （生产 `append_history` 就是这张表的逐条写入；1M 行若逐条走生产 API，测量时间会花在
//!    夹具上）。
//! 2. 归档段的行写 `excluded = 1`、有效段写 `excluded = 0`——生产里由 Compact 把旧前缀置为
//!    `excluded`，夹具直接落成同一个行状态。
//! 3. 计划：对**生产语句原文**加 `EXPLAIN QUERY PLAN` 前缀（语句文本从实现的常量取，不复制
//!    SQL），断言里禁止 `messages` 全表扫描与任何全历史聚合。
//! 4. 扫描行数：`SELECT COUNT(*) ... WHERE thread_id = ?1` 行数探针。计划里对 `messages` 的
//!    访问是 `idx_messages_thread_id` 的范围搜索，而该索引对 `excluded`/`content` 不覆盖，
//!    因此范围行数就是这一步允许触达的行数上界（报告写明用的是哪一种方法）。
//! 5. 字节：只报告实测值——解码回来的 payload 正文字节、`dbstat`/`page_count` 的落库占用。
//!    页级实际读盘字节在 sqlx 公开面上不可观测（本任务不得新增依赖），报告里标为未测量。
//! 6. 查询次数：按代码路径逐条列出（`file:line`），不冒充执行期计数。
//! 7. 冷/热：冷 = 每次测量前新开一个部署句柄（新连接池 ⇒ SQLite 页缓存为空），热 = 同一个
//!    句柄上的连续读取。两者都**不**刷新 OS 页缓存（无权限），报告里同样写明。

use super::helpers::{BINDING_ROW_STATE_SQL, FROZEN_CONTEXT_SQL, THREAD_EXISTS_SQL};
use super::history::{
    ACTIVE_ENTRY_POSITIONS_SQL, HISTORY_PAGE_BODY_SQL, HISTORY_PAGE_BOUNDS_SQL,
    HISTORY_PAGE_ROWS_SQL, HISTORY_SEGMENT_ANCHOR_SQL, HISTORY_SEGMENT_HEAD_SQL,
    HISTORY_SEGMENT_NEXT_SQL, HISTORY_SEGMENT_TAIL_SQL,
};
use crate::sessions::sqlite_store::compaction::ACTIVE_FLAGS_SQL;
use crate::sessions::sqlite_store::context::{
    meta_by_id_statement, ACTIVE_PAYLOADS_SQL, INHERITED_CONTEXT_SQL, PARENT_THREAD_ID_SQL,
};
use peri_acp_types::history_page::HistoryCursor;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const PERF_SESSION: &str = "perf-history";

/// 有效集合：条数与正文大小固定（需求 A04 只固定**有效**条数与正文大小）。
const LIVE_ENTRIES: usize = 3;
const LIVE_BODY_BYTES: usize = 64 * 1024;

/// 归档正文默认大小：归档段行数与正文大小是夹具参数，需求未固定。
const ARCHIVED_BODY_BYTES: usize = 1024;

/// 页参数 = ACP 侧配置值（`peri-acp/src/host/history_page.rs` 的 `DEFAULT_PAGE_LIMIT` 与
/// `PAGE_BYTE_BUDGET`）：peri-resources 不能依赖 peri-acp，这里按同一数值传入并记录实际值。
const PAGE_LIMIT: usize = 50;
const PAGE_BYTE_BUDGET: usize = 512 * 1024;

const DEFAULT_ROWS: &str = "10000,100000,1000000";
const DEFAULT_ITERS: usize = 20;
const DEFAULT_PAGES: usize = 20;
const DEFAULT_TRIPWIRE_ROWS: usize = 10_000;

/// 单条 INSERT 的行数：占位符按行数生成（每行 6 个参数），值全部绑定。
const INSERT_BATCH_ROWS: usize = 500;

/// 归档正文的不可解码替代：初始恢复一旦读到归档正文，反序列化就会在这里失败。
const UNDECODABLE_ARCHIVED_BODY: &str = "excluded-archive-body-not-a-persisted-payload";

/// 归档正文形态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchivedBody {
    /// 真实可解码载荷（分页读取要读它）。
    Payload,
    /// 不可解码正文：把「零归档正文读取」变成会响的探针（见
    /// [`history_perf_excluded_bodies_are_never_decoded`]）。
    Undecodable,
}

// ── 夹具 ──────────────────────────────────────────────────────────────────────

/// 一行夹具数据；`excluded`/`truncated` 与生产落库的口径一致。
struct FixtureRow {
    message_id: String,
    content: String,
    excluded: bool,
    truncated: bool,
}

/// 夹具：一个会话 + 固定有效段 + 增长归档段。
struct Fixture {
    /// 只做 RAII：目录随夹具存活（库文件与 WAL 侧车文件都在里面）。
    _directory: TempDir,
    db_path: PathBuf,
    thread: String,
    /// 有效段的 id，按写入顺序（= 存储序）。
    live_ids: Vec<MessageId>,
    /// 实测：写入的有效正文编码字节合计。
    live_bytes: usize,
    /// 实测：写入的归档正文编码字节合计（Rust 侧精确计数）。
    archived_bytes: usize,
    archived_rows: usize,
}

impl Fixture {
    fn messages(&self) -> i64 {
        (self.archived_rows + LIVE_ENTRIES) as i64
    }
}

fn env_rows() -> Vec<usize> {
    let raw = std::env::var("PERI_PERF_ROWS").unwrap_or_else(|_| DEFAULT_ROWS.to_owned());
    raw.split(',')
        .filter_map(|value| value.trim().parse::<usize>().ok())
        .collect()
}

fn env_usize(key: &str, fallback: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn archived_text(index: usize, body_bytes: usize) -> String {
    format!("archived #{index} {}", "a".repeat(body_bytes))
}

fn live_text(index: usize, body_bytes: usize) -> String {
    format!("live #{index} {}", "L".repeat(body_bytes))
}

fn fixture_session(id: &str, workspace: &ResolvedWorkspace) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-10-13T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: FrozenSnapshotBytes::new(format!(r#"{{"version":1,"marker":"{id}"}}"#)),
    }
}

/// 部署装配点做的事（`Resources::open_with` + `into_session_resources`）。
///
/// 冷测量每次调用都会新开一个连接池（SQLite 页缓存为空）；池的打开不计入计时。
///
/// 夹具阶段先经门面建会话，那个池是**后台异步关闭**的：紧接着再开一个池可能撞上
/// 尚未释放的库锁（`database is locked`）。这里退避重试，重试只在打开失败时发生、
/// 且在任何计时窗口之外；超过上限仍然把最后一个错误抛出来。
async fn deployment(db_path: &Path) -> Arc<dyn SessionResources> {
    const ATTEMPTS: usize = 20;
    for attempt in 1..=ATTEMPTS {
        match crate::Resources::open_with(Some(db_path.to_path_buf())).await {
            Ok(resources) => {
                if attempt > 1 {
                    println!("  （打开会话库第 {attempt} 次尝试才成功：上一个池仍在异步关闭）");
                }
                return resources.into_session_resources();
            }
            Err(error) if attempt < ATTEMPTS => {
                println!("  （打开会话库第 {attempt} 次失败：{error}；250ms 后重试）");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => panic!("打开会话库: {error}"),
        }
    }
    unreachable!("重试循环在上限处 panic")
}

/// 与生产 store 同一组连接选项（`sqlite_store/connection.rs`：WAL + `synchronous=NORMAL`
/// + 外键），只用于夹具写入与计划/探针读取。
async fn fixture_pool(db_path: &Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("foreign_keys", "ON");
    SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("打开夹具连接池")
}

/// 批量写入历史行：同一张 `messages` 表、同一套列约束、同一份序列化编码；占位符按行数生成，
/// 值全部走绑定参数（语句里不出现任何数据文本）。
async fn insert_rows(pool: &SqlitePool, thread_id: &str, rows: &[FixtureRow]) {
    for chunk in rows.chunks(INSERT_BATCH_ROWS) {
        let mut statement = String::from(
            "INSERT INTO messages (message_id, thread_id, role, content, excluded, truncated) VALUES ",
        );
        for index in 0..chunk.len() {
            if index > 0 {
                statement.push(',');
            }
            let base = index * 6;
            statement.push_str(&format!(
                "(?{},?{},?{},?{},?{},?{})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
                base + 5,
                base + 6
            ));
        }
        let mut query = sqlx::query(sqlx::AssertSqlSafe(statement));
        for row in chunk {
            query = query
                .bind(row.message_id.as_str())
                .bind(thread_id)
                .bind("user")
                .bind(row.content.as_str())
                .bind(row.excluded)
                .bind(row.truncated);
        }
        query.execute(pool).await.expect("写入夹具历史行");
    }
}

/// 建夹具：门面建会话，然后按「归档段在前、有效段在后」写同一张 `messages` 表。
async fn build_fixture(
    archived_rows: usize,
    archived_body: ArchivedBody,
    archived_body_bytes: usize,
) -> Fixture {
    let directory = tempfile::tempdir().expect("临时目录");
    let db_path = directory.path().join("threads.db");
    let facade = deployment(&db_path).await;
    let workspace = facade
        .resolve_workspace(directory.path())
        .await
        .expect("解析工作区");
    facade
        .create_session(&fixture_session(PERF_SESSION, &workspace))
        .await
        .expect("建立会话");

    // 门面池是后台异步关闭的：先放掉它、等一拍，再开夹具池，免得库锁还没释放。
    // 会话行已经在库里，后续写历史行不再需要门面。
    drop(facade);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pool = fixture_pool(&db_path).await;
    let mut archived_bytes = 0usize;
    let mut generate_cost = Duration::ZERO;
    let mut insert_cost = Duration::ZERO;
    for start in (0..archived_rows).step_by(INSERT_BATCH_ROWS) {
        let end = usize::min(start + INSERT_BATCH_ROWS, archived_rows);
        let generate_started = Instant::now();
        let rows = (start..end)
            .map(|index| {
                let (message_id, content) = match archived_body {
                    ArchivedBody::Payload => {
                        let payload = PersistedPayload::Message(BaseMessage::human(archived_text(
                            index,
                            archived_body_bytes,
                        )));
                        // 行 id 取载荷自己的 id：生产 `append_history` 就是这么写的，
                        // 存储层读取时会复核「行内 ID 与主键一致」。
                        let message_id = payload.id().as_uuid().to_string();
                        let content =
                            serialize_persisted_payload(&payload).expect("归档载荷可序列化");
                        (message_id, content)
                    }
                    ArchivedBody::Undecodable => (
                        MessageId::new().as_uuid().to_string(),
                        UNDECODABLE_ARCHIVED_BODY.to_owned(),
                    ),
                };
                archived_bytes += content.len();
                FixtureRow {
                    message_id,
                    content,
                    excluded: true,
                    truncated: false,
                }
            })
            .collect::<Vec<_>>();
        generate_cost += generate_started.elapsed();
        let insert_started = Instant::now();
        insert_rows(&pool, PERF_SESSION, &rows).await;
        insert_cost += insert_started.elapsed();
    }
    println!(
        "  夹具分相：生成（uuid + 正文 + 序列化）{:.2}s，批量 INSERT {:.2}s（{INSERT_BATCH_ROWS} 行/语句）",
        generate_cost.as_secs_f64(),
        insert_cost.as_secs_f64(),
    );

    let mut live_ids = Vec::with_capacity(LIVE_ENTRIES);
    let mut live_rows = Vec::with_capacity(LIVE_ENTRIES);
    let mut live_bytes = 0usize;
    for index in 0..LIVE_ENTRIES {
        let payload =
            PersistedPayload::Message(BaseMessage::human(live_text(index, LIVE_BODY_BYTES)));
        let id = payload.id();
        let content = serialize_persisted_payload(&payload).expect("有效载荷可序列化");
        live_bytes += content.len();
        live_ids.push(id);
        live_rows.push(FixtureRow {
            message_id: id.as_uuid().to_string(),
            content,
            excluded: false,
            // 第一条带投影标记：让「活跃 flags」读取有非默认标记可返回。
            truncated: index == 0,
        });
    }
    insert_rows(&pool, PERF_SESSION, &live_rows).await;

    // 元数据与生产写入同步：`message_count` 是事实字段，夹具不留下 0。
    sqlx::query("UPDATE threads SET message_count = ?1, updated_at = ?2 WHERE id = ?3")
        .bind((archived_rows + LIVE_ENTRIES) as i64)
        .bind(peri_time::now_utc_rfc3339())
        .bind(PERF_SESSION)
        .execute(&pool)
        .await
        .expect("同步 message_count");
    pool.close().await;

    Fixture {
        _directory: directory,
        db_path,
        thread: PERF_SESSION.to_owned(),
        live_ids,
        live_bytes,
        archived_bytes,
        archived_rows,
    }
}

// ── 计划证据 ──────────────────────────────────────────────────────────────────

/// 计划参数：显式枚举，绑定形状与生产调用一致（`EXPLAIN` 不执行，值不参与计划）。
enum Param<'a> {
    Text(&'a str),
    OptionalText(Option<&'a str>),
    Integer(i64),
}

/// 一段计划 + 它的语句原文：报告里逐条抄录同一份文本。
struct PlanEvidence {
    label: &'static str,
    statement: String,
    plan: Vec<String>,
}

/// 对生产语句原文取计划：只加 `EXPLAIN QUERY PLAN` 前缀，不改语句一个字符。
async fn explain_plan(pool: &SqlitePool, sql: &str, params: &[Param<'_>]) -> Vec<String> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")));
    for param in params {
        query = match param {
            Param::Text(value) => query.bind(*value),
            Param::OptionalText(value) => query.bind(*value),
            Param::Integer(value) => query.bind(*value),
        };
    }
    let rows = query.fetch_all(pool).await.expect("EXPLAIN QUERY PLAN");
    rows.iter()
        .map(|row| {
            let id: i64 = row.try_get(0).unwrap_or(-1);
            let detail: String = row.try_get(3).unwrap_or_default();
            format!("#{id} {detail}")
        })
        .collect()
}

/// 初始恢复（门面 `load_active_snapshot`）的语句链与计划，顺序 = `session_data.rs` 里的执行顺序。
async fn restore_chain_plans(pool: &SqlitePool, thread: &str) -> Vec<PlanEvidence> {
    let meta = meta_by_id_statement();
    vec![
        PlanEvidence {
            label: "load_meta_on",
            statement: meta.clone(),
            plan: explain_plan(pool, &meta, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "binding_row_state_on",
            statement: BINDING_ROW_STATE_SQL.to_owned(),
            plan: explain_plan(pool, BINDING_ROW_STATE_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "frozen_bytes_on",
            statement: FROZEN_CONTEXT_SQL.to_owned(),
            plan: explain_plan(pool, FROZEN_CONTEXT_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "load_active_payloads_on",
            statement: ACTIVE_PAYLOADS_SQL.to_owned(),
            plan: explain_plan(pool, ACTIVE_PAYLOADS_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "load_active_flags_on",
            statement: ACTIVE_FLAGS_SQL.to_owned(),
            plan: explain_plan(pool, ACTIVE_FLAGS_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "load_inherited_context_on",
            statement: INHERITED_CONTEXT_SQL.to_owned(),
            plan: explain_plan(pool, INHERITED_CONTEXT_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "resolve_ancestor_chain_on",
            statement: PARENT_THREAD_ID_SQL.to_owned(),
            plan: explain_plan(pool, PARENT_THREAD_ID_SQL, &[Param::Text(thread)]).await,
        },
    ]
}

/// 初始 load 的附带读取与分页读取的计划（`anchor` 为空 = 首屏游标）。
async fn initial_load_extras(
    pool: &SqlitePool,
    thread: &str,
    anchor: Option<&str>,
    limit: usize,
) -> Vec<PlanEvidence> {
    let evidence = vec![
        PlanEvidence {
            label: "require_session（分页路径）",
            statement: THREAD_EXISTS_SQL.to_owned(),
            plan: explain_plan(pool, THREAD_EXISTS_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "read_active_entry_positions",
            statement: ACTIVE_ENTRY_POSITIONS_SQL.to_owned(),
            plan: explain_plan(pool, ACTIVE_ENTRY_POSITIONS_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "read_history_page_bounds",
            statement: HISTORY_PAGE_BOUNDS_SQL.to_owned(),
            plan: explain_plan(
                pool,
                HISTORY_PAGE_BOUNDS_SQL,
                &[Param::Text(thread), Param::OptionalText(anchor)],
            )
            .await,
        },
        PlanEvidence {
            label: "read_history_page/阶段一",
            statement: HISTORY_PAGE_ROWS_SQL.to_owned(),
            plan: explain_plan(
                pool,
                HISTORY_PAGE_ROWS_SQL,
                &[
                    Param::Text(thread),
                    Param::OptionalText(anchor),
                    Param::Integer(limit.saturating_add(1) as i64),
                ],
            )
            .await,
        },
    ];
    evidence
}

/// 阶段二正文读取的计划（按 id 的主键单行读取）。
async fn page_body_plan(pool: &SqlitePool, thread: &str, body_id: &str) -> Vec<PlanEvidence> {
    vec![PlanEvidence {
        label: "read_page_bodies/阶段二",
        statement: HISTORY_PAGE_BODY_SQL.to_owned(),
        plan: explain_plan(
            pool,
            HISTORY_PAGE_BODY_SQL,
            &[Param::Text(thread), Param::Text(body_id)],
        )
        .await,
    }]
}

/// 向前段读取（canonical 完整回放）的计划：边界（上界）、首段、锚点存在性、非首段。
///
/// 与分页读取同一套参数形状：会话文本、存储序键（`after`/`tail`，数值）与段长。
async fn segment_read_plans(
    pool: &SqlitePool,
    thread: &str,
    after: i64,
    tail: i64,
    limit: usize,
) -> Vec<PlanEvidence> {
    let rows = limit.saturating_add(1) as i64;
    vec![
        PlanEvidence {
            label: "read_history_segment/边界（上界）",
            statement: HISTORY_SEGMENT_TAIL_SQL.to_owned(),
            plan: explain_plan(pool, HISTORY_SEGMENT_TAIL_SQL, &[Param::Text(thread)]).await,
        },
        PlanEvidence {
            label: "read_history_segment/首段（阶段一）",
            statement: HISTORY_SEGMENT_HEAD_SQL.to_owned(),
            plan: explain_plan(
                pool,
                HISTORY_SEGMENT_HEAD_SQL,
                &[Param::Text(thread), Param::Integer(rows)],
            )
            .await,
        },
        PlanEvidence {
            label: "read_history_segment/锚点存在性",
            statement: HISTORY_SEGMENT_ANCHOR_SQL.to_owned(),
            plan: explain_plan(
                pool,
                HISTORY_SEGMENT_ANCHOR_SQL,
                &[Param::Text(thread), Param::Integer(after)],
            )
            .await,
        },
        PlanEvidence {
            label: "read_history_segment/非首段（阶段一）",
            statement: HISTORY_SEGMENT_NEXT_SQL.to_owned(),
            plan: explain_plan(
                pool,
                HISTORY_SEGMENT_NEXT_SQL,
                &[
                    Param::Text(thread),
                    Param::Integer(after),
                    Param::Integer(tail),
                    Param::Integer(rows),
                ],
            )
            .await,
        },
    ]
}

/// 需求禁止的两件事：`messages` 全表扫描与任何全历史聚合。
fn assert_bounded_plan(evidence: &PlanEvidence) {
    let upper = evidence.statement.to_uppercase();
    assert!(
        !upper.contains("SUM("),
        "{}：语句文本出现全历史聚合: {}",
        evidence.label,
        evidence.statement
    );
    for line in &evidence.plan {
        assert!(
            !line.contains("SCAN m"),
            "{}：计划出现 messages 全表扫描: {line}",
            evidence.label
        );
        assert!(
            !line.to_uppercase().contains("SUM("),
            "{}：计划出现聚合: {line}",
            evidence.label
        );
    }
}

/// 对 `messages` 的定界访问必须按索引/主键定位，而不是全表。
fn assert_located_by_index(evidence: &PlanEvidence) {
    assert!(
        evidence.plan.iter().any(|line| line.contains("USING INDEX")
            || line.contains("USING COVERING INDEX")
            || line.contains("USING INTEGER PRIMARY KEY")),
        "{}：未按索引定位: {:?}",
        evidence.label,
        evidence.plan
    );
}

fn print_plan(evidence: &PlanEvidence) {
    println!("  · {} — 语句: {}", evidence.label, evidence.statement);
    for line in &evidence.plan {
        println!("      {line}");
    }
}

/// 向前段读取的语句文本也可以用 `s` 当 `messages` 的别名（候选行）：既要按索引定位，也不能
/// 出现任何一种全表扫描（`assert_bounded_plan` 只认分页语句用的 `m`）。
fn assert_segment_plan_bounded(evidence: &PlanEvidence) {
    assert_bounded_plan(evidence);
    for line in &evidence.plan {
        assert!(
            !line.contains("SCAN s") && !line.contains("SCAN messages"),
            "{}：计划出现 messages 全表扫描: {line}",
            evidence.label
        );
    }
}

// ── 探针、统计与环境事实 ──────────────────────────────────────────────────────

/// 会话行数与其中被排除的行数：**行数探针**（方法在报告里写明，不冒充计划给出的数字）。
async fn row_probe(pool: &SqlitePool, thread: &str) -> (i64, i64) {
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE thread_id = ?1")
        .bind(thread)
        .fetch_one(pool)
        .await
        .expect("行数探针");
    let excluded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND excluded = 1")
            .bind(thread)
            .fetch_one(pool)
            .await
            .expect("排除行数探针");
    (rows, excluded)
}

/// 落库占用事实：库文件、页数、`messages` 表占用（`dbstat` 不可用时显式说不可用）。
async fn storage_facts(pool: &SqlitePool) -> Vec<(String, String)> {
    let sqlite: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| "unknown".to_owned());
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let messages_bytes: Option<i64> =
        sqlx::query_scalar("SELECT SUM(pgsize) FROM dbstat WHERE name = 'messages'")
            .fetch_one(pool)
            .await
            .ok()
            .flatten();
    vec![
        ("sqlite_version".to_owned(), sqlite),
        (
            "page_size/page_count".to_owned(),
            format!("{page_size}/{page_count}"),
        ),
        (
            "messages 表落库占用（dbstat）".to_owned(),
            messages_bytes
                .map(|bytes| format!("{bytes} 字节"))
                .unwrap_or_else(|| "dbstat 不可用".to_owned()),
        ),
    ]
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// 机器与工具链：报告里要写明，所以就地取。
fn machine_facts() -> Vec<(String, String)> {
    vec![
        (
            "平台".to_owned(),
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ),
        (
            "CPU".to_owned(),
            command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
                .unwrap_or_else(|| "不可用".to_owned()),
        ),
        (
            "rustc".to_owned(),
            command_output("rustc", &["-vV"]).unwrap_or_else(|| "不可用".to_owned()),
        ),
    ]
}

/// 解码回来的 payload 编码字节（实测；与写入的字节数比较即「只有有效段被解码」的字节级证据）。
fn decoded_bytes(snapshot: &ActiveSessionSnapshot) -> usize {
    snapshot
        .payloads
        .iter()
        .map(|payload| {
            serialize_persisted_payload(payload)
                .map(|encoded| encoded.len())
                .unwrap_or(0)
        })
        .sum()
}

struct RestoreSample {
    elapsed: Duration,
    payloads: usize,
    flags: usize,
    payload_bytes: usize,
}

fn restore_sample(elapsed: Duration, snapshot: &ActiveSessionSnapshot) -> RestoreSample {
    RestoreSample {
        elapsed,
        payloads: snapshot.payloads.len(),
        flags: snapshot.flags.len(),
        payload_bytes: decoded_bytes(snapshot),
    }
}

/// 冷：每次读取前新开一个部署句柄（新连接池 ⇒ SQLite 页缓存为空）；句柄打开不计入计时。
async fn restore_cold(db_path: &Path, thread: &str, iters: usize) -> Vec<RestoreSample> {
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let facade = deployment(db_path).await;
        let started = Instant::now();
        let snapshot = facade
            .load_active_snapshot(&thread.to_owned())
            .await
            .expect("初始恢复");
        samples.push(restore_sample(started.elapsed(), &snapshot));
        drop(facade);
    }
    samples
}

/// 热：同一个句柄上连续读取（先预热一次，避免把首次读取当热态）。
async fn restore_hot(
    facade: &Arc<dyn SessionResources>,
    thread: &str,
    iters: usize,
) -> Vec<RestoreSample> {
    let _ = facade
        .load_active_snapshot(&thread.to_owned())
        .await
        .expect("预热读取");
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let started = Instant::now();
        let snapshot = facade
            .load_active_snapshot(&thread.to_owned())
            .await
            .expect("初始恢复");
        samples.push(restore_sample(started.elapsed(), &snapshot));
    }
    samples
}

fn percentile(sorted: &[Duration], percent: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = (((sorted.len() - 1) as f64) * percent).round() as usize;
    sorted[index]
}

fn ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.0
}

fn print_durations(label: &str, samples: &[Duration]) {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let mean = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().map(|value| value.as_secs_f64()).sum::<f64>() / sorted.len() as f64 * 1000.0
    };
    println!(
        "  {label}: n={} p50={:.3}ms p95={:.3}ms min={:.3}ms max={:.3}ms mean={mean:.3}ms",
        sorted.len(),
        ms(percentile(&sorted, 0.50)),
        ms(percentile(&sorted, 0.95)),
        ms(sorted.first().copied().unwrap_or_default()),
        ms(sorted.last().copied().unwrap_or_default()),
    );
}

fn print_restore_stats(label: &str, samples: &[RestoreSample]) {
    let durations: Vec<Duration> = samples.iter().map(|sample| sample.elapsed).collect();
    print_durations(label, &durations);
    if let Some(first) = samples.first() {
        println!(
            "      返回 payload={} flags={} 解码正文字节={}",
            first.payloads, first.flags, first.payload_bytes
        );
    }
}

// ── 用例 ──────────────────────────────────────────────────────────────────────

/// A04 / A05 / A06：三种归档规模下的初始恢复证据 + 最大规模的 20 页遍历。
#[ignore = "性能证据夹具（1 万 / 10 万 / 100 万行）与冷热计时；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_restore_and_paging_evidence() {
    let sizes = env_rows();
    let iters = env_usize("PERI_PERF_ITERS", DEFAULT_ITERS);
    let pages = env_usize("PERI_PERF_PAGES", DEFAULT_PAGES);
    let largest = *sizes.iter().max().expect("至少一个规模");

    println!("== 环境 ==");
    for (key, value) in machine_facts() {
        println!("  {key}: {value}");
    }
    println!(
        "  夹具：有效 {LIVE_ENTRIES} 条 × {LIVE_BODY_BYTES} 字节；归档正文 {ARCHIVED_BODY_BYTES} 字节；\
         页 limit={PAGE_LIMIT} 目标={PAGE_BYTE_BUDGET} 字节；iters={iters} pages={pages}"
    );

    for size in sizes {
        println!("\n== 归档 {size} 行 ==");
        let build_started = Instant::now();
        let fixture = build_fixture(size, ArchivedBody::Payload, ARCHIVED_BODY_BYTES).await;
        let build_cost = build_started.elapsed().as_secs_f64();
        let db_bytes = std::fs::metadata(&fixture.db_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        println!(
            "  夹具：messages={} 行（归档 {}，有效 {}），归档正文合计 {} 字节，有效正文合计 {} 字节，库文件 {db_bytes} 字节，夹具生成 {build_cost:.1}s",
            fixture.messages(),
            fixture.archived_rows,
            LIVE_ENTRIES,
            fixture.archived_bytes,
            fixture.live_bytes,
        );

        let pool = fixture_pool(&fixture.db_path).await;
        for (key, value) in storage_facts(&pool).await {
            println!("  {key}: {value}");
        }
        let (rows_in_range, excluded_rows) = row_probe(&pool, &fixture.thread).await;

        println!("  -- 计划（EXPLAIN QUERY PLAN，生产语句原文） --");
        let plans_started = Instant::now();
        let mut plans = restore_chain_plans(&pool, &fixture.thread).await;
        plans.extend(initial_load_extras(&pool, &fixture.thread, None, PAGE_LIMIT).await);
        let anchor = fixture.live_ids[0].as_uuid().to_string();
        plans.extend(page_body_plan(&pool, &fixture.thread, &anchor).await);
        for evidence in &plans {
            assert_bounded_plan(evidence);
            if evidence.label.contains("active_payloads")
                || evidence.label.contains("active_flags")
                || evidence.label.contains("entry_positions")
                || evidence.label.contains("history_page")
                || evidence.label.contains("page_bodies")
            {
                assert_located_by_index(evidence);
            }
            print_plan(evidence);
        }
        println!(
            "  计划采集耗时 {:.3}s",
            plans_started.elapsed().as_secs_f64()
        );
        pool.close().await;

        // 实测：恢复只返回有效段（条数与字节都对齐夹具的有效段）。
        let facade = deployment(&fixture.db_path).await;
        let snapshot = facade
            .load_active_snapshot(&fixture.thread)
            .await
            .expect("初始恢复");
        let ids: Vec<MessageId> = snapshot
            .payloads
            .iter()
            .map(|payload| payload.id())
            .collect();
        assert_eq!(ids, fixture.live_ids, "有效集合必须与写入的有效段一致");
        let decoded = decoded_bytes(&snapshot);
        assert_eq!(
            decoded, fixture.live_bytes,
            "解码的正文必须只有有效段（字节级证据）"
        );
        assert_eq!(
            snapshot.flags.len(),
            1,
            "活跃 flags 只应返回带标记的有效条目"
        );
        println!(
            "  恢复实测：payloads={} flags={} 解码正文字节={} | 范围内行数（探针）={rows_in_range} 其中 excluded={excluded_rows}",
            snapshot.payloads.len(),
            snapshot.flags.len(),
            decoded,
        );
        println!(
            "  恢复的语句数（代码路径逐条列出，非执行期计数）：恢复链 7 条（session_data.rs 的 \
             load_active_snapshot）+ 该链不含分页/位置读取"
        );

        let cold = restore_cold(&fixture.db_path, &fixture.thread, iters).await;
        print_restore_stats("恢复 cold（每次新句柄，SQLite 页缓存为空）", &cold);
        let hot = restore_hot(&facade, &fixture.thread, iters).await;
        print_restore_stats("恢复 hot（同一句柄连续读取）", &hot);
        for sample in cold.iter().chain(hot.iter()) {
            assert_eq!(sample.payloads, LIVE_ENTRIES, "每次恢复都只返回有效段");
            assert_eq!(sample.payload_bytes, fixture.live_bytes);
        }

        if size == largest {
            println!(
                "  -- 分页：{pages} 页 × limit={PAGE_LIMIT} / 目标 {PAGE_BYTE_BUDGET} 字节（同一句柄） --"
            );
            let mut cursor: Option<HistoryCursor> = None;
            let mut previous_min: Option<u64> = None;
            let mut walked = 0usize;
            let mut walk_durations = Vec::with_capacity(pages);
            for page_index in 0..pages {
                let started = Instant::now();
                let page = facade
                    .load_history_page(
                        &fixture.thread,
                        cursor.as_ref(),
                        PAGE_LIMIT,
                        PAGE_BYTE_BUDGET,
                    )
                    .await
                    .expect("读取历史页");
                let elapsed = started.elapsed();
                let bytes: usize = page.items.iter().map(|item| item.encoded_bytes).sum();
                let positions: Vec<u64> = page.items.iter().map(|item| item.position).collect();
                assert!(page.items.len() <= PAGE_LIMIT, "页不得超过 limit");
                assert!(bytes <= PAGE_BYTE_BUDGET, "普通页不得超过字节目标");
                assert!(
                    positions.windows(2).all(|pair| pair[0] < pair[1]),
                    "页内必须正序（更早 → 更晚）"
                );
                if let Some(previous_min) = previous_min {
                    assert!(positions[0] < previous_min, "下一页必须严格更旧");
                }
                let oldest = *positions.first().expect("页非空");
                previous_min = Some(oldest);
                walked += page.items.len();
                walk_durations.push(elapsed);
                println!(
                    "    第 {:>2} 页: items={} bytes={} oversize={} 最旧 position={} 语句数（代码路径）= {} p_ms={:.3}",
                    page_index + 1,
                    page.items.len(),
                    bytes,
                    page.items.iter().filter(|item| item.oversized).count(),
                    oldest,
                    3 + page.items.len(),
                    ms(elapsed),
                );
                cursor = page.next_cursor.clone();
                assert!(cursor.is_some(), "{pages} 页之内不得到头");
            }
            if fixture.messages() as usize >= pages * PAGE_LIMIT {
                assert_eq!(
                    walked,
                    pages * PAGE_LIMIT,
                    "连续翻页必须不重不漏地覆盖 limit × 页数 条"
                );
            }
            let mut first_page = Vec::with_capacity(iters);
            for _ in 0..iters {
                let started = Instant::now();
                let _ = facade
                    .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
                    .await
                    .expect("首屏页");
                first_page.push(started.elapsed());
            }
            print_durations("分页 hot（无游标首屏，重复读）", &first_page);
            print_durations("分页 20 页连续遍历（含游标页）", &walk_durations);
        }
    }
}

/// A04：被排除的正文**一条都不解码**——归档正文写成不可解码内容，一旦读到就失败。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_excluded_bodies_are_never_decoded() {
    let rows = env_usize("PERI_PERF_TRIPWIRE_ROWS", DEFAULT_TRIPWIRE_ROWS);
    let fixture = build_fixture(rows, ArchivedBody::Undecodable, 0).await;
    println!(
        "== 探针夹具：归档 {rows} 行，正文为不可解码占位（{} 字节/行，合计 {} 字节） ==",
        UNDECODABLE_ARCHIVED_BODY.len(),
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;

    // 对照组：完整读取会解码归档正文，必须失败——证明占位内容真的不可解码。
    let full = facade.load_session_snapshot(&fixture.thread).await;
    let full_error = full.expect_err("对照组必须失败：完整读取会解码归档正文");
    println!("  对照组 load_session_snapshot 失败（预期）：{full_error}");

    // 处理组：初始恢复只读有效范围，必须成功且只返回有效段。
    let snapshot = facade
        .load_active_snapshot(&fixture.thread)
        .await
        .expect("初始恢复不得读归档正文");
    let ids: Vec<MessageId> = snapshot
        .payloads
        .iter()
        .map(|payload| payload.id())
        .collect();
    assert_eq!(ids, fixture.live_ids, "只返回有效段");
    let decoded = decoded_bytes(&snapshot);
    assert_eq!(decoded, fixture.live_bytes, "解码字节只等于有效段");
    println!(
        "  处理组 load_active_snapshot 成功：payloads={} flags={} 解码正文字节={}（归档 {} 行正文一条未解码）",
        snapshot.payloads.len(),
        snapshot.flags.len(),
        decoded,
        fixture.archived_rows,
    );
}

/// A07：单条正文超过页字节目标时独占一页、内容完整、游标前进。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_oversize_page_carries_one_body() {
    const OVERSIZE_ROWS: usize = 6;
    const OVERSIZE_BODY_BYTES: usize = 1_250_000;
    let fixture = build_fixture(OVERSIZE_ROWS, ArchivedBody::Payload, OVERSIZE_BODY_BYTES).await;
    println!(
        "== oversize 夹具：{OVERSIZE_ROWS} 条 × >1 MiB 正文（合计 {} 字节），页目标 {PAGE_BYTE_BUDGET} 字节 ==",
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;
    // 首屏：最新的 3 条有效正文各自都放得进预算，超预算的归档正文被留到下一页。
    let first = facade
        .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
        .await
        .expect("读取首屏页");
    assert_eq!(first.items.len(), LIVE_ENTRIES, "首屏是有效段");
    println!(
        "    第 1 页（有效段）: items={} bytes={} oversize={}",
        first.items.len(),
        first
            .items
            .iter()
            .map(|item| item.encoded_bytes)
            .sum::<usize>(),
        first.items.iter().filter(|item| item.oversized).count(),
    );
    let mut cursor: Option<HistoryCursor> = first.next_cursor.clone();
    let mut previous_min: Option<u64> = first.items.iter().map(|item| item.position).min();
    for page_index in 0..3 {
        let started = Instant::now();
        let page = facade
            .load_history_page(
                &fixture.thread,
                cursor.as_ref(),
                PAGE_LIMIT,
                PAGE_BYTE_BUDGET,
            )
            .await
            .expect("读取 oversize 页");
        let elapsed = started.elapsed();
        assert_eq!(page.items.len(), 1, "oversized 封装必须独占一页");
        let item = &page.items[0];
        assert!(item.oversized, "超过字节目标必须显式标记 oversize");
        assert!(
            item.encoded_bytes > PAGE_BYTE_BUDGET,
            "本页正文必须确实超过预算"
        );
        assert!(
            item.payload.as_message().is_some(),
            "内容必须完整（可解码为消息）"
        );
        if let Some(previous_min) = previous_min {
            assert!(item.position < previous_min, "游标必须严格更旧");
        }
        previous_min = Some(item.position);
        println!(
            "    第 {} 页: items=1 bytes={} oversize={} position={} 语句数（代码路径）= 4 p_ms={:.3}",
            page_index + 2,
            item.encoded_bytes,
            item.oversized,
            item.position,
            ms(elapsed),
        );
        cursor = page.next_cursor.clone();
        assert!(cursor.is_some(), "oversized 页之后游标必须继续前进");
    }
}

/// A06：普通页长度由字节预算（而不是条数上限）决定时应停在预算上。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_byte_budget_bounds_page_length() {
    const ROWS: usize = 200;
    const BODY_BYTES: usize = 20 * 1024;
    let fixture = build_fixture(ROWS, ArchivedBody::Payload, BODY_BYTES).await;
    println!(
        "== 字节预算夹具：{ROWS} 条 × 约 {BODY_BYTES} 字节正文（合计 {} 字节），页目标 {PAGE_BYTE_BUDGET} 字节 ==",
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;
    let started = Instant::now();
    let page = facade
        .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
        .await
        .expect("读取首屏页");
    let elapsed = started.elapsed();
    let bytes: usize = page.items.iter().map(|item| item.encoded_bytes).sum();
    assert!(
        page.items.len() < PAGE_LIMIT,
        "这一页应当由字节预算而不是条数上限截断"
    );
    assert!(bytes <= PAGE_BYTE_BUDGET, "不得超过页字节目标");
    assert!(
        bytes + BODY_BYTES > PAGE_BYTE_BUDGET,
        "预算应当被用满到只差下一条放不下"
    );
    println!(
        "    首屏页: items={} bytes={}（{:.1}% 预算）p_ms={:.3} 语句数（代码路径）= {}",
        page.items.len(),
        bytes,
        bytes as f64 / PAGE_BYTE_BUDGET as f64 * 100.0,
        ms(elapsed),
        3 + page.items.len(),
    );
}

/// 新增的向前段读取语句的计划证据：按索引定位、无 `messages` 全表扫描。
///
/// 与其余用例不同，这一条**默认运行**（不加 `--ignored`）：`EXPLAIN QUERY PLAN` 只看 schema
/// 与索引，不需要大夹具，所以「新增语句是定界读取」这件事不依赖性能夹具是否跑过。
#[tokio::test]
async fn history_segment_statements_are_index_located() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("history-segment-plan.db");
    // 生产的打开路径建库（schema 与索引来自同一份 canonical DDL）。
    let resources = deployment(&db_path).await;
    let pool = fixture_pool(&db_path).await;

    println!("== 向前段读取的计划（EXPLAIN QUERY PLAN，生产语句原文；无夹具） ==");
    let plans = segment_read_plans(&pool, PERF_SESSION, 1, 2, PAGE_LIMIT).await;
    assert_eq!(plans.len(), 4);
    for evidence in &plans {
        assert_segment_plan_bounded(evidence);
        assert_located_by_index(evidence);
        print_plan(evidence);
    }

    drop(resources);
    pool.close().await;
}

// ── 审计补充（verification 独立复核用；不是切片交付物） ────────────────────────

/// `EXPLAIN`（VDBE 字节码）原文：`addr opcode p1 p2 p3 p5 p4 comment`。
///
/// 与 `EXPLAIN QUERY PLAN` 的差别：字节码能回答「残差谓词在索引项上判定，还是回表之后才判定」，
/// 用来把「按索引定位」与「触达行数有界」分开。参数不绑定（`EXPLAIN` 不执行语句）。
async fn explain_bytecode(pool: &SqlitePool, sql: &str) -> Vec<String> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN {sql}")))
        .fetch_all(pool)
        .await
        .expect("EXPLAIN");
    rows.iter()
        .map(|row| {
            let addr: i64 = row.try_get(0).unwrap_or(-1);
            let opcode: String = row.try_get(1).unwrap_or_default();
            let p1: i64 = row.try_get(2).unwrap_or(0);
            let p2: i64 = row.try_get(3).unwrap_or(0);
            let p3: i64 = row.try_get(4).unwrap_or(0);
            let p4: Option<String> = row.try_get(5).unwrap_or(None);
            let p5: i64 = row.try_get(6).unwrap_or(0);
            let comment: Option<String> = row.try_get(7).unwrap_or(None);
            format!(
                "{addr:>4} {opcode:<16} {p1:>3} {p2:>4} {p3:>4} {p5:>2} {} {}",
                p4.unwrap_or_default(),
                comment.unwrap_or_default()
            )
        })
        .collect()
}

/// 审计证据（独立复核，不改动生产语义）：
///
/// ① 两条阶段一语句的 `EXPLAIN` 字节码（残差判定位置）；
/// ② 探针夹具：本段之后那一条候选行的正文不可解码——本段仍能读出（阶段二没取它），
///    而把它推进为下一段时以 `Corrupt` 失败（证明探针真的不可解码）；
/// ③ 段长与字节预算的实测：逐段打印行数与编码字节。
#[ignore = "审计补充证据；命令与输出见 .recon/S9-boundedness.md"]
#[tokio::test]
async fn audit_segment_lookahead_and_plan_shape() {
    // ── ① 字节码形状（无夹具） ──
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("audit-plan.db");
    let resources = deployment(&db_path).await;
    let pool = fixture_pool(&db_path).await;
    println!("== EXPLAIN（VDBE 字节码，生产语句原文；参数未绑定） ==");
    for (label, sql) in [
        (
            "HISTORY_PAGE_ROWS_SQL（分页阶段一，带游标）",
            HISTORY_PAGE_ROWS_SQL,
        ),
        (
            "HISTORY_SEGMENT_NEXT_SQL（回放非首段）",
            HISTORY_SEGMENT_NEXT_SQL,
        ),
        // 对照语句（**不是生产语句**，只为证据）：不带 CAST 的 length()。它的 `Column` 上应出现
        // `OPFLAG_LENGTHARG`（p5 含 64）——生产语句若没有这个标志，就说明长度是从**取回的值**
        // 上算的，而不是从记录头里的序列类型直接读出的长度。
        (
            "对照：SELECT length(content)（无 CAST）",
            "SELECT length(content) FROM messages WHERE thread_id = ?1",
        ),
    ] {
        println!("  · {label} — 语句: {sql}");
        for line in explain_bytecode(&pool, sql).await {
            println!("      {line}");
        }
    }
    drop(resources);
    pool.close().await;

    // ── ② 探针：本段之后那一条候选行的正文不可解码 ──
    let fixture = build_fixture(6, ArchivedBody::Payload, 8 * 1024).await;
    let pool = fixture_pool(&fixture.db_path).await;
    insert_rows(
        &pool,
        &fixture.thread,
        &[FixtureRow {
            message_id: MessageId::new().as_uuid().to_string(),
            content: UNDECODABLE_ARCHIVED_BODY.to_owned(),
            excluded: false,
            truncated: false,
        }],
    )
    .await;
    pool.close().await;
    let facade = deployment(&fixture.db_path).await;
    println!(
        "== 探针：6 条 8 KiB 归档行 + 3 条 64 KiB 有效行 + 1 条不可解码行（rowid 最大），limit=3 =="
    );
    let mut cursor: Option<peri_acp_types::history_page::HistoryForwardCursor> = None;
    let mut index = 0usize;
    loop {
        index += 1;
        let outcome = facade
            .load_history_segment(&fixture.thread, cursor.as_ref(), 3, usize::MAX)
            .await;
        match outcome {
            Ok(segment) => {
                let ids: Vec<MessageId> =
                    segment.payloads.iter().map(PersistedPayload::id).collect();
                println!(
                    "    第 {index} 段: payloads={} next_cursor={}",
                    segment.payloads.len(),
                    segment.next_cursor.is_some()
                );
                if index == 3 {
                    assert_eq!(
                        ids, fixture.live_ids,
                        "第 3 段是有效段；它的下一条候选行是不可解码行，本段读出即证明阶段二没取它"
                    );
                }
                match segment.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            Err(error) => {
                println!("    第 {index} 段失败: {error}");
                assert!(
                    matches!(
                        error.kind(),
                        peri_acp_types::session_resources::SessionResourceErrorKind::Corrupt { .. }
                    ),
                    "不可解码行被推进时才应以 Corrupt 失败，实际: {error}"
                );
                assert_eq!(index, 4, "只有推进到不可解码行的那一段才允许失败");
                break;
            }
        }
    }
    assert_eq!(index, 4, "探针行必须真的被推进过一段（否则探针没响）");

    // ── ③ 段长与字节预算实测 ──
    let fixture = build_fixture(6, ArchivedBody::Payload, 8 * 1024).await;
    let facade = deployment(&fixture.db_path).await;
    let budget = 96 * 1024;
    let limit = 50;
    println!("== 段长/字节预算实测：limit={limit} 预算={budget} 字节 ==");
    let mut cursor: Option<peri_acp_types::history_page::HistoryForwardCursor> = None;
    let mut total_rows = 0usize;
    let mut total_bytes = 0usize;
    loop {
        let segment = facade
            .load_history_segment(&fixture.thread, cursor.as_ref(), limit, budget)
            .await
            .expect("段读取");
        let bytes: usize = segment
            .payloads
            .iter()
            .map(|payload| {
                serialize_persisted_payload(payload)
                    .expect("段载荷可序列化")
                    .len()
            })
            .sum();
        println!(
            "    段: payloads={} bytes={} 超预算={} next_cursor={}",
            segment.payloads.len(),
            bytes,
            bytes > budget,
            segment.next_cursor.is_some()
        );
        assert!(segment.payloads.len() <= limit, "段不得超过 limit");
        assert!(
            bytes <= budget || segment.payloads.len() == 1,
            "超过字节预算时只允许单条独占一段"
        );
        total_rows += segment.payloads.len();
        total_bytes += bytes;
        match segment.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    println!("    合计推进 {total_rows} 条自有行 / {total_bytes} 字节（夹具：6 归档 + 3 有效）");
    assert_eq!(total_rows, (6 + LIVE_ENTRIES) as usize, "回放必须不重不漏");
    assert_eq!(
        total_bytes,
        fixture.archived_bytes + fixture.live_bytes,
        "段内编码字节之和必须等于夹具写入的正文编码字节"
    );

    // ── ④ 反例：回放中途删掉 tail 那一行（精确删除/rewind 的尾部切除）再追加新行 ──
    //
    // `tail` 是首段那一刻的**数值**上界；契约只要求「追加的行 rowid > tail 时不进入本次遍历」，
    // 不检测上界行自己消失。SQLite 在删掉最大 rowid 之后会把该 rowid 复用给下一次插入，
    // 于是新行落回 (after, tail] 区间内。这一条测的是该情形下的实际交付。
    let fixture = build_fixture(0, ArchivedBody::Payload, 0).await;
    let facade = deployment(&fixture.db_path).await;
    let thread = fixture.thread.clone();
    let first = facade
        .load_history_segment(&thread, None, 2, usize::MAX)
        .await
        .expect("首段");
    let cursor = first.next_cursor.clone().expect("还有第 3 条");
    let removed = fixture.live_ids[2];
    facade
        .remove_history_entries(&thread, &[removed])
        .await
        .expect("删除 tail 行");
    let appended = PersistedPayload::Message(BaseMessage::human("回放期间新增"));
    facade
        .append_history(&thread, std::slice::from_ref(&appended))
        .await
        .expect("追加新行");
    let next = facade
        .load_history_segment(&thread, Some(&cursor), 5, usize::MAX)
        .await
        .expect("续段");
    let next_ids: Vec<MessageId> = next.payloads.iter().map(PersistedPayload::id).collect();
    println!(
        "    反例（删 tail 行 + 追加）：续段 payloads={} 含新行={} 含被删行={} next_cursor={}",
        next.payloads.len(),
        next_ids.contains(&appended.id()),
        next_ids.contains(&removed),
        next.next_cursor.is_some()
    );
}
