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

/// v15 的 `projects`：唯一键收窄为 `UNIQUE(locator)`，不再有 `object_identity`。
const PROJECTS_COLUMNS: &[&str] = &["id", "locator"];
/// v15 的 `workspaces`：`identity` 退役后只剩归属键与发现证据。
const WORKSPACES_COLUMNS: &[&str] = &[
    "id",
    "machine_id",
    "path",
    "path_source",
    "project_id",
    "discovery",
];
/// 当前（v14）`threads` 列：v13 的 16 列 + 工作意图与上下文占用快照。
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
    "context_usage_tokens",
    "work_state",
];

/// v13 冻结的 `threads` 列（= v14 形状去掉两列工作快照）；v12 的 `threads` 形状与它相同。
const V13_THREADS_COLUMNS: &[&str] = &[
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

/// 当前（v14）的 `threads` 声明；`work_state` 非空，占用快照可空（NULL = 未知）。
const CURRENT_THREADS_TABLE: TableShape = TableShape {
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
        "work_state",
    ],
    primary_key: &["id"],
};
const MESSAGES_COLUMNS: &[&str] = &[
    "message_id",
    "thread_id",
    "role",
    "content",
    "truncated",
    "excluded",
    "projection",
];
/// v15 的 `session_bindings`：`evidence_origin` 退役，只剩绑定本身与最小执行根。
const BINDINGS_COLUMNS: &[&str] = &[
    "thread_id",
    "schema_version",
    "project_id",
    "workspace_id",
    "relative_cwd",
    "discovery_snapshot",
];
const OAUTH_COLUMNS: &[&str] = &[
    "principal_id",
    "workspace_id",
    "server_key",
    "credentials_blob",
    "updated_at",
];
const CLOSE_INTENTS_COLUMNS: &[&str] = &["thread_id", "requested_at"];
/// v14 冻结的 `projects` 列（含已退役的 `object_identity`）。
const V14_PROJECTS_COLUMNS: &[&str] = &["id", "locator", "object_identity"];
/// v14 冻结的 `workspaces` 列（含已退役的 `identity`）。
const V14_WORKSPACES_COLUMNS: &[&str] = &[
    "id",
    "machine_id",
    "path",
    "path_source",
    "project_id",
    "identity",
    "discovery",
];
/// v14 冻结的 `session_bindings` 列（含已退役的 `evidence_origin`）。
const V14_BINDINGS_COLUMNS: &[&str] = &[
    "thread_id",
    "schema_version",
    "project_id",
    "workspace_id",
    "relative_cwd",
    "discovery_snapshot",
    "evidence_origin",
];
/// v14 冻结的 inherited 派生索引列（该代表整体退役，只在形状判定里出现）。
const INHERITED_SNAPSHOT_INDEX_COLUMNS: &[&str] = &[
    "snapshot_id",
    "thread_id",
    "payload_bytes",
    "entry_count",
    "prepared_at",
];
const INHERITED_PAYLOAD_INDEX_COLUMNS: &[&str] = &[
    "snapshot_id",
    "ordinal",
    "entry_id",
    "raw_start",
    "raw_len",
    "logical_len",
];
/// Thread MQ 列（`docs/design/thread-mq.md` §3）：待发事实 + 派生的入队时间，没有状态列。
const THREAD_MQ_COLUMNS: &[&str] = &[
    "thread_id",
    "id",
    "enqueued_at",
    "kind",
    "source",
    "policy_json",
    "payload_json",
    "original_draft",
];

