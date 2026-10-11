//! canonical 形状的**声明式快照**与比对：新建、迁移终点与远端服务端共用一份判定。
//!
//! 为什么不直接比对 DDL 文本：升级库的 `sqlite_master.sql` 保留历史建表文本（旧版本建表、
//! 后续补列），逐字比对会误拒真实库。形状判定的对象因此是**结构**——表的列序列、NOT NULL
//! 列与主键列——由两种执行器各自用同一条只读语句读回（[`CURRENT_COLUMNS_SQL`] /
//! [`INPUT_COLUMNS_SQL`]），再与这里的声明比对。
//!
//! 声明与 canonical DDL 的一致性不靠人工维护：`schema_shape_test` 断言「真实新建库的实际
//! 形状 == 声明」。DDL 改动而声明未跟上会在测试里失败，而不是在用户的库上变成误判。
//!
//! 三条判定线：
//!
//! - **当前形状**（[`CURRENT_TABLES`]，严格）：列序列、NOT NULL 集合、主键集合逐项相等。
//!   判定 Current 只是「能直接读写」的入口，误收残缺库的代价是延迟失败甚至静默改写，
//!   所以这里不宽容。只有两处例外（[`TableShape::optional`]）：`mcp_oauth_credentials`
//!   缺表即保持缺失（凭证能力按不可用上报，不替使用者凭空建一张空表），
//!   `session_close_intents` 由写打开幂等补齐；两者存在时仍必须逐项相符。
//! - **迁移输入形状**（[`INPUT_TABLES`]，带上下界）：下界是搬运必须读到的列，上界是
//!   本构建认识的列集合。搬运会 DROP 并重建这些表，未知列的值没有去处——出现未知列
//!   即拒绝升级（`fail-closed`），而不是把不认识的数据搬丢。
//! - **删除对象形状**（[`DROPPED_TABLES`]，严格）：搬运要删掉的压缩前表，删除前逐项判定其
//!   形状。同名的别的表（无论长得像不像）都不该被这次迁移删掉——判定不过即拒绝。
//!
//! 边界（有意不声明）：索引、列的类型文本与表级 UNIQUE 约束不参与比对。索引是**派生对象**
//! （缺失只影响性能，不改变读写语义）：由新建与迁移各自负责建齐，终点索引集在测试里断言，
//! 而不是让读取路径因为少一条索引拒绝一个健康的库。类型文本的规范化写法在两种执行器上
//! 没有实测；UNIQUE 缺失的失败模式是**显式报错**（`ON CONFLICT` 找不到唯一索引），不是
//! 静默写错。

use std::collections::HashMap;

/// 一列的实际形状（两种执行器读回后的共用形态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActualColumn {
    pub name: String,
    /// 列声明的 NOT NULL 位（`pragma_table_xinfo.notnull`）。
    pub not_null: bool,
    /// 列在表主键中的序号（`pragma_table_xinfo.pk`，0 表示不在主键中）。
    pub primary_key: i64,
}

impl ActualColumn {
    pub(super) fn new(name: impl Into<String>, not_null: bool, primary_key: i64) -> Self {
        Self {
            name: name.into(),
            not_null,
            primary_key,
        }
    }
}

/// 一表的结构声明：列序列（顺序即建表文本的定义顺序）、NOT NULL 集合与主键集合。
///
/// 当前形状（[`CURRENT_TABLES`]）与迁移中要删除的压缩前对象（[`DROPPED_TABLES`]）共用它。
/// 判定不宽容：声明与真实 DDL 的一致性由 `schema_shape_test` 断言，而不是靠人工维护。
pub(super) struct TableShape {
    pub name: &'static str,
    /// 缺表是否可接受（`true`）：搬入判定里「表不存在」不是漂移。存在时必须逐项相符——
    /// 这是「同名的别的表」的唯一守卫，不能因为可选就放宽。
    pub optional: bool,
    pub columns: &'static [&'static str],
    pub not_null: &'static [&'static str],
    pub primary_key: &'static [&'static str],
}

