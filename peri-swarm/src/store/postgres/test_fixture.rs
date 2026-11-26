//! Shared fixture for the PostgreSQL state store contract tests.
//!
//! One private schema per test on `PERI_TEST_POSTGRES_URL`; the DSN's `options`
//! parameter selects it through `search_path`, so the suite needs no
//! `CREATEDB` privilege and tests never see each other's tables. The sample
//! rows are the port's own types, so a round trip cannot drift from the port.

use std::collections::BTreeMap;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use url::Url;

use super::{SwarmStateStore, SwarmStateStoreConfig};
use crate::config::PostgresLocator;
use crate::domain::{
    EnvironmentRef, InputState, ItemKind, ItemSource, McpToolSpec, SessionState, TurnStatus,
};
use crate::store::{SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow, SwarmTurnRow};

pub(super) const TEST_POSTGRES_ENV: &str = "PERI_TEST_POSTGRES_URL";

// ── fixture ─────────────────────────────────────────────────────────────────

/// A private schema on the test server.
pub(super) struct TestSchema {
    schema: String,
    dsn: String,
}

impl TestSchema {
    pub(super) async fn create() -> Option<Self> {
        let base = match std::env::var(TEST_POSTGRES_ENV) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!(
                    "skipping the PostgreSQL state store contract test: set {TEST_POSTGRES_ENV} \
                     to a test database"
                );
                return None;
            }
        };
        let schema = format!("swarm_it_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect PERI_TEST_POSTGRES_URL");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA {}",
            quote(&schema)
        )))
        .execute(&admin)
        .await
        .expect("create the private test schema");
        admin.close().await;

        let mut url = Url::parse(&base).expect("valid test DSN");
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let fixture = Self {
            schema,
            dsn: url.to_string(),
        };
        // The DSN rewrite must actually select the private schema.
        let probe = fixture.pool().await;
        let current = current_schema(&probe).await;
        probe.close().await;
        assert_eq!(
            current, fixture.schema,
            "the test DSN must select {}",
            fixture.schema
        );
        Some(fixture)
    }

    pub(super) fn config(&self) -> SwarmStateStoreConfig {
        SwarmStateStoreConfig {
            locator: PostgresLocator::parse(&self.dsn, TEST_POSTGRES_ENV)
                .expect("the test DSN is a PostgreSQL DSN"),
        }
    }

    pub(super) async fn store(&self) -> SwarmStateStore {
        SwarmStateStore::open(&self.config())
            .await
            .expect("open the PostgreSQL state store")
    }

    pub(super) async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&self.dsn)
            .await
            .expect("probe pool")
    }

    pub(super) async fn execute(&self, sql: &str) {
        let pool = self.pool().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&pool)
            .await
            .expect("fixture statement");
        pool.close().await;
    }

    /// `(relkind, relname)` of every relation in this schema.
    pub(super) async fn relations(&self) -> Vec<(String, String)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT c.relkind::text, c.relname::text FROM pg_catalog.pg_class c
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 ORDER BY c.relkind, c.relname",
        )
        .bind(self.schema.clone())
        .fetch_all(&pool)
        .await
        .expect("catalog relations");
        pool.close().await;
        rows.iter()
            .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
            .collect()
    }

    pub(super) async fn meta(&self) -> Vec<(i64, String, String)> {
        let pool = self.pool().await;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT version, engine, contract FROM {}.swarm_meta",
            quote(&self.schema)
        )))
        .fetch_all(&pool)
        .await
        .expect("swarm_meta rows");
        pool.close().await;
        rows.iter()
            .map(|row| {
                (
                    row.get::<i64, _>(0),
                    row.get::<String, _>(1),
                    row.get::<String, _>(2),
                )
            })
            .collect()
    }

    pub(super) async fn count(&self, table: &str) -> i64 {
        let pool = self.pool().await;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {}.{}",
            quote(&self.schema),
            quote(table)
        )))
        .fetch_one(&pool)
        .await
        .expect("row count");
        pool.close().await;
        count
    }

    pub(super) async fn close(self) {
        let base = {
            let mut url = Url::parse(&self.dsn).expect("test DSN");
            url.set_query(None);
            url.to_string()
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("cleanup pool");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            quote(&self.schema)
        )))
        .execute(&admin)
        .await
        .expect("drop the private test schema");
        admin.close().await;
    }
}

/// `current_schema()` of this DSN: the schema the store writes to.
pub(super) async fn current_schema(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, String>("SELECT current_schema()::text")
        .fetch_one(pool)
        .await
        .expect("current_schema")
}

pub(super) fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(super) fn now() -> String {
    peri_time::now_utc_rfc3339()
}

pub(super) fn session(logical_id: &str, cwd: &str) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: logical_id.to_owned(),
        root_session_id: format!("root-{logical_id}"),
        actor_key: format!("agent:{logical_id}"),
        cwd: cwd.to_owned(),
        state: SessionState::Idle,
        model: Some("stub-model".to_owned()),
        instructions: Some("be concise".to_owned()),
        mcp_tools: vec![McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp")],
        environment: EnvironmentRef::none(),
        metadata: BTreeMap::from([("tenant".to_owned(), "acme".to_owned())]),
        prompt_cache_options: None,
        created_at: now(),
        last_activity_at: now(),
        last_item_seq: 0,
    }
}

pub(super) fn item(logical_id: &str, seq: u64, item_id: &str, text: &str) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: logical_id.to_owned(),
        seq,
        item_id: item_id.to_owned(),
        kind: ItemKind::Message,
        source: ItemSource::root(),
        payload: serde_json::json!({"text": text, "complete": true}),
        created_at: now(),
        truncated: false,
    }
}

pub(super) fn input(logical_id: &str, input_id: &str, fingerprint: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: logical_id.to_owned(),
        input_id: input_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        state: InputState::Accepted,
        created_at: now(),
    }
}

pub(super) fn binding(logical_env_id: &str, logical_id: &str) -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: logical_env_id.to_owned(),
        logical_id: logical_id.to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        workspace_path: "/workspace/cwd".to_owned(),
        sandbox_root: "/workspace".to_owned(),
        settings_profile: Some("sonnet".to_owned()),
        token_file: None,
        last_run_id: Some("run-1".to_owned()),
        last_used_at: now(),
    }
}

pub(super) fn turn(
    logical_id: &str,
    turn_id: &str,
    status: TurnStatus,
    created_at: i64,
) -> SwarmTurnRow {
    SwarmTurnRow {
        logical_id: logical_id.to_owned(),
        turn_id: turn_id.to_owned(),
        status,
        created_at,
        started_at: None,
        completed_at: None,
        error: None,
        usage: None,
        subagent_id: None,
    }
}