/// 上一代当前形状（v12）的逐表声明：本代（v15）当前形状去掉 `thread_mq` 与两列工作快照后的
/// **冻结快照**。v13 的 `threads` 形状与它相同，因此同一份声明同时服务 v12 与 v13 的
/// 形状判定。
///
/// 冻结而不是复用当前声明：v12 的库只有这些表，判定必须按代际比对，不能因为本代新增了
/// 一张表就把健康的 v12 库判成漂移（那会关掉升级路径）。顺序与
/// `canonical::CANONICAL_TABLES` 的父表在前顺序一致。
pub(super) const V12_TABLES: &[TableShape] = &[
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
        columns: V14_PROJECTS_COLUMNS,
        not_null: &["locator", "object_identity"],
        primary_key: &["id"],
    },
    TableShape {
        name: "workspaces",
        optional: false,
        columns: V14_WORKSPACES_COLUMNS,
        not_null: &["machine_id", "path", "path_source"],
        primary_key: &["id"],
    },
    TableShape {
        name: "threads",
        optional: false,
        columns: V13_THREADS_COLUMNS,
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
        columns: V14_BINDINGS_COLUMNS,
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
        // 本表所有已知列都是 NOT NULL：列清单与 NOT NULL 集合同一份声明。
        not_null: OAUTH_COLUMNS,
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
    TableShape {
        name: "inherited_snapshot_index",
        // 可选：派生索引。缺表只意味着读取回退全列扫描，不影响 inherited 事实本身；
        // v14 形状里写打开按 canonical DDL 幂等补齐，存在时必须同形。
        optional: true,
        columns: INHERITED_SNAPSHOT_INDEX_COLUMNS,
        not_null: &["thread_id", "payload_bytes", "entry_count", "prepared_at"],
        primary_key: &["snapshot_id"],
    },
    TableShape {
        name: "inherited_payload_index",
        // 可选：同 `inherited_snapshot_index`。`logical_len` 允许为 NULL（legacy 回填
        // 未解码正文，读取时按需解码）。
        optional: true,
        columns: INHERITED_PAYLOAD_INDEX_COLUMNS,
        not_null: &["snapshot_id", "ordinal", "entry_id", "raw_start", "raw_len"],
        primary_key: &["snapshot_id", "ordinal"],
    },
];

/// v15 的 `projects` 声明：唯一键收窄为 `UNIQUE(locator)`，`locator` 即项目身份。
const CURRENT_PROJECTS_TABLE: TableShape = TableShape {
    name: "projects",
    optional: false,
    columns: PROJECTS_COLUMNS,
    not_null: &["locator"],
    primary_key: &["id"],
};
/// v15 的 `workspaces` 声明：`identity` 退役，`discovery` 仍可空（历史惰性行）。
const CURRENT_WORKSPACES_TABLE: TableShape = TableShape {
    name: "workspaces",
    optional: false,
    columns: WORKSPACES_COLUMNS,
    not_null: &["machine_id", "path", "path_source"],
    primary_key: &["id"],
};
/// v15 的 `session_bindings` 声明：`evidence_origin` 退役。
const CURRENT_BINDINGS_TABLE: TableShape = TableShape {
    name: "session_bindings",
    optional: false,
    columns: BINDINGS_COLUMNS,
    not_null: &[
        "schema_version",
        "project_id",
        "workspace_id",
        "relative_cwd",
    ],
    primary_key: &["thread_id"],
};

/// Thread MQ：当前形状必需的表（缺失即不是本代形状，不得直接读写）。
///
/// 与「派生索引可选」不同：MQ 承载唯一一份待发事实，没有回退来源；缺表必须让升级路径
/// 建出来，而不是让仓库把它当成空队列。
pub(super) const THREAD_MQ_TABLE: TableShape = TableShape {
    name: "thread_mq",
    optional: false,
    columns: THREAD_MQ_COLUMNS,
    not_null: &[
        "thread_id",
        "id",
        "enqueued_at",
        "kind",
        "source",
        "policy_json",
        "payload_json",
    ],
    primary_key: &["thread_id", "id"],
};

/// 把声明里的 `threads` 换成给定形状，其余表原样保留。///
/// 各代之间只有 `threads` 在变长（v14 补两列工作快照），逐代另抄一份全表声明会让形状
/// 第二次各自维护；`TableShape` 没有 `Copy`，所以这里按引用替换。
fn with_threads<'a>(tables: Vec<&'a TableShape>, threads: &'a TableShape) -> Vec<&'a TableShape> {
    tables
        .into_iter()
        .map(|table| {
            if table.name == threads.name {
                threads
            } else {
                table
            }
        })
        .collect()
}

