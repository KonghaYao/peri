//! 远程会话层离线断言（不连网）：投影与下标一致、编解码往返、语句形状与绑定。
//!
//! 这里断言的是**离线可判定**的事实：投影文本与解码下标同源、非法形状被判损坏、
//! SQL 文本静态（两次不同输入得到同一文本、只有参数不同）、读语句只读。真实读写与
//! 重连读在 `cloud_session_test.rs`（默认 `#[ignore]`）。

use std::collections::HashMap;
use std::path::PathBuf;

use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionMetaPatch, SessionResourceErrorKind,
};
use peri_acp_types::store::{InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, CancelPolicy};
use peri_acp_types::workspace::{
    ProjectId, ScopedThreadQuery, SessionBinding, ThreadScope, WorkspaceId,
};
use turso_serverless::Value;

use super::session_codec as codec;
use super::session_sql::{
    self, FACT_BINDING_RELATIVE_CWD, FACT_BINDING_VERSION, PAGE_BINDING_VERSION,
};
use super::sql::StatementSpec;

// ─── 投影与下标同源 ───────────────────────────────────────────────────────────

/// 从投影文本抽出列名（content_size 子查询按一列计）。
fn projected_columns(projection: &str) -> Vec<String> {
    let mut columns = Vec::new();
    for line in projection.lines() {
        let line = line.trim();
        if line.starts_with("(SELECT") {
            columns.push("content_size".to_owned());
            continue;
        }
        for part in line.split(',') {
            let name = part
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(',');
            if !name.is_empty() {
                columns.push(name.to_owned());
            }
        }
    }
    columns
}