/// 迁移输入（压缩前形状）的一表契约。
pub(super) struct InputTable {
    pub name: &'static str,
    /// 表本身是否必须存在。老库上由迁移补齐的表（`mcp_oauth_credentials` /
    /// `session_environments`）缺表不是错误；搬运直接依赖的表缺表即拒绝。
    pub required_table: bool,
    /// 搬运必须读到的列（下界）。
    pub required: &'static [&'static str],
    /// 允许存在的列（上界）。`None` 表示不做上界校验——只有「不重建、不删除」的表可以。
    pub allowed: Option<&'static [&'static str]>,
}

const MACHINES_COLUMNS: &[&str] = &["id", "name", "identity_kind"];
const PROJECTS_COLUMNS: &[&str] = &["id", "locator", "object_identity"];
const WORKSPACES_COLUMNS: &[&str] = &[
    "id",
    "machine_id",
    "path",
    "path_source",
    "project_id",
    "identity",
    "discovery",
];
const THREADS_COLUMNS: &[&str] = &[
    "id",
    "title",
    "cwd",
    "created_at",
    "updated_at",
    "message_count",
    "parent_thread_id",
    "snapshot_at_message_id",
    "hidden",
    "cancel_policy",
    "config",
    "frozen_context",
    "inherited_context",
    "agent_status",
    "workspace_id",
    "archived",
];
const MESSAGES_COLUMNS: &[&str] = &[
    "message_id",
    "thread_id",
    "role",
    "content",
    "truncated",
    "excluded",
    "projection",
];
const BINDINGS_COLUMNS: &[&str] = &[
    "thread_id",
    "schema_version",
    "project_id",
    "workspace_id",
    "relative_cwd",
    "discovery_snapshot",
    "evidence_origin",
];
const OAUTH_COLUMNS: &[&str] = &[
    "principal_id",
    "workspace_id",
    "server_key",
    "credentials_blob",
    "updated_at",
];
const CLOSE_INTENTS_COLUMNS: &[&str] = &["thread_id", "requested_at"];

/// 当前形状（canonical 8 张表）的逐表声明。顺序与 `canonical::CANONICAL_TABLES` 一致。
pub(super) const CURRENT_TABLES: &[TableShape] = &[
    TableShape {
        name: "machines",
        optional: false,
        columns: MACHINES_COLUMNS,
        not_null: &["name", "identity_kind"],
        primary_key: &["id"],
    },
    TableShape {
        name: "projects",
        optional: false,
        columns: PROJECTS_COLUMNS,
        not_null: &["locator", "object_identity"],
        primary_key: &["id"],
    },
    TableShape {
        name: "workspaces",
        optional: false,
        columns: WORKSPACES_COLUMNS,
        not_null: &["machine_id", "path", "path_source"],
        primary_key: &["id"],
    },
    TableShape {
        name: "threads",
        optional: false,
        columns: THREADS_COLUMNS,
        not_null: &[
            "cwd",
            "created_at",
            "updated_at",
            "message_count",
            "hidden",
            "cancel_policy",
            "agent_status",
            "workspace_id",
            "archived",
        ],
        primary_key: &["id"],
    },
    TableShape {
        name: "messages",
        optional: false,
        columns: MESSAGES_COLUMNS,
        not_null: &["thread_id", "role", "content", "truncated", "excluded"],
        primary_key: &["message_id"],
    },
    TableShape {
        name: "session_bindings",
        optional: false,
        columns: BINDINGS_COLUMNS,
        not_null: &[
            "schema_version",
            "project_id",
            "workspace_id",
            "relative_cwd",
            "evidence_origin",
        ],
        primary_key: &["thread_id"],
    },
    TableShape {
        name: "mcp_oauth_credentials",
        // 可选：缺表即「本库不存凭证」。打开路径**不**补建（补建会把「凭证能力不可用」
        // 悄悄换成「没有凭证」），因此缺表必须在判定里可接受。
        optional: true,
        columns: OAUTH_COLUMNS,
        not_null: &[
            "principal_id",
            "workspace_id",
            "server_key",
            "credentials_blob",
            "updated_at",
        ],
        primary_key: &["principal_id", "workspace_id", "server_key"],
    },
    TableShape {
        name: "session_close_intents",
        // 可选：写打开会幂等补齐这张表（判定先于补齐，所以这里必须容忍缺表）。
        optional: true,
        columns: CLOSE_INTENTS_COLUMNS,
        not_null: &["requested_at"],
        primary_key: &["thread_id"],
    },
];

