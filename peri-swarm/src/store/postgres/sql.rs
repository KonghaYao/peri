//! Statements of the Swarm state database.
//!
//! Every statement is static with `$n` binding parameters; dynamic values (ids,
//! timestamps, JSON text) only travel as bound parameters. `$n` covers exactly
//! `1..=bind_count` for every statement — [`super::sql_test`] pins that, so a
//! rewrite cannot silently leave a hole.
//!
//! Two SQLite-isms of the Loom-era adapter are gone:
//!
//! - `changes()` sequencing is replaced by an explicit transaction check
//!   (`create_hosted_session` inspects `rows_affected` of the guarded binding
//!   update before inserting the session);
//! - `json_extract` / `json_each` are `jsonb` operators on the stored TEXT
//!   columns (`metadata_json`, `environment_json`), which keeps the stored
//!   bytes unchanged.

use crate::domain::ItemOrder;

/// Same identity rule as the in-memory fake: only `root_session_id` and `cwd`
/// may repeat for one `logical_id`, otherwise the update affects 0 rows and the
/// caller sees a conflict. A second `logical_id` claiming the same
/// `root_session_id` fails on the unique constraint, also a conflict.
pub(super) const UPSERT_SESSION_SQL: &str = "INSERT INTO swarm_sessions (
    logical_id, root_session_id, actor_key, cwd, state, model, instructions,
    mcp_json, environment_json, metadata_json, prompt_cache_options_json,
    created_at, last_activity_at, last_item_seq)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
ON CONFLICT (logical_id) DO UPDATE SET
    actor_key = excluded.actor_key,
    state = excluded.state,
    model = excluded.model,
    instructions = excluded.instructions,
    mcp_json = excluded.mcp_json,
    environment_json = excluded.environment_json,
    metadata_json = excluded.metadata_json,
    prompt_cache_options_json = excluded.prompt_cache_options_json,
    last_activity_at = excluded.last_activity_at,
    last_item_seq = excluded.last_item_seq
WHERE swarm_sessions.root_session_id = excluded.root_session_id
  AND swarm_sessions.cwd = excluded.cwd";

/// Unconditional insert of a brand-new hosted session identity.
pub(super) const INSERT_SESSION_SQL: &str = "INSERT INTO swarm_sessions (
    logical_id, root_session_id, actor_key, cwd, state, model, instructions,
    mcp_json, environment_json, metadata_json, prompt_cache_options_json,
    created_at, last_activity_at, last_item_seq)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)";

pub(super) const SELECT_SESSION_SQL: &str =
    "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions WHERE logical_id = $1";

pub(super) const UPDATE_SESSION_SQL: &str = "UPDATE swarm_sessions
    SET state = $2, last_item_seq = $3, last_activity_at = $4 WHERE logical_id = $1";

/// `sessions.update`. Each boolean decides whether the paired value replaces
/// the stored one; the two fields whose official spelling can also "clear"
/// bind `NULL` with the boolean set.
pub(super) const PATCH_SESSION_SQL: &str = "UPDATE swarm_sessions SET
    model = CASE WHEN $2 THEN $3 ELSE model END,
    mcp_json = CASE WHEN $4 THEN $5 ELSE mcp_json END,
    metadata_json = CASE WHEN $6 THEN $7 ELSE metadata_json END,
    prompt_cache_options_json = CASE WHEN $8 THEN $9 ELSE prompt_cache_options_json END,
    last_activity_at = CASE WHEN $10 THEN $11 ELSE last_activity_at END
    WHERE logical_id = $1";

/// `sessions.list` base statement; metadata predicates are inserted by
/// [`session_list_sql`]. `$1` is the exclusive id cursor (`''` = none), `$2` is
/// `limit + 1`.
const SESSION_LIST_ASC_PREFIX: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions s
    WHERE ($1 = '' OR s.logical_id > $1)";

const SESSION_LIST_DESC_PREFIX: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions s
    WHERE ($1 = '' OR s.logical_id < $1)";

const SESSION_LIST_ASC_SUFFIX: &str = " ORDER BY s.logical_id ASC LIMIT $2";
const SESSION_LIST_DESC_SUFFIX: &str = " ORDER BY s.logical_id DESC LIMIT $2";

/// Environment id of one session, read before the binding lock. `NULL` when the
/// session does not exist or its environment carries no id (for example `none`).
pub(super) const SELECT_SESSION_ENV_ID_SQL: &str =
    "SELECT environment_json::jsonb ->> 'id' FROM swarm_sessions WHERE logical_id = $1";