/// v14 冻结声明：v12 的全部表 + `thread_mq` + 带两列工作快照的 `threads`。
///
/// v13 与 v14 只差 `threads` 末尾两列，因此同一份声明也服务 v13（替换回
/// [`V13_THREADS_COLUMNS`]），见 [`check_v13_shape`]。
pub(super) fn v14_tables() -> Vec<&'static TableShape> {
    with_threads(
        V12_TABLES
            .iter()
            .chain(std::iter::once(&THREAD_MQ_TABLE))
            .collect(),
        &CURRENT_THREADS_TABLE,
    )
}

/// v13 冻结的 `threads` 声明（16 列，缺两列工作快照）。
const V13_THREADS_TABLE: TableShape = TableShape {
    name: "threads",
    optional: false,
    columns: V13_THREADS_COLUMNS,
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
};

/// 当前形状（v15）的逐表声明：v14 去掉两张 inherited 派生索引表，三张被重建的表换成
/// v15 形状（`projects` 无 `object_identity`、`workspaces` 无 `identity`、
/// `session_bindings` 无 `evidence_origin`）。
///
/// 用函数而不是 `const` 拼接：本代形状就是「上一代声明 − 退役表 + 本代改形的表」，另抄
/// 一份全表声明会让两份形状第二次各自维护。
pub(super) fn current_tables() -> Vec<&'static TableShape> {
    let v14 = v14_tables();
    v14.iter()
        .filter(|table| {
            table.name != "inherited_snapshot_index" && table.name != "inherited_payload_index"
        })
        .map(|table| match table.name {
            "projects" => &CURRENT_PROJECTS_TABLE,
            "workspaces" => &CURRENT_WORKSPACES_TABLE,
            "session_bindings" => &CURRENT_BINDINGS_TABLE,
            _ => *table,
        })
        .collect()
}

/// 当前形状（v15）的表名序列：`current_tables()`（九张表，两张 inherited 派生索引已退役）。
///
/// 远端打开按它逐表下发 `PRAGMA table_xinfo`：探测面与 [`check_current_shape`] 的判定面
/// 必须逐表一致，否则会出现「读了却没判」的表。
pub(super) fn current_table_names() -> impl Iterator<Item = &'static str> {
    current_tables().into_iter().map(|table| table.name)
}

/// 压缩前形状的 `threads` 列（14 列；当前形状的 `workspace_id` / `archived` 与两列工作
/// 快照都不在其中）。
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