/// 压缩前形状的 `threads` 列（= 当前形状去掉 `workspace_id` / `archived`）。
const INPUT_THREADS_COLUMNS: &[&str] = &[
    "id",
    "title",
    "cwd",
    "created_at",
    "updated_at",
    "message_count",
    "parent_thread_id",
    "snapshot_at_message_id",
    "hidden",
    "cancel_policy",
    "config",
    "frozen_context",
    "inherited_context",
    "agent_status",
];

/// 压缩前形状的 `mcp_oauth_credentials` 列（machine 作用域；当前形状换成 workspace 作用域）。
/// 同一份清单同时是迁移输入的列上界（[`INPUT_TABLES`]）与删除前的形状判定（[`DROPPED_TABLES`]）。
const LEGACY_OAUTH_COLUMNS: &[&str] = &[
    "principal_id",
    "machine_id",
    "server_key",
    "credentials_blob",
    "updated_at",
];

/// 压缩前形状独有的 `session_environments` 列（合并进归属行之后整表退役）。
const LEGACY_ENVIRONMENT_COLUMNS: &[&str] = &["thread_id", "machine_id"];

/// 迁移输入的逐表契约。顺序与 `INPUT_COLUMNS_SQL` 一致。
pub(super) const INPUT_TABLES: &[InputTable] = &[
    InputTable {
        name: "threads",
        required_table: true,
        required: INPUT_THREADS_COLUMNS,
        // 上界 = 当前列 ∪ 正式发布里退役的两个缓存列（`removal_plan` 在搬运开始前显式删除
        // 它们，因此允许出现；重建只搬运当前列，除此之外的列没有去处）。
        allowed: Some(&[
            "id",
            "title",
            "cwd",
            "created_at",
            "updated_at",
            "message_count",
            "parent_thread_id",
            "snapshot_at_message_id",
            "hidden",
            "cancel_policy",
            "config",
            "frozen_context",
            "inherited_context",
            "agent_status",
            "cached_context",
            "context_cache_epoch",
        ]),
    },
    InputTable {
        name: "workspaces",
        required_table: true,
        required: &["id", "project_id", "root", "root_identity", "discovery"],
        allowed: Some(&["id", "project_id", "root", "root_identity", "discovery"]),
    },
    InputTable {
        name: "session_bindings",
        required_table: true,
        required: &[
            "thread_id",
            "schema_version",
            "project_id",
            "workspace_id",
            "relative_cwd",
        ],
        // v2 形状的 `revision` 列由旧库降级路径显式删除；列入上界是为了不因为它仍在
        // 而拒绝（重建后它自然消失，与显式 DROP 等效）。
        allowed: Some(&[
            "thread_id",
            "schema_version",
            "project_id",
            "workspace_id",
            "relative_cwd",
            "revision",
        ]),
    },
    InputTable {
        name: "messages",
        required_table: true,
        required: MESSAGES_COLUMNS,
        // 不重建、不删除：未知列保持原样，无需上界。
        allowed: None,
    },
    InputTable {
        name: "mcp_oauth_credentials",
        required_table: false,
        required: LEGACY_OAUTH_COLUMNS,
        allowed: Some(LEGACY_OAUTH_COLUMNS),
    },
    InputTable {
        name: "session_environments",
        required_table: false,
        required: LEGACY_ENVIRONMENT_COLUMNS,
        allowed: Some(LEGACY_ENVIRONMENT_COLUMNS),
    },
];