/// Transaction-scoped row lock on one environment binding.
///
/// Two sessions of one hosted environment must not hand the binding to a
/// session the other transaction is deleting, so every delete of such a session
/// takes this lock first; the second deleter blocks and then re-reads under
/// READ COMMITTED.
pub(super) const LOCK_BINDING_SQL: &str =
    "SELECT 1 FROM swarm_bindings WHERE logical_env_id = $1 FOR UPDATE";

/// Remove a binding of this environment whose owner session no longer exists.
///
/// The hand-over above keeps the binding on a live session; a binding left
/// pointing at a deleted session (out-of-band `DELETE`, restored dump) would
/// otherwise survive forever. Scoped to the environment the delete already
/// locked.
pub(super) const DELETE_DANGLING_BINDING_SQL: &str = "DELETE FROM swarm_bindings b
      WHERE b.logical_env_id = $1
        AND NOT EXISTS (SELECT 1 FROM swarm_sessions s WHERE s.logical_id = b.logical_id)";

pub(super) const DELETE_SESSION_SQL: &str = "DELETE FROM swarm_sessions WHERE logical_id = $1";
pub(super) const DELETE_ITEMS_SQL: &str = "DELETE FROM swarm_items WHERE logical_id = $1";
pub(super) const DELETE_INPUTS_SQL: &str = "DELETE FROM swarm_inputs WHERE logical_id = $1";
pub(super) const DELETE_TURNS_SQL: &str = "DELETE FROM swarm_turns WHERE logical_id = $1";

/// The binding records the most recent session to attach. Before deleting that
/// session, give another user of the same environment the binding in this
/// transaction. `environment_json` is TEXT this build wrote; a corrupt value
/// fails the delete visibly instead of silently keeping a stale binding.
pub(super) const REASSIGN_BINDINGS_SQL: &str = "UPDATE swarm_bindings SET logical_id = COALESCE((
    SELECT s.logical_id FROM swarm_sessions s
    WHERE s.logical_id <> $1
      AND (s.environment_json::jsonb ->> 'type') = 'openai_hosted'
      AND (s.environment_json::jsonb ->> 'id') = swarm_bindings.logical_env_id
    ORDER BY s.logical_id LIMIT 1
), logical_id) WHERE logical_id = $1";

pub(super) const DELETE_BINDINGS_SQL: &str = "DELETE FROM swarm_bindings WHERE logical_id = $1";

pub(super) const UPSERT_TURN_SQL: &str = "INSERT INTO swarm_turns
    (logical_id, turn_id, status, created_at, started_at, completed_at,
     error_json, usage_json, subagent_id)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
    ON CONFLICT (logical_id, turn_id) DO UPDATE SET
        status = excluded.status,
        started_at = excluded.started_at,
        completed_at = excluded.completed_at,
        error_json = excluded.error_json,
        usage_json = excluded.usage_json,
        subagent_id = excluded.subagent_id";

pub(super) const SELECT_TURN_SQL: &str =
    "SELECT logical_id, turn_id, status, created_at, started_at,
    completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = $1 AND turn_id = $2";

pub(super) const SELECT_TURNS_ASC_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = $1
    ORDER BY created_at ASC, turn_id ASC LIMIT $2";

pub(super) const SELECT_TURNS_ASC_AFTER_SQL: &str =
    "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = $1 AND
    (created_at > $2 OR (created_at = $2 AND turn_id > $3))
    ORDER BY created_at ASC, turn_id ASC LIMIT $4";

pub(super) const SELECT_TURNS_DESC_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = $1
    ORDER BY created_at DESC, turn_id DESC LIMIT $2";

pub(super) const SELECT_TURNS_DESC_AFTER_SQL: &str =
    "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = $1 AND
    (created_at < $2 OR (created_at = $2 AND turn_id < $3))
    ORDER BY created_at DESC, turn_id DESC LIMIT $4";

pub(super) const CLOSE_OPEN_TURNS_SQL: &str = "UPDATE swarm_turns
    SET status = $2, completed_at = $3, error_json = $4
    WHERE logical_id = $1 AND status IN ('queued', 'in_progress', 'waiting')";

/// Crash-recovery probe: does the session still own a non-terminal turn row?
/// Predicate matches [`crate::domain::TurnStatus::is_open`].
pub(super) const HAS_OPEN_TURN_SQL: &str = "SELECT EXISTS(SELECT 1 FROM swarm_turns
    WHERE logical_id = $1 AND status IN ('queued', 'in_progress', 'waiting'))";

pub(super) const INSERT_ITEM_SQL: &str = "INSERT INTO swarm_items
    (logical_id, seq, item_id, kind, source, payload_json, created_at, truncated)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

pub(super) const SELECT_ITEMS_AFTER_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = $1 AND seq > $2 ORDER BY seq ASC LIMIT $3";