/// 迁移输入 `workspaces` 的已知列：下界与上界相同（重建后由当前 DDL 定形）。
const INPUT_WORKSPACE_COLUMNS: &[&str] =
    &["id", "project_id", "root", "root_identity", "discovery"];

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
        // 上界 = 14 列下界 ∪ 正式发布里退役的两个缓存列（`removal_plan` 在搬运开始前显式
        // 删除它们，因此允许出现；重建只搬运当前列，除此之外的列没有去处）。
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
        required: INPUT_WORKSPACE_COLUMNS,
        allowed: Some(INPUT_WORKSPACE_COLUMNS),
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
    DroppedTable {
        shape: TableShape {
            name: "inherited_payload_index",
            optional: true,
            columns: INHERITED_PAYLOAD_INDEX_COLUMNS,
            not_null: &["snapshot_id", "ordinal", "entry_id", "raw_start", "raw_len"],
            primary_key: &["snapshot_id", "ordinal"],
        },
        // 只是派生索引：内容可从 `threads.inherited_context` 重建。缺表（远端从未建过）跳过；
        // 同名异形仍由 `check_dropped_shape` 拦下——迁移不认识的表不删。
        always_dropped: false,
        drop_sql: "DROP TABLE inherited_payload_index",
    },
    DroppedTable {
        shape: TableShape {
            name: "inherited_snapshot_index",
            optional: true,
            columns: INHERITED_SNAPSHOT_INDEX_COLUMNS,
            not_null: &["thread_id", "payload_bytes", "entry_count", "prepared_at"],
            primary_key: &["snapshot_id"],
        },
        // 顺序在 payload 之后：payload 行经 `snapshot_id` 关联它。
        always_dropped: false,
        drop_sql: "DROP TABLE inherited_snapshot_index",
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
    UNION ALL SELECT 'thread_mq', name, \"notnull\", pk, cid FROM pragma_table_xinfo('thread_mq')
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

/// 完整上一代当前形状（v12，10 张表）的判定：只用于把 v11/v12 的库认成升级来源。
pub(super) fn check_v12_shape(tables: &HashMap<String, Vec<ActualColumn>>) -> Result<(), String> {
    for table in V12_TABLES {
        let actual = tables.get(table.name).map(Vec::as_slice).unwrap_or(&[]);
        check_strict(table, actual)?;
    }
    Ok(())
}

/// 完整上一代当前形状（v14，11 张表）的判定：只用于把 v14 的库认成升级来源。
///
/// v14 与 v13 的差别只有 `threads` 末尾两列，与 v15 的差别是三列退役加两张表退役——
/// 判定链因此必须按「v14 → v13 → v12」逐级退让，任一成立即进入对应的升级路径。
pub(super) fn check_v14_shape(tables: &HashMap<String, Vec<ActualColumn>>) -> Result<(), String> {
    for table in v14_tables() {
        let actual = tables.get(table.name).map(Vec::as_slice).unwrap_or(&[]);
        check_strict(table, actual)?;
    }
    Ok(())
}

/// 上一代当前形状（v13）的判定：v14 的表集合、`threads` 换回 16 列后逐表严格判定。
///
/// v13 是 v12 的升级终点里「只差两列」的那一档：同一份读回结果既能回答「这是不是 v13」，
/// 也能回答「这是不是 v12」——两者的差别是 `thread_mq`，与 v14 的差别只是 `threads` 末尾
/// 两列。
pub(super) fn check_v13_shape(tables: &HashMap<String, Vec<ActualColumn>>) -> Result<(), String> {
    for table in v14_tables() {
        let threads: &TableShape = if table.name == "threads" {
            &V13_THREADS_TABLE
        } else {
            table
        };
        let actual = tables.get(threads.name).map(Vec::as_slice).unwrap_or(&[]);
        check_strict(threads, actual)?;
    }
    Ok(())
}

/// 完整当前形状（v15）：`current_tables()` 逐表严格判定。
pub(super) fn check_current_shape(
    tables: &HashMap<String, Vec<ActualColumn>>,
) -> Result<(), String> {
    for table in current_tables() {
        let actual = tables.get(table.name).map(Vec::as_slice).unwrap_or(&[]);
        check_strict(table, actual)?;
    }
    Ok(())
}

/// 读回**要删除**对象的列形状（v14 冻结声明里的两张 inherited 派生索引表 + 压缩前对象）。
///
/// 这些表既不在当前形状的读取面里（v15 已退役），也可能不在迁移输入的读取面里（远端
/// 从未建过），因此单独一条只读语句；调用方把它并入形状映射后交给 [`check_dropped_shape`]。
pub(super) const DROPPED_COLUMNS_SQL: &str =
    "SELECT 'inherited_snapshot_index' AS t, name, \"notnull\", pk, cid FROM pragma_table_xinfo('inherited_snapshot_index')
    UNION ALL SELECT 'inherited_payload_index', name, \"notnull\", pk, cid FROM pragma_table_xinfo('inherited_payload_index')
    UNION ALL SELECT 'session_environments', name, \"notnull\", pk, cid FROM pragma_table_xinfo('session_environments')
    UNION ALL SELECT 'mcp_oauth_credentials', name, \"notnull\", pk, cid FROM pragma_table_xinfo('mcp_oauth_credentials')
    ORDER BY 1, 5";

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