/// 迁移中显式删除的压缩前对象：删除是破坏性的，因此**先判定形状**再删。
///
/// 形状声明来自压缩前的建表文本（`canonical::V10_CREATE_ENVIRONMENTS_TABLE_SQL` /
/// `V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL`），由 `schema_shape_test` 断言一致。同名的别的
/// 表（列序列不符）不删并拒绝迁移：迁移不认识它，也无权替使用者丢掉它。
pub(super) const DROPPED_TABLES: &[DroppedTable] = &[
    DroppedTable {
        shape: TableShape {
            name: "session_environments",
            optional: true,
            columns: LEGACY_ENVIRONMENT_COLUMNS,
            not_null: &["machine_id"],
            primary_key: &["thread_id"],
        },
        // 搬运为读入规划而补齐这张表（缺表则建、缺行则补）：搬完之后它必然存在，
        // 无条件删除，否则「为规划而建的表」会留在当前形状里。
        //
        // `IF EXISTS`：本机在规划前补齐它，但远端的升级输入可能整表缺失（远端初始化
        // 从未建过环境表，判定也允许缺表）——删除语句必须对缺表幂等，否则这类库会卡在
        // 「形状判定通过、批次在 DROP 处报 no such table」。同名异形的表仍由
        // [`check_dropped_shape`] 在计划之前拦下，`IF EXISTS` 不放松这条守卫。
        always_dropped: true,
        drop_sql: "DROP TABLE IF EXISTS session_environments",
    },
    DroppedTable {
        shape: TableShape {
            name: "mcp_oauth_credentials",
            optional: true,
            columns: LEGACY_OAUTH_COLUMNS,
            not_null: LEGACY_OAUTH_COLUMNS,
            primary_key: &["principal_id", "machine_id", "server_key"],
        },
        // 删除之后按当前形状重建，缺表时跳过删除即可。
        always_dropped: false,
        drop_sql: "DROP TABLE mcp_oauth_credentials",
    },
];

/// 迁移中显式删除的一表声明。
pub(super) struct DroppedTable {
    pub shape: TableShape,
    /// 缺表时是否仍下发删除（见 [`DROPPED_TABLES`] 的逐条说明）。
    pub always_dropped: bool,
    pub drop_sql: &'static str,
}

/// 读回当前形状的全部列形状（一条只读语句，本机与远端共用）。
///
/// 表不存在时该表没有任何行；调用方必须把「没有行」判成缺表，而不是空形状。
/// 列顺序由 `pragma_table_xinfo` 的 `cid` 决定，`ORDER BY 1, 5` 只按表名与 cid 稳定排序。
pub(super) const CURRENT_COLUMNS_SQL: &str = "SELECT 'machines' AS t, name, \"notnull\", pk, cid FROM pragma_table_xinfo('machines')
    UNION ALL SELECT 'projects', name, \"notnull\", pk, cid FROM pragma_table_xinfo('projects')
    UNION ALL SELECT 'workspaces', name, \"notnull\", pk, cid FROM pragma_table_xinfo('workspaces')
    UNION ALL SELECT 'threads', name, \"notnull\", pk, cid FROM pragma_table_xinfo('threads')
    UNION ALL SELECT 'messages', name, \"notnull\", pk, cid FROM pragma_table_xinfo('messages')
    UNION ALL SELECT 'session_bindings', name, \"notnull\", pk, cid FROM pragma_table_xinfo('session_bindings')
    UNION ALL SELECT 'mcp_oauth_credentials', name, \"notnull\", pk, cid FROM pragma_table_xinfo('mcp_oauth_credentials')
    UNION ALL SELECT 'session_close_intents', name, \"notnull\", pk, cid FROM pragma_table_xinfo('session_close_intents')
    ORDER BY 1, 5";

