//! Bound SQL shared by migration snapshot reads and batch guards.

/// 会话行：机器归属可能缺行（旧版打开路径不写环境表）。
pub(super) const READ_SESSIONS_SQL: &str = "SELECT t.id, t.parent_thread_id, t.cwd, e.machine_id
    FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id ORDER BY t.id";
/// 环境表整表缺失时（迁移输入允许它缺席）的会话行：机器归属全部按缺行处理（NULL），
/// 由 [`decode_legacy`] 落到兜底机器——读法不因为可选表不在而失败。
pub(super) const READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL: &str =
    "SELECT id, parent_thread_id, cwd, NULL FROM threads ORDER BY id";
/// 绑定行：`workspace_id` 指向执行登记，不是归属行。
pub(super) const READ_BINDINGS_SQL: &str =
    "SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd
    FROM session_bindings ORDER BY thread_id";
/// 登记行：归属证据随行搬到当前形状的归属行。
pub(super) const READ_REGISTRATIONS_SQL: &str =
    "SELECT id, project_id, root, discovery FROM workspaces ORDER BY id";
/// 项目行（id + locator，按 rowid 升序）：v15 的唯一键收窄为 `UNIQUE(locator)`，重复 locator
/// 必须保留最早一行并改绑引用。
pub(super) const READ_PROJECTS_SQL: &str = "SELECT id, locator FROM projects ORDER BY rowid";

/// 绑定行的创建证据：v15 清理要把它投影成最小执行根，因此单独读一次原文。
pub(super) const READ_BINDING_SNAPSHOTS_SQL: &str =
    "SELECT thread_id, discovery_snapshot FROM session_bindings ORDER BY thread_id";
/// 历史行数：重建链之后的结果守卫（[`GUARD_MESSAGES_COUNT_SQL`]）按它核对搬完之后的历史还在不在。
pub(super) const MESSAGES_COUNT_SQL: &str = "SELECT COUNT(*) FROM messages";

pub(super) const GUARD_OBJECT_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM sqlite_master WHERE substr(lower(name), 1, 7) <> 'sqlite_') <> ?1";
pub(super) const GUARD_OBJECT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2 AND tbl_name = ?3 AND sql IS ?4)";
pub(in crate::sessions::remote) const GUARD_META_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM peri_store_meta WHERE singleton = 0 AND schema_version = ?1 AND store_id = ?2 AND contract = ?3)";
pub(super) const GUARD_THREAD_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE (SELECT COUNT(*) FROM threads) <> ?1";
pub(super) const GUARD_THREAD_ROW_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id
    WHERE t.id = ?1 AND t.parent_thread_id IS ?2 AND t.cwd = ?3 AND e.machine_id IS ?4)";
/// 环境表整表缺失时的会话行守卫：与 [`READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL`] 读回的同一事实
/// （机器归属恒为 NULL）。
pub(super) const GUARD_THREAD_ROW_WITHOUT_ENVIRONMENTS_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM threads t WHERE t.id = ?1 AND t.parent_thread_id IS ?2 AND t.cwd = ?3 AND ?4 IS NULL)";
/// 绑定行的并发拒绝面：重建只搬批外读到的绑定行，批间新增的行必须以唯一键冲突失败。
/// `pub(super)` 供 additive 计划断言这条守卫落在破坏性语句之前。
pub(in crate::sessions::remote) const GUARD_BINDING_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM session_bindings) <> ?1";
pub(super) const GUARD_BINDING_ROW_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM session_bindings
    WHERE thread_id = ?1 AND schema_version = ?2 AND project_id = ?3 AND workspace_id = ?4 AND relative_cwd = ?5)";
/// 登记行的守卫：两批读取之间对 `workspaces` 的并发写入如果不拦，迁移会按旧快照重建这张表
/// 并覆盖并发数据。逐行守卫覆盖登记行的全部事实列（与 `READ_REGISTRATIONS_SQL` 同列）。
///
/// `pub(super)`：这两条与下一条守卫是批次的拒绝面，由 `schema_upgrade_test` 按同一条语句
/// 断言它们仍在批次里、且落在破坏性语句之前。
pub(in crate::sessions::remote) const GUARD_REGISTRATION_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM workspaces) <> ?1";
pub(in crate::sessions::remote) const GUARD_REGISTRATION_ROW_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM workspaces
    WHERE id = ?1 AND project_id = ?2 AND root = ?3 AND discovery = ?4)";
/// 重建前的归属守卫：重建后的 `workspace_id` 是 NOT NULL，无归属的会话行会让重建在服务端
/// 失败并回滚整批——那时行已经搬进过暂存表。守卫插在重建链**之前**把拒绝提前：它之前的
/// `ALTER` / `UPDATE` 由整批回滚覆盖，没有任何行被搬进当前形状。
pub(in crate::sessions::remote) const GUARD_UNASSIGNED_THREADS_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0
    WHERE EXISTS (SELECT 1 FROM threads WHERE workspace_id IS NULL)";
/// 重建链之后的**结果守卫**：搬完之后历史行数必须与搬到的一致。
///
/// 为什么要它：`DROP TABLE threads` 在外键被打开时不是简单删除，而是**隐式删除 + 级联**——
/// `messages.thread_id REFERENCES threads(id) ON DELETE CASCADE` 会跟着清空全部历史，而远端
/// 服务端的 `PRAGMA foreign_keys` 是跨连接共享的可变状态（见 `RemoteStore::force_parent_checks_off`），
/// 归位失效时本机收不到任何信号；远端也没有本机那样的 `PRAGMA foreign_key_check` 兜底。
/// 于是把「历史还在不在」做成批次里的一条断言：行数不符即唯一键冲突，整批回滚，历史原样保留。
///
/// 位置与其余守卫相反（其余都在任何搬动之前）：级联只发生在重建链里，前置核对看不到它。
pub(in crate::sessions::remote) const GUARD_MESSAGES_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0
    WHERE (SELECT COUNT(*) FROM messages) <> ?1";
/// 绑定行的创建证据守卫：逐行核对 `discovery_snapshot` 在批外读到、批内写入之前没有被
/// 并发改写。现有 `GUARD_BINDING_COUNT_SQL` / `GUARD_BINDING_ROW_SQL` 不覆盖该列。
pub(super) const GUARD_BINDING_SNAPSHOT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM session_bindings WHERE thread_id = ?1 AND discovery_snapshot IS ?2)";
pub(super) const INSERT_MACHINE_SQL: &str =
    "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, ?2, ?3)";
pub(super) const INSERT_WORKSPACE_SQL: &str =
    "INSERT INTO workspaces(id, machine_id, path, path_source, project_id, discovery)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
pub(super) const INSERT_BINDING_SQL: &str = "INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
pub(super) const UPDATE_THREAD_WORKSPACE_SQL: &str =
    "UPDATE threads SET workspace_id = ?2 WHERE id = ?1";
pub(super) const ADVANCE_VERSION_SQL: &str =
    "UPDATE peri_store_meta SET schema_version = ?1, contract = ?2 WHERE singleton = 0 AND schema_version = ?3 AND store_id = ?4 AND contract = ?5";