pub(super) const SELECT_ITEMS_BEFORE_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = $1 AND seq < $2 ORDER BY seq DESC LIMIT $3";

pub(super) const SELECT_ITEMS_TAIL_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = $1 ORDER BY seq DESC LIMIT $2";

pub(super) const SELECT_ITEM_BY_ID_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = $1 AND item_id = $2 ORDER BY seq DESC LIMIT 1";

// Item-grouped pagination (`design §4.2`). `page` selects items by their
// greatest `seq` — never by rows — so a cursor resolved from an item id cannot
// land inside the same item and repeat it. The outer query then returns every
// row of the selected items; the caller derives `data` from each item's last
// row. The join carries `logical_id` so a caller-supplied input id that repeats
// across sessions can never pull another session's rows.
pub(super) const SELECT_ITEM_PAGE_ASC_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = $1 GROUP BY logical_id, item_id HAVING MAX(seq) > $2
    ORDER BY last_seq ASC LIMIT $3)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq ASC";

pub(super) const SELECT_ITEM_PAGE_DESC_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = $1 GROUP BY logical_id, item_id HAVING MAX(seq) < $2
    ORDER BY last_seq DESC LIMIT $3)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq DESC";

pub(super) const SELECT_ITEM_PAGE_TAIL_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = $1 GROUP BY logical_id, item_id
    ORDER BY last_seq DESC LIMIT $2)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq DESC";

pub(super) const SELECT_LAST_SEQ_SQL: &str =
    "SELECT COALESCE(MAX(seq), 0) FROM swarm_items WHERE logical_id = $1";

pub(super) const INSERT_INPUT_SQL: &str = "INSERT INTO swarm_inputs
    (logical_id, input_id, fingerprint, state, created_at)
    VALUES ($1, $2, $3, $4, $5)
    ON CONFLICT (logical_id, input_id) DO NOTHING";

pub(super) const SELECT_INPUT_SQL: &str =
    "SELECT logical_id, input_id, fingerprint, state, created_at
    FROM swarm_inputs WHERE logical_id = $1 AND input_id = $2";

pub(super) const UPSERT_BINDING_SQL: &str = "INSERT INTO swarm_bindings
    (logical_env_id, logical_id, sandbox_id, workspace_path, sandbox_root,
     settings_profile, token_file, last_run_id, last_used_at)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
    ON CONFLICT (logical_env_id) DO UPDATE SET
        logical_id = excluded.logical_id,
        sandbox_id = excluded.sandbox_id,
        workspace_path = excluded.workspace_path,
        sandbox_root = excluded.sandbox_root,
        settings_profile = excluded.settings_profile,
        token_file = excluded.token_file,
        last_run_id = excluded.last_run_id,
        last_used_at = excluded.last_used_at";

/// Attach an existing environment: only a binding whose sandbox and workspace
/// still match is handed to the new session. 0 rows means "not ours" (the
/// caller reports `NotFound` and changes nothing).
pub(super) const ATTACH_BINDING_SQL: &str = "UPDATE swarm_bindings
    SET logical_id = $2, last_used_at = $6, last_run_id = COALESCE($5, last_run_id)
    WHERE logical_env_id = $1 AND sandbox_id = $3 AND workspace_path = $4";

pub(super) const SELECT_BINDING_SQL: &str =
    "SELECT logical_env_id, logical_id, sandbox_id, workspace_path,
    sandbox_root, settings_profile, token_file, last_run_id, last_used_at
    FROM swarm_bindings WHERE logical_env_id = $1";

/// `sessions.list` with one metadata predicate per `(key, value)` pair.
///
/// `jsonb_each_text` compares the stored key and value as text, so no caller
/// string becomes SQL or a JSON path. A `NULL` `metadata_json` yields no rows.
pub(super) fn session_list_sql(order: ItemOrder, filters: usize) -> String {
    let (prefix, suffix) = match order {
        ItemOrder::Asc => (SESSION_LIST_ASC_PREFIX, SESSION_LIST_ASC_SUFFIX),
        ItemOrder::Desc => (SESSION_LIST_DESC_PREFIX, SESSION_LIST_DESC_SUFFIX),
    };
    let mut sql = String::from(prefix);
    for index in 0..filters {
        // `$1`/`$2` are the cursor and limit; each metadata pair binds two
        // parameters.
        let key_param = 3 + index * 2;
        let value_param = key_param + 1;
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM jsonb_each_text(s.metadata_json::jsonb) m(key, value) \
             WHERE m.key = ${key_param} AND m.value = ${value_param})"
        ));
    }
    sql.push_str(suffix);
    sql
}

#[cfg(test)]
#[path = "sql_test.rs"]
mod tests;