/// 读回迁移输入形状的全部列形状（缺表在结果里没有行，由[`check_input`]判定）。
pub(super) const INPUT_COLUMNS_SQL: &str = "SELECT 'threads' AS t, name, \"notnull\", pk, cid FROM pragma_table_xinfo('threads')
    UNION ALL SELECT 'workspaces', name, \"notnull\", pk, cid FROM pragma_table_xinfo('workspaces')
    UNION ALL SELECT 'session_bindings', name, \"notnull\", pk, cid FROM pragma_table_xinfo('session_bindings')
    UNION ALL SELECT 'messages', name, \"notnull\", pk, cid FROM pragma_table_xinfo('messages')
    UNION ALL SELECT 'mcp_oauth_credentials', name, \"notnull\", pk, cid FROM pragma_table_xinfo('mcp_oauth_credentials')
    UNION ALL SELECT 'session_environments', name, \"notnull\", pk, cid FROM pragma_table_xinfo('session_environments')
    ORDER BY 1, 5";

/// SQLite 的标识符大小写不敏感，两种执行器保留的列名文本可能各自不同；比较统一按
/// 大小写不敏感进行（同样的原因，声明里保持 canonical DDL 的小写写法）。
fn same_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn contains_name(names: &[&str], name: &str) -> bool {
    names.iter().any(|candidate| same_name(candidate, name))
}

/// 严格判定：实际列形状必须与声明逐项相同（当前形状与要删除的压缩前对象共用）。
///
/// 返回的 `Err` 只用于诊断（谁漂移了）；调用方一律把它折算成「拒绝」。
pub(super) fn check_strict(table: &TableShape, actual: &[ActualColumn]) -> Result<(), String> {
    if actual.is_empty() {
        // 可选表缺表不是漂移（存在时仍必须逐项相符，见下方判定）。
        if table.optional {
            return Ok(());
        }
        return Err(format!("table {} is missing", table.name));
    }
    let sequence_matches = actual.len() == table.columns.len()
        && actual
            .iter()
            .zip(table.columns)
            .all(|(column, expected)| same_name(&column.name, expected));
    if !sequence_matches {
        let names: Vec<&str> = actual.iter().map(|column| column.name.as_str()).collect();
        return Err(format!(
            "table {} column sequence drifted: {:?}",
            table.name, names
        ));
    }
    for column in actual {
        let expected = contains_name(table.not_null, &column.name);
        if column.not_null != expected {
            return Err(format!(
                "column {}.{} not-null is {} but declared {}",
                table.name, column.name, column.not_null, expected
            ));
        }
        let expected_key = contains_name(table.primary_key, &column.name);
        if (column.primary_key > 0) != expected_key {
            return Err(format!(
                "column {}.{} primary-key membership drifted",
                table.name, column.name
            ));
        }
    }
    Ok(())
}

/// 迁移输入判定：下界（必需列）与上界（认识列）同时成立。
///
/// 只判定「列在不在」，列序、`NOT NULL` 与主键归属都不参与：输入要重建的表在重建后由
/// [`CURRENT_TABLES`] 的 DDL 定形状，旧定义的那几项差别随旧表一起消失；即使某列的约束
/// 与当前形状不符、数据又满足不了新约束，重建当场就会失败回滚（两端都不会静默留下半套形状）。
pub(super) fn check_input(table: &InputTable, actual: &[ActualColumn]) -> Result<(), String> {
    if actual.is_empty() {
        if table.required_table {
            return Err(format!("table {} is missing", table.name));
        }
        return Ok(());
    }
    for required in table.required {
        if !actual
            .iter()
            .any(|column| same_name(&column.name, required))
        {
            return Err(format!("table {} lacks column {}", table.name, required));
        }
    }
    if let Some(allowed) = table.allowed {
        for column in actual {
            if !contains_name(allowed, &column.name) {
                return Err(format!(
                    "table {} has unknown column {}",
                    table.name, column.name
                ));
            }
        }
    }
    Ok(())
}

