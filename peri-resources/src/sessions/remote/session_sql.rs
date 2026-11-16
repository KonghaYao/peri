//! 远程会话语句：静态 SQL 文本 + 全绑定参数，**表名与列名就是本机形状**。
//!
//! 统一之后远端不再有自己的会话表：语句里的 `threads` / `messages` / `session_bindings`
//! 与 `sqlite_store` 说的是同一份 canonical schema（`sessions::canonical` 是那份 DDL 的
//! 唯一来源）。列语义、`messages.rowid` 承载的 canonical 历史顺序、绑定四列所在的表都逐项
//! 对应；远端独有的只剩执行器机制（托管批与幂等账本），不体现在会话形状上。
//!
//! 规则与 [`super::sql`] 一致，只是范围更大：SQL 文本在编译期定型（`concat!` + `const`），
//! 会话 id、绑定、时间戳、分页游标、scope 过滤值全部走绑定参数。列投影只有一处来源
//! （宏 `meta_columns!`），事实投影与前缀投影由它拼出，避免「同一列清单手抄三遍」漂移；
//! 旧代只读库（schema ≤ 13）用同形占位列的 `meta_columns_legacy!`，解码仍是同一套。
//!
//! 解码用的列下标与投影同处一个模块，且由最后一个下标推出（[`reads::META_COLUMN_COUNT`]），
//! 测试拿真实列文本核对前缀关系。

mod reads;
mod writes;

pub(super) use reads::{
    active_entry_positions_statement, history_page_body_statement, history_page_bounds_statement,
    history_page_rows_statement, history_segment_anchor_statement, history_segment_head_statement,
    history_segment_next_statement, history_segment_tail_statement, inherited_context_statement,
    page_statement_for_archive, page_statement_v11, rewind_candidate_body_statement,
    rewind_candidate_rows_statement, select_active_messages_statement, select_children_statement,
    select_frozen_statement, select_interrupted_tree_statement, select_message_flags_statement,
    select_messages_statement, select_meta_statement, select_root_statement,
    select_session_statement, select_tree_statement, select_working_tree_statement, MetaProjection,
    FACT_BINDING_VERSION, FACT_FROZEN, FACT_INHERITED, META_AGENT_STATUS, META_CANCEL_POLICY,
    META_CONFIG, META_CONTENT_SIZE, META_CONTEXT_USAGE_TOKENS, META_CREATED_AT, META_CWD,
    META_HIDDEN, META_ID, META_MESSAGE_COUNT, META_PARENT, META_SNAPSHOT_AT, META_TITLE,
    META_UPDATED_AT, META_WORK_STATE, PAGE_BINDING_VERSION,
};
pub(super) use writes::{
    commit_frozen_statement, context_usage_statement, insert_message_statement,
    insert_session_draft_statements, insert_session_statements, revoke_draft_statements,
    revoke_session_statements, session_insert, update_meta_statement, work_state_statement,
    SessionInsert,
};

#[cfg(test)]
pub(super) use reads::{
    page_statement, FACT_BINDING_PROJECT, FACT_BINDING_RELATIVE_CWD, FACT_BINDING_WORKSPACE,
    FACT_COLUMN_TOTAL, FACT_PROJECTION, FACT_PROJECTION_LEGACY, META_COLUMN_COUNT, META_PROJECTION,
    META_PROJECTION_LEGACY, PAGE_BINDING_PROJECT, PAGE_BINDING_RELATIVE_CWD,
    PAGE_BINDING_WORKSPACE, PAGE_COLUMN_TOTAL, PAGE_PROJECTION, PAGE_PROJECTION_LEGACY,
};
#[cfg(test)]
pub(super) use writes::{
    DELETE_SESSION_BINDINGS_SQL, DELETE_SESSION_MESSAGES_SQL, DELETE_SESSION_SQL,
};

#[cfg(test)]
mod tests {
    use super::*;
    use turso_serverless::Value;

    /// 远端两条撤销入口的判据点：write-once（fork 补偿）不叠加 frozen 判据，
    /// 两阶段草稿的三条删除全部带 `frozen_context IS NULL`。
    #[test]
    fn revocation_statements_split_by_entry_point() {
        // 两阶段草稿：判据在每一条删除上（同一批内不会出现「历史删了、会话行还在」）。
        let draft = revoke_draft_statements("s");
        assert_eq!(draft.len(), 3);
        for statement in &draft {
            assert!(
                statement.sql.contains("frozen_context IS NULL"),
                "草稿撤销的每条删除都必须带未提交判据: {}",
                statement.sql
            );
            assert!(
                statement.sql.starts_with("DELETE"),
                "只允许删除语句: {}",
                statement.sql
            );
        }
        // write-once：撤销未发布创建（fork 等）不叠加 frozen 判据——目标创建即带 frozen，
        // 补偿就是把它整条删掉。
        let write_once = revoke_session_statements("s");
        assert_eq!(write_once.len(), 3);
        for statement in &write_once {
            assert!(
                !statement.sql.contains("frozen_context"),
                "write-once 撤销不得叠加 frozen 判据: {}",
                statement.sql
            );
        }
    }

    /// inherited 派生索引随 v15 退役：撤销批次只剩 canonical 删除，线程删除仍在末尾。
    #[test]
    fn revoke_batches_end_with_the_thread_row_deletion() {
        let effects = revoke_session_statements("s");
        assert_eq!(effects.len(), 3);
        assert_eq!(effects.last().unwrap().sql, DELETE_SESSION_SQL);

        let draft = revoke_draft_statements("s");
        for statement in &draft[..2] {
            assert!(
                statement.sql.contains("frozen_context IS NULL"),
                "草稿撤销必须与线程删除共用未提交判据: {}",
                statement.sql
            );
        }
        assert!(
            draft.last().unwrap().sql.starts_with("DELETE FROM threads"),
            "线程删除必须留在批次末尾（撤销流程按 counts.last() 判定）"
        );
        let selected_entries = effects.iter().chain(draft.iter());
        for statement in selected_entries {
            assert!(
                !statement.sql.contains("inherited_"),
                "v15 起撤销批次不得引用已退役的派生索引: {}",
                statement.sql
            );
        }
    }

    /// 回退候选语句形状：阶段一 user 过滤 + keyset + LIMIT，阶段二按主键单行；
    /// 参数全部走绑定（静态 SQL 不插值）。
    #[test]
    fn rewind_candidate_statements_are_user_filtered_bounded_and_parameterized() {
        let first = rewind_candidate_rows_statement("s", None, 128);
        assert!(first.is_read_only(), "阶段一必须是只读语句");
        assert!(first.sql.contains("m.role = 'user'"));
        assert!(first.sql.contains("ORDER BY m.rowid DESC LIMIT ?3"));
        assert!(!first.sql.contains("excluded"), "归档 user 仍是合法目标");
        assert_eq!(
            first.params,
            vec![
                Value::Text("s".to_owned()),
                Value::Null,
                Value::Integer(128)
            ]
        );

        let next = rewind_candidate_rows_statement("s", Some(42), 128);
        assert_eq!(
            next.params,
            vec![
                Value::Text("s".to_owned()),
                Value::Integer(42),
                Value::Integer(128)
            ],
            "keyset 游标只走绑定参数"
        );

        let body = rewind_candidate_body_statement("s", "m-1");
        assert!(body.is_read_only());
        assert!(body.sql.contains("m.message_id = ?2"));
        assert_eq!(
            body.params,
            vec![Value::Text("s".to_owned()), Value::Text("m-1".to_owned())]
        );
    }
}