/// 事实行与列表行的后缀列（在 meta 投影之后的追加列）。
fn appended_columns(projection: &str) -> Vec<String> {
    projection[session_sql::META_PROJECTION.len()..]
        .split(',')
        .map(|part| part.split_whitespace().next().unwrap_or("").to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn meta_projection_matches_decoding_indices() {
    let columns = projected_columns(session_sql::META_PROJECTION);
    assert_eq!(
        columns,
        [
            "s.id",
            "s.title",
            "s.cwd",
            "s.created_at",
            "s.updated_at",
            "s.message_count",
            "content_size",
            "s.parent_thread_id",
            "s.snapshot_at_message_id",
            "s.hidden",
            "s.cancel_policy",
            "s.config",
            "s.agent_status",
        ]
    );
    assert_eq!(columns.len(), session_sql::META_COLUMN_COUNT);
}

#[test]
fn bigger_projections_extend_the_same_meta_source() {
    assert!(session_sql::FACT_PROJECTION.starts_with(session_sql::META_PROJECTION));
    assert!(session_sql::PAGE_PROJECTION.starts_with(session_sql::META_PROJECTION));
    assert_eq!(
        appended_columns(session_sql::FACT_PROJECTION),
        [
            "s.frozen_context",
            "s.inherited_context",
            "b.schema_version",
            "b.project_id",
            "b.workspace_id",
            "b.relative_cwd",
        ]
    );
    assert_eq!(
        appended_columns(session_sql::PAGE_PROJECTION),
        [
            "b.schema_version",
            "b.project_id",
            "b.workspace_id",
            "b.relative_cwd",
        ]
    );
    // 下标常量与投影顺序一致：绑定四列连续且起点相同。
    assert_eq!(session_sql::FACT_FROZEN, session_sql::META_COLUMN_COUNT);
    assert_eq!(session_sql::FACT_INHERITED, session_sql::FACT_FROZEN + 1);
    assert_eq!(FACT_BINDING_VERSION, session_sql::FACT_INHERITED + 1);
    assert_eq!(session_sql::FACT_BINDING_PROJECT, FACT_BINDING_VERSION + 1);
    assert_eq!(
        session_sql::FACT_BINDING_WORKSPACE,
        session_sql::FACT_BINDING_PROJECT + 1
    );
    assert_eq!(FACT_BINDING_RELATIVE_CWD, FACT_BINDING_VERSION + 3);
    assert_eq!(
        session_sql::FACT_COLUMN_TOTAL,
        FACT_BINDING_RELATIVE_CWD + 1
    );
    assert_eq!(PAGE_BINDING_VERSION, session_sql::META_COLUMN_COUNT);
    assert_eq!(session_sql::PAGE_BINDING_PROJECT, PAGE_BINDING_VERSION + 1);
    assert_eq!(
        session_sql::PAGE_BINDING_WORKSPACE,
        session_sql::PAGE_BINDING_PROJECT + 1
    );
    assert_eq!(
        session_sql::PAGE_BINDING_RELATIVE_CWD,
        session_sql::PAGE_BINDING_WORKSPACE + 1
    );
    assert_eq!(
        session_sql::FACT_COLUMN_TOTAL,
        session_sql::META_COLUMN_COUNT + 6
    );
    assert_eq!(
        session_sql::PAGE_COLUMN_TOTAL,
        session_sql::META_COLUMN_COUNT + 4
    );
}

#[test]
fn schema_ddl_matches_the_canonical_shape() {
    let plan = super::session_schema::initialization_plan();
    // 一条语句一个 spec：远端执行器的语句单元就是一条语句，多句拼一个请求只会执行第一条。
    // plan = canonical v2 建表段 + canonical 索引段 + control/work 机制表段。后 6 条不在
    // CREATE_V2_* 里，但新建（本函数）与迁移（schema_v14_upgrade / sqlite_store::schema）
    // 两条路径都会创建它们，因此逐条比对完整序列，而不是只比 canonical 的计数。
    let expected: Vec<&str> = crate::sessions::canonical::CREATE_V2_TABLES
        .iter()
        .chain(crate::sessions::canonical::CREATE_V2_INDEXES.iter())
        .copied()
        .chain([
            crate::sessions::control::CREATE_STATE,
            crate::sessions::control::CREATE_RECEIPTS,
        ])
        .chain(crate::sessions::work_store::schema::initialization_sql())
        .collect();
    let actual: Vec<&str> = plan.iter().map(|spec| spec.sql).collect();
    assert_eq!(
        actual, expected,
        "远端 DDL：canonical 建表段 + canonical 索引段 + control/work 机制表段"
    );
    for spec in &plan {
        assert!(
            spec.sql.contains("IF NOT EXISTS"),
            "初始化 DDL 必须幂等: {}",
            spec.sql
        );
        assert!(spec.params.is_empty(), "DDL 不带绑定参数");
        assert!(
            !spec.sql.contains(';'),
            "一个 spec 只能是一条语句: {}",
            spec.sql
        );
    }
    // 远端建的是本机那一份 canonical 表（清单来自同一处，不另抄一遍）。
    for table in [
        "machines",
        "workspaces",
        "threads",
        "messages",
        "projects",
        "legacy_execution_registrations",
        "session_bindings",
        "mcp_oauth_credentials",
    ] {
        assert!(
            plan.iter().any(|spec| spec.sql.contains(table)),
            "canonical 表必须有建表语句: {table}"
        );
    }
    // 索引段必须晚于建表（`idx_threads_updated` 引用建表时的列）：清单顺序就是执行顺序。
    let first_index = plan
        .iter()
        .position(|spec| spec.sql.starts_with("CREATE INDEX"))
        .expect("索引段存在");
    assert_eq!(
        first_index,
        crate::sessions::canonical::CREATE_V2_TABLES.len(),
        "建表段在前、索引段在后"
    );
    for index_sql in crate::sessions::canonical::CREATE_V2_INDEXES {
        assert!(
            plan.iter().any(|spec| spec.sql == *index_sql),
            "v2 canonical index missing: {index_sql}"
        );
    }
    // 会话表主键是会话 id（canonical 列名），历史表主键是消息 id（同一条消息不属于两个会话）。
    let sessions = plan
        .iter()
        .find(|spec| spec.sql.contains("CREATE TABLE IF NOT EXISTS threads"))
        .expect("会话表建表语句");
    assert!(sessions.sql.contains("id TEXT PRIMARY KEY"));
    let messages = plan
        .iter()
        .find(|spec| spec.sql.contains("CREATE TABLE IF NOT EXISTS messages"))
        .expect("历史表建表语句");
    assert!(messages.sql.contains("message_id TEXT PRIMARY KEY"));
    // 统一之后不再有远端自己的会话表名。
    for spec in &plan {
        assert!(!spec.sql.contains("peri_sessions"));
        assert!(!spec.sql.contains("peri_session_messages"));
    }
}

// ─── 解码 ─────────────────────────────────────────────────────────────────────

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// 平台绝对路径文本，用作「绑定相对路径必须是相对的」这条规则的反例。
///
/// 不能写死 `/etc`：Windows 上带根无盘符的路径不是绝对路径，规则不会拒绝它，
/// 断言就会把「没被拒绝」误报成契约失败。
fn absolute_cwd() -> &'static str {
    if cfg!(windows) {
        r"C:\etc"
    } else {
        "/etc"
    }
}

/// 一行会话事实（列顺序 = `FACT_PROJECTION`）。
fn fact_row() -> Vec<Value> {
    vec![
        text("session-1"),
        text("title"),
        text("/home/u/project"),
        text("2026-09-26T10:00:00+00:00"),
        text("2026-09-26T11:00:00+00:00"),
        Value::Integer(2),
        Value::Integer(42),
        Value::Null,
        Value::Null,
        Value::Integer(0),
        text("cascade"),
        Value::Null,
        text("active"),
        text("{\"frozen\":true}"),
        Value::Null,
        Value::Integer(1),
        text("11111111-1111-1111-1111-111111111111"),
        text("22222222-2222-2222-2222-222222222222"),
        text("sub"),
    ]
}

#[test]
fn meta_row_decodes_field_by_field() {
    let row = fact_row();
    let meta = codec::decode_meta(&row).expect("row is a valid meta row");
    assert_eq!(meta.id, "session-1");
    assert_eq!(meta.title.as_deref(), Some("title"));
    assert_eq!(meta.cwd, "/home/u/project");
    assert_eq!(meta.message_count, 2);
    assert_eq!(meta.content_size, 42);
    assert_eq!(meta.parent_thread_id, None);
    assert!(!meta.hidden);
    assert_eq!(meta.cancel_policy, CancelPolicy::Cascade);
    assert_eq!(meta.agent_status, AgentStatus::Active);
}

#[test]
fn binding_decodes_from_the_appended_columns() {
    let row = fact_row();
    let binding = codec::decode_binding(&row, FACT_BINDING_VERSION)
        .expect("binding columns are readable")
        .expect("binding is present");
    assert_eq!(
        binding.project_id,
        ProjectId::from_str_expect("11111111-1111-1111-1111-111111111111")
    );
    assert_eq!(binding.cwd_relative_to_workspace, PathBuf::from("sub"));
    assert_eq!(binding.schema_version, 1);
    assert_eq!(binding.revision, 1, "不可变绑定的协议字段恒为 1");

    // 四列全 NULL = 无绑定行（不是「有一半绑定」）。
    let mut absent = fact_row();
    for offset in 0..4 {
        absent[FACT_BINDING_VERSION + offset] = Value::Null;
    }
    assert!(codec::decode_binding(&absent, FACT_BINDING_VERSION)
        .expect("absent binding is not an error")
        .is_none());

    // 部分 NULL 或非法值 = 损坏，不猜。
    let mut partial = fact_row();
    partial[FACT_BINDING_VERSION] = Value::Null;
    assert!(matches!(
        codec::decode_binding(&partial, FACT_BINDING_VERSION),
        Err(error) if matches!(error.kind(), SessionResourceErrorKind::Corrupt { .. })
    ));

    let mut absolute = fact_row();
    absolute[FACT_BINDING_RELATIVE_CWD] = text(absolute_cwd());
    assert!(matches!(
        codec::decode_binding(&absolute, FACT_BINDING_VERSION),
        Err(error) if matches!(error.kind(), SessionResourceErrorKind::Corrupt { .. })
    ));
}

#[test]
fn broken_meta_cells_are_corrupt_not_defaulted() {
    let mut negative = fact_row();
    negative[session_sql::META_MESSAGE_COUNT] = Value::Integer(-1);
    assert!(matches!(
        codec::decode_meta(&negative),
        Err(error) if matches!(error.kind(), SessionResourceErrorKind::Corrupt { .. })
    ));

    let mut bad_policy = fact_row();
    bad_policy[session_sql::META_CANCEL_POLICY] = text("whatever");
    assert!(codec::decode_meta(&bad_policy).is_err());

    let mut bad_time = fact_row();
    bad_time[session_sql::META_UPDATED_AT] = text("yesterday");
    assert!(codec::decode_meta(&bad_time).is_err());

    let mut bad_flag = fact_row();
    bad_flag[session_sql::META_HIDDEN] = Value::Integer(7);
    assert!(codec::decode_meta(&bad_flag).is_err());
}

// ─── 历史行编解码 ─────────────────────────────────────────────────────────────

fn projection_directive() -> MessageProjectionDirective {
    MessageProjectionDirective {
        policy_version: 3,
        entries: vec![ProjectionActionEntry {
            message_id: MessageId::new(),
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactText { max_chars: 120 },
        }],
    }
}

#[test]
fn payload_row_round_trips_through_binding_parameters() {
    let payload = PersistedPayload::Message(BaseMessage::human("hello remote"));
    let flags = MessageFlags {
        truncated: true,
        excluded: false,
        projection: Some(projection_directive()),
    };
    let (reference, _) =
        crate::sessions::work_store::payload::prepare(&payload, "session-1").unwrap();
    let params =
        codec::payload_params("session-1", &payload, Some(&flags), &reference).expect("encodable");
    // 行形状：message_id, thread_id, role, content, truncated, excluded, projection
    assert_eq!(params.len(), 7);
    assert_eq!(params[2], Value::Text("user".to_owned()));
    let row = vec![
        params[0].clone(),
        params[3].clone(),
        params[4].clone(),
        params[5].clone(),
        params[6].clone(),
        params[2].clone(),
        Value::Integer(i64::from(reference.encoding)),
        Value::Integer(reference.byte_length as i64),
        text(&reference.sha256),
        Value::Blob(
            peri_acp_types::store::serialize_persisted_payload(&payload)
                .unwrap()
                .into_bytes(),
        ),
    ];
    let decoded = codec::decode_message_row(&row).expect("decodable");
    assert_eq!(decoded.id(), payload.id());
    match decoded {
        PersistedPayload::Message(BaseMessage::Human { content, .. }) => {
            assert_eq!(content, MessageContent::Text("hello remote".to_owned()));
        }
        other => panic!("unexpected payload shape: {other:?}"),
    }
    assert!(!codec::flags_are_default(
        &codec::decode_message_flags_row(&row).expect("flags decode")
    ));
    assert_eq!(
        codec::decode_message_flags_row(&row).expect("flags decode"),
        flags
    );
}

#[test]
fn history_row_id_must_match_its_payload() {
    let payload = PersistedPayload::Message(BaseMessage::human("mismatch"));
    let (reference, _) =
        crate::sessions::work_store::payload::prepare(&payload, "session-1").unwrap();
    let params = codec::payload_params("session-1", &payload, None, &reference).expect("encodable");
    let mut row = vec![
        text("33333333-3333-3333-3333-333333333333"),
        params[3].clone(),
        params[4].clone(),
        params[5].clone(),
        params[6].clone(),
        params[2].clone(),
        Value::Integer(i64::from(reference.encoding)),
        Value::Integer(reference.byte_length as i64),
        text(&reference.sha256),
        Value::Blob(
            peri_acp_types::store::serialize_persisted_payload(&payload)
                .unwrap()
                .into_bytes(),
        ),
    ];
    assert!(codec::decode_message_row(&row).is_err());
    row[0] = text(&payload.id().as_uuid().to_string());
    assert!(codec::decode_message_row(&row).is_ok());

    // 默认 flags 不进派生视图；非法 projection 是损坏。
    let flags_row = |truncated: Value, excluded: Value, projection: Value| {
        vec![
            row[0].clone(),
            row[1].clone(),
            truncated,
            excluded,
            projection,
        ]
    };
    let default_flags = codec::decode_message_flags_row(&flags_row(
        Value::Integer(0),
        Value::Integer(0),
        Value::Null,
    ))
    .expect("decodable");
    assert!(codec::flags_are_default(&default_flags));
    assert!(
        codec::decode_message_flags_row(&flags_row(
            Value::Integer(0),
            Value::Integer(0),
            text("{"),
        ))
        .is_err(),
        "非法 projection 必须报损坏"
    );
}

#[test]
fn inherited_context_round_trips_and_rejects_broken_references() {
    let payload = PersistedPayload::Message(BaseMessage::ai("inherited"));
    let mut flags = HashMap::new();
    flags.insert(
        payload.id(),
        MessageFlags {
            truncated: false,
            excluded: true,
            projection: None,
        },
    );
    let context = InheritedContext {
        payloads: vec![payload.clone()],
        flags,
    };
    let json = codec::inherited_json(&context).expect("encodable");
    let decoded = codec::decode_inherited(Some(&json)).expect("decodable");
    assert_eq!(decoded.payloads.len(), 1);
    assert_eq!(decoded.payloads[0].id(), payload.id());
    assert_eq!(decoded.flags.len(), 1);

    // 引用不存在的消息 id：发布前就被拒绝（与本地 adapter 同一规则）。
    let orphan = format!(
        "{{\"version\":1,\"payloads\":[\"{}\"],\"flags\":{{\"{}\":{{\"truncated\":false,\"excluded\":true}}}}}}",
        json_escape(&json),
        "44444444-4444-4444-4444-444444444444"
    );
    assert!(codec::decode_inherited(Some(&orphan)).is_err());
    assert!(codec::decode_inherited(None)
        .expect("absent is empty")
        .payloads
        .is_empty());
}

/// 最小 JSON 字符串转义：只处理引号与反斜杠（测试数据里没有别的特殊字符）。
fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

// ─── 语句形状 ─────────────────────────────────────────────────────────────────

fn binding(relative: &str) -> SessionBinding {
    SessionBinding {
        schema_version: 1,
        revision: 1,
        project_id: ProjectId::from_str_expect("11111111-1111-1111-1111-111111111111"),
        workspace_id: WorkspaceId::from_str_expect("22222222-2222-2222-2222-222222222222"),
        cwd_relative_to_workspace: PathBuf::from(relative),
    }
}

fn new_session(thread_id: &str, snapshot_at: Option<MessageId>) -> NewSession {
    NewSession {
        thread_id: thread_id.to_owned(),
        created_at: "2026-09-26T10:00:00+00:00".to_owned(),
        meta: NewSessionMeta {
            title: Some("t".to_owned()),
            cwd: "/home/u/project".to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: snapshot_at,
        },
        binding: binding("sub"),
        frozen: FrozenSnapshotBytes::new("{\"frozen\":true}"),
    }
}

#[tokio::test]
async fn write_sql_is_static_and_all_values_are_bound() {
    let first = new_session("session-a", None);
    let second = new_session("session-b", Some(MessageId::new()));
    let one = session_sql::insert_session_statements(&session_sql::session_insert(&first, 0, None))
        .expect("encodable");
    let two =
        session_sql::insert_session_statements(&session_sql::session_insert(&second, 3, None))
            .expect("encodable");
    assert_eq!(one.len(), 3);
    assert!(one[0].sql.contains("session_work_commands"));
    assert_eq!(one[0].params, vec![Value::Text("session-a".into())]);
    let one = &one[1..];
    let two = &two[1..];
    assert_eq!(one[0].sql, two[0].sql);
    assert_eq!(one[1].sql, two[1].sql);
    assert_ne!(one[0].params, two[0].params);
    assert!(one[0].sql.starts_with("INSERT INTO threads"));
    assert!(one[1].sql.starts_with("INSERT INTO session_bindings"));
    assert_eq!(one[0].params.len(), 14);
    assert_eq!(one[1].params.len(), 7);
    // 全部动态内容都出现在参数里，不出现在 SQL 文本里。
    for secret in ["session-a", "session-b", "/home/u/project"] {
        assert!(!one[0].sql.contains(secret));
        assert!(!one[1].sql.contains(secret));
    }
    // 会话 id 与绑定身份落到参数位置。
    assert_eq!(one[0].params[0], Value::Text("session-a".to_owned()));
    assert_eq!(one[1].params[0], Value::Text("session-a".to_owned()));
    // 绑定相对 cwd 是绑定语句的最后一位参数。
    assert_eq!(one[1].params[4], Value::Text("sub".to_owned()));
    // 创建路径 agent_status 起步为 active；快照截止点写成文本。
    assert_eq!(one[0].params[12], Value::Text("active".to_owned()));
    assert_eq!(one[0].params[7], Value::Null);
    assert_eq!(
        two[0].params[7],
        Value::Text(
            second
                .meta
                .snapshot_at_message_id
                .expect("present")
                .as_uuid()
                .to_string()
        )
    );
    // child 多一条继承区写入：与本机 child 路径同一句形态。
    let mut child_session = new_session("child", None);
    child_session.meta.parent_thread_id = Some(first.thread_id.clone());
    let child = session_sql::insert_session_statements(&session_sql::session_insert(
        &child_session,
        0,
        Some("{\"inherited\":true}"),
    ))
    .expect("encodable");
    assert_eq!(child.len(), 4);
    assert!(child[3]
        .sql
        .starts_with("UPDATE threads SET inherited_context"));
}

#[test]
fn binding_cwd_must_be_relative_and_textual() {
    let mut session = new_session("session-a", None);
    session.binding.cwd_relative_to_workspace = PathBuf::from(absolute_cwd());
    let error =
        session_sql::insert_session_statements(&session_sql::session_insert(&session, 0, None))
            .expect_err("absolute binding cwd is refused");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
}

#[tokio::test]
async fn draft_revocation_cleans_environment_without_cascade_and_preserves_committed_rows() {
    use sqlx::Connection;

    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    let foreign_keys: (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(foreign_keys.0, 0);
    for sql in crate::sessions::canonical::CREATE_TABLES {
        sqlx::query(*sql).execute(&mut connection).await.unwrap();
    }
    for (id, frozen, expected_deleted) in [("draft", None, 1), ("committed", Some("{}"), 0)] {
        sqlx::query("INSERT INTO threads(id, created_at, updated_at, frozen_context) VALUES (?1, 'now', 'now', ?2)")
            .bind(id).bind(frozen).execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO messages(message_id, thread_id, role, content_ref,transcript_seq) VALUES (?1, ?1, 'human', '{}',1)")
            .bind(id).execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id, relative_cwd) VALUES (?1, 1, 'project', 'workspace', '')")
            .bind(id).execute(&mut connection).await.unwrap();
        let statements = session_sql::revoke_draft_statements(id);
        assert_eq!(statements.len(), 3);
        assert!(statements[2].sql.starts_with("DELETE FROM threads"));
        let mut transaction = connection.begin().await.unwrap();
        for statement in &statements {
            assert_eq!(statement.params, vec![Value::Text(id.to_owned())]);
            assert!(statement.sql.contains("frozen_context IS NULL"));
            let result = sqlx::query(statement.sql)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
            assert_eq!(
                result.rows_affected(),
                expected_deleted,
                "{}",
                statement.sql
            );
        }
        transaction.commit().await.unwrap();
        for statement in &statements {
            let result = sqlx::query(statement.sql)
                .bind(id)
                .execute(&mut connection)
                .await
                .unwrap();
            assert_eq!(result.rows_affected(), 0);
        }
        assert!(statements
            .iter()
            .all(|statement| !statement.sql.contains("session_environments")));
    }
}

#[test]
fn read_statements_are_read_only_and_parameterized() {
    let single_id_reads = [
        session_sql::select_session_statement("s"),
        session_sql::select_meta_statement("s"),
        session_sql::select_children_statement("s"),
        session_sql::select_tree_statement("s"),
        session_sql::select_messages_statement("s"),
        session_sql::select_root_statement("s"),
    ];
    for spec in &single_id_reads {
        assert!(
            spec.is_read_only(),
            "read path must not write: {}",
            spec.sql
        );
        assert_eq!(spec.params.len(), 1, "单会话读取只绑定会话 id");
    }
    // 树查询是递归 CTE，不是「查一层再查一层」。
    assert!(single_id_reads[3].sql.starts_with("WITH RECURSIVE"));

    let page = session_sql::page_statement(&query(ThreadScope::All, None)).expect("bindable");
    assert!(page.is_read_only(), "分页列举不得写: {}", page.sql);
    assert_eq!(
        page.params.len(),
        8,
        "scope/游标/上限与归档状态都是绑定参数"
    );
}

#[test]
fn page_statement_binds_scope_cursor_and_limit() {
    let project = ProjectId::from_str_expect("11111111-1111-1111-1111-111111111111");
    let all = query(ThreadScope::All, None);
    let spec = session_sql::page_statement(&all).expect("bindable");
    assert_eq!(spec.params[0], Value::Integer(0));
    assert_eq!(spec.params[1], Value::Null);
    assert_eq!(spec.params[3], Value::Integer(0), "无游标时不带比较值");

    let scoped = query(ThreadScope::Project(project), None);
    let spec = session_sql::page_statement(&scoped).expect("bindable");
    assert_eq!(spec.params[0], Value::Integer(1));
    assert_eq!(spec.params[1], Value::Text(project.to_string()));

    let cursor = peri_acp_types::workspace::ThreadListCursor {
        updated_at: "2026-09-26T10:00:00+00:00".parse().expect("rfc3339"),
        thread_id: "session-9".to_owned(),
    };
    let paged = query(
        ThreadScope::ExactDirectory {
            workspace_id: WorkspaceId::from_str_expect("22222222-2222-2222-2222-222222222222"),
            relative_cwd: PathBuf::from("sub"),
        },
        Some(cursor),
    );
    let spec = session_sql::page_statement(&paged).expect("bindable");
    assert_eq!(spec.params[0], Value::Integer(3));
    assert_eq!(spec.params[2], Value::Text("sub".to_owned()));
    assert_eq!(spec.params[3], Value::Integer(1));
    assert_eq!(
        spec.params[4],
        Value::Text("2026-09-26T10:00:00+00:00".to_owned())
    );
    assert_eq!(spec.params[5], Value::Text("session-9".to_owned()));
    assert_eq!(
        spec.params[6],
        Value::Integer(26),
        "limit 25 多取一行判有无下一页"
    );

    // 上限被夹到 200：请求 1000 行时仍然只取 201 行。
    let mut oversized = query(ThreadScope::All, None);
    oversized.limit = 1000;
    let spec = session_sql::page_statement(&oversized).expect("bindable");
    assert_eq!(spec.params[6], Value::Integer(201));

    // 两次不同 scope 的 SQL 文本相同：过滤值全在参数里。
    assert_eq!(
        session_sql::page_statement(&all).expect("bindable").sql,
        session_sql::page_statement(&scoped).expect("bindable").sql
    );
}

#[test]
fn update_meta_distinguishes_keep_clear_and_set() {
    let patch = SessionMetaPatch {
        title: Some(None),
        status: Some(AgentStatus::Done),
        cancel_policy: None,
        config: Some(Some("{\"k\":1}".to_owned())),
    };
    let spec = session_sql::update_meta_statement("session-1", &patch, "2026-09-26T12:00:00+00:00");
    assert_eq!(spec.params[1], Value::Integer(1), "title 有意图");
    assert_eq!(spec.params[2], Value::Null, "Some(None) 是清除");
    assert_eq!(spec.params[3], Value::Integer(1));
    assert_eq!(spec.params[4], Value::Text("done".to_owned()));
    assert_eq!(spec.params[5], Value::Integer(0), "None 是保持不变");
    assert_eq!(spec.params[8], Value::Text("{\"k\":1}".to_owned()));
    assert_eq!(spec.params[9], Value::Text("session-1".to_owned()));

    let untouched = SessionMetaPatch::default();
    let spec = session_sql::update_meta_statement("session-1", &untouched, "now");
    assert_eq!(spec.params[1], Value::Integer(0));
    assert_eq!(spec.params[9], Value::Text("session-1".to_owned()));
}

fn query(
    scope: ThreadScope,
    cursor: Option<peri_acp_types::workspace::ThreadListCursor>,
) -> ScopedThreadQuery {
    ScopedThreadQuery {
        scope,
        cursor,
        limit: 25,
    }
}

/// `ProjectId` / `WorkspaceId` 的测试构造：解析失败即测试数据有误。
trait OpaqueIdExt: Sized {
    fn from_str_expect(value: &str) -> Self;
}

impl OpaqueIdExt for ProjectId {
    fn from_str_expect(value: &str) -> Self {
        use std::str::FromStr;
        Self::from_str(value).expect("test project id is a uuid")
    }
}

impl OpaqueIdExt for WorkspaceId {
    fn from_str_expect(value: &str) -> Self {
        use std::str::FromStr;
        Self::from_str(value).expect("test workspace id is a uuid")
    }
}

/// 语句形状断言不依赖客户端：`StatementSpec` 的 Debug 不打印绑定值。
#[test]
fn statement_debug_does_not_leak_bound_values() {
    let spec: StatementSpec = session_sql::select_meta_statement("session-secret");
    let rendered = format!("{spec:?}");
    assert!(!rendered.contains("session-secret"));
}