/// 完整当前形状：8 张表逐表严格判定（[`TableShape::optional`] 表缺表容忍，存在即逐项相符）。
pub(super) fn check_current_shape(
    tables: &HashMap<String, Vec<ActualColumn>>,
) -> Result<(), String> {
    for table in CURRENT_TABLES {
        let actual = tables.get(table.name).map(Vec::as_slice).unwrap_or(&[]);
        check_strict(table, actual)?;
    }
    Ok(())
}

/// 删除前的形状判定：缺表不算错（要不要删见 [`dropped_plan`]），存在即必须逐项相符。
///
/// 判定不过就拒绝迁移——这是**删除**动作的守卫：同名异形的表不是迁移认识的对象。
pub(super) fn check_dropped_shape(
    tables: &HashMap<String, Vec<ActualColumn>>,
) -> Result<(), String> {
    for table in DROPPED_TABLES {
        let actual = tables
            .get(table.shape.name)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if actual.is_empty() {
            continue;
        }
        check_strict(&table.shape, actual)?;
    }
    Ok(())
}

/// 删除计划：先判定形状（[`check_dropped_shape`]），再给出本库实际要下发的 `DROP TABLE`
/// 语句（按声明顺序；缺表且允许缺表时跳过）。
///
/// 判定与计划合成一个入口：调用方不可能拿到「没校验过的删除语句」。
pub(super) fn dropped_plan(
    tables: &HashMap<String, Vec<ActualColumn>>,
) -> Result<Vec<&'static str>, String> {
    check_dropped_shape(tables)?;
    Ok(DROPPED_TABLES
        .iter()
        .filter(|table| {
            table.always_dropped
                || tables
                    .get(table.shape.name)
                    .is_some_and(|columns| !columns.is_empty())
        })
        .map(|table| table.drop_sql)
        .collect())
}

/// 完整迁移输入形状：逐表上下界判定（缺表按各表契约处理）。
pub(super) fn check_input_shape(tables: &HashMap<String, Vec<ActualColumn>>) -> Result<(), String> {
    for table in INPUT_TABLES {
        let actual = tables.get(table.name).map(Vec::as_slice).unwrap_or(&[]);
        check_input(table, actual)?;
    }
    Ok(())
}

/// 按 `(表名, 列名, NOT NULL, 主键)` 的四列读取结果分组为逐表列序列。
///
/// 生产读取语句是[`CURRENT_COLUMNS_SQL`] / [`INPUT_COLUMNS_SQL`]（自带 `cid` 排序），
/// 远端执行器把读回的 `Value` 解成同样的四列后复用本函数。
pub(super) fn group_columns(
    rows: impl IntoIterator<Item = (String, String, bool, i64)>,
) -> HashMap<String, Vec<ActualColumn>> {
    let mut tables: HashMap<String, Vec<ActualColumn>> = HashMap::new();
    for (table, name, not_null, primary_key) in rows {
        tables
            .entry(table)
            .or_default()
            .push(ActualColumn::new(name, not_null, primary_key));
    }
    tables
}

/// 本机读取一条列形状语句（[`CURRENT_COLUMNS_SQL`] / [`INPUT_COLUMNS_SQL`]）。
#[cfg(not(target_os = "emscripten"))]
pub(super) async fn read_local_columns(
    connection: &mut sqlx::SqliteConnection,
    sql: &'static str,
) -> anyhow::Result<HashMap<String, Vec<ActualColumn>>> {
    let rows: Vec<(String, String, i64, i64, i64)> =
        sqlx::query_as(sql).fetch_all(&mut *connection).await?;
    Ok(group_columns(rows.into_iter().map(
        |(table, name, not_null, primary_key, _cid)| (table, name, not_null != 0, primary_key),
    )))
}

#[cfg(test)]
#[path = "schema_shape_test.rs"]
mod tests;
