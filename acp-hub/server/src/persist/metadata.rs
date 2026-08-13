//! SQLite navigation catalog for project → logical session metadata.
//!
//! This store is deliberately separate from the existing Yjs/outbox file logs.
//! SQLite is authoritative for navigation metadata; Registry Doc maps are a
//! rebuildable projection owned by `ProjectService`.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

pub const METADATA_DB_FILE: &str = "metadata.sqlite3";
const SCHEMA_VERSION: i64 = 2;

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations(
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projects(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  cwd TEXT NOT NULL,
  instance_id TEXT NOT NULL DEFAULT 'local',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT
);
CREATE INDEX IF NOT EXISTS projects_updated_idx ON projects(updated_at DESC);
CREATE TABLE IF NOT EXISTS project_sessions(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  acp_session_id TEXT UNIQUE,
  acp_title TEXT,
  custom_name TEXT,
  lifecycle TEXT NOT NULL CHECK(lifecycle IN
    ('pending','activating','ready','failed','reconciliation_required','archived')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_opened_at TEXT,
  last_chat_id TEXT,
  failure_code TEXT
);
CREATE INDEX IF NOT EXISTS project_sessions_project_updated_idx
  ON project_sessions(project_id, updated_at DESC);
CREATE TABLE IF NOT EXISTS metadata_commands(
  command_id TEXT PRIMARY KEY,
  command_type TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  phase TEXT NOT NULL,
  project_id TEXT,
  session_id TEXT,
  chat_id TEXT,
  acp_session_id TEXT,
  error_code TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_activations(
  session_id TEXT PRIMARY KEY REFERENCES project_sessions(id) ON DELETE CASCADE,
  command_id TEXT NOT NULL REFERENCES metadata_commands(command_id) ON DELETE RESTRICT,
  phase TEXT NOT NULL,
  chat_id TEXT,
  acp_session_id TEXT,
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS metadata_imports(
  source TEXT PRIMARY KEY,
  completed_at TEXT NOT NULL,
  imported_count INTEGER NOT NULL,
  skipped_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS projection_state(
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  generation INTEGER NOT NULL,
  projected_generation INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT OR IGNORE INTO projection_state(singleton,generation,projected_generation,updated_at)
VALUES(1,0,0,'1970-01-01T00:00:00Z');
"#;

const MIGRATION_V2: &str = r#"
ALTER TABLE project_sessions ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy_hidden'
  CHECK(origin IN ('hub','imported','legacy_hidden'));
UPDATE project_sessions
SET origin='hub'
WHERE EXISTS (
  SELECT 1 FROM metadata_commands c
  WHERE c.session_id=project_sessions.id AND c.command_type='session/create'
);
"#;

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("metadata database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("metadata schema version {found} is newer than supported {supported}")]
    NewerSchema { found: i64, supported: i64 },
    #[error("metadata conflict: {0}")]
    Conflict(String),
    #[error("metadata not found: {0}")]
    NotFound(String),
    #[error("metadata invalid state: {0}")]
    InvalidState(String),
}

pub type Result<T> = std::result::Result<T, MetadataError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub instance_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSessionRecord {
    pub id: String,
    pub project_id: String,
    pub acp_session_id: Option<String>,
    pub acp_title: Option<String>,
    pub custom_name: Option<String>,
    pub lifecycle: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
    pub last_chat_id: Option<String>,
    pub failure_code: Option<String>,
    pub origin: String,
}

impl ProjectSessionRecord {
    pub fn display_title(&self) -> String {
        self.custom_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.acp_title.as_deref().filter(|s| !s.trim().is_empty()))
            .unwrap_or("新对话")
            .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataCommand {
    pub command_id: String,
    pub command_type: String,
    pub payload_hash: String,
    pub phase: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub chat_id: Option<String>,
    pub acp_session_id: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginCommand {
    New,
    Existing,
}

pub struct NewSession<'a> {
    pub id: &'a str,
    pub project_id: &'a str,
    pub title: Option<&'a str>,
}

#[derive(Clone)]
pub struct MetadataStore {
    pool: SqlitePool,
    path: PathBuf,
    _owner_lock: std::sync::Arc<File>,
}

impl MetadataStore {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).map_err(sqlx::Error::Io)?;
        let lock_path = data_dir.join("metadata.owner.lock");
        let owner_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(sqlx::Error::Io)?;
        set_private_permissions(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let rc = unsafe { libc::flock(owner_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return Err(MetadataError::Conflict(
                    "metadata database is owned by another server process".into(),
                ));
            }
        }
        let path = data_dir.join(METADATA_DB_FILE);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let store = Self {
            pool,
            path,
            _owner_lock: std::sync::Arc::new(owner_lock),
        };
        store.migrate().await?;
        store.verify_pragmas().await?;
        set_private_permissions(&store.path)?;
        store.secure_sidecars()?;
        Ok(store)
    }

    fn secure_sidecars(&self) -> Result<()> {
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{}", self.path.display(), suffix));
            if path.exists() {
                set_private_permissions(&path)?;
            }
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn migrate(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)")
            .execute(&mut *tx).await?;
        let found: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
            .fetch_one(&mut *tx)
            .await?;
        let found = found.unwrap_or(0);
        if found > SCHEMA_VERSION {
            return Err(MetadataError::NewerSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < 1 {
            for statement in MIGRATION_V1
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                sqlx::query(statement).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO schema_migrations(version,applied_at) VALUES(1,?)")
                .bind(now())
                .execute(&mut *tx)
                .await?;
        }
        if found < 2 {
            for statement in MIGRATION_V2
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                sqlx::query(statement).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO schema_migrations(version,applied_at) VALUES(2,?)")
                .bind(now())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn verify_pragmas(&self) -> Result<()> {
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await?;
        if fk != 1 {
            return Err(MetadataError::InvalidState(
                "foreign_keys is disabled".into(),
            ));
        }
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(MetadataError::InvalidState(format!(
                "journal_mode is {mode}, expected wal"
            )));
        }
        Ok(())
    }

    pub async fn begin_command(
        &self,
        command_id: &str,
        command_type: &str,
        payload_hash: &str,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<BeginCommand> {
        self.begin_command_with_activation(
            command_id,
            command_type,
            payload_hash,
            project_id,
            session_id,
            None,
            None,
        )
        .await
    }

    /// Durably records the command intention and, when requested, the logical
    /// session plus its single-flight activation in one SQLite transaction.
    #[allow(clippy::too_many_arguments)] // One durable transaction carries the full command/activation identity tuple.
    pub async fn begin_command_with_activation(
        &self,
        command_id: &str,
        command_type: &str,
        payload_hash: &str,
        project_id: Option<&str>,
        session_id: Option<&str>,
        new_session: Option<NewSession<'_>>,
        activate_session: Option<&str>,
    ) -> Result<BeginCommand> {
        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query("SELECT command_id,command_type,payload_hash,phase,project_id,session_id,chat_id,acp_session_id,error_code FROM metadata_commands WHERE command_id=?")
            .bind(command_id).fetch_optional(&mut *tx).await?
            .map(|r| MetadataCommand { command_id: r.get(0), command_type: r.get(1), payload_hash: r.get(2), phase: r.get(3), project_id: r.get(4), session_id: r.get(5), chat_id: r.get(6), acp_session_id: r.get(7), error_code: r.get(8) });
        if let Some(existing) = existing {
            if existing.command_type != command_type || existing.payload_hash != payload_hash {
                return Err(MetadataError::Conflict(format!(
                    "command {command_id} payload/type mismatch"
                )));
            }
            return Ok(BeginCommand::Existing);
        }
        let ts = now();
        sqlx::query("INSERT INTO metadata_commands(command_id,command_type,payload_hash,phase,project_id,session_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?)")
            .bind(command_id).bind(command_type).bind(payload_hash).bind("intention_durable")
            .bind(project_id).bind(session_id).bind(&ts).bind(&ts).execute(&mut *tx).await?;
        let has_new_session = new_session.is_some();
        if let Some(new_session) = new_session {
            sqlx::query("INSERT INTO project_sessions(id,project_id,acp_title,lifecycle,created_at,updated_at,origin) VALUES(?,?,?,?,?,?,?)")
                .bind(new_session.id).bind(new_session.project_id).bind(new_session.title)
                .bind("pending").bind(&ts).bind(&ts).bind("hub").execute(&mut *tx).await?;
        }
        if let Some(session_id) = activate_session {
            sqlx::query("INSERT INTO session_activations(session_id,command_id,phase,started_at,updated_at) VALUES(?,?,?,?,?)")
                .bind(session_id).bind(command_id).bind("intention_durable").bind(&ts).bind(&ts)
                .execute(&mut *tx).await.map_err(|e| {
                    if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
                        MetadataError::Conflict(format!("session {session_id} activation already in progress"))
                    } else { MetadataError::Database(e) }
                })?;
            sqlx::query(
                "UPDATE project_sessions SET lifecycle='activating',updated_at=? WHERE id=?",
            )
            .bind(&ts)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        }
        if has_new_session || activate_session.is_some() {
            bump_generation_tx(&mut tx).await?;
        }
        tx.commit().await?;
        Ok(BeginCommand::New)
    }

    pub async fn command(&self, id: &str) -> Result<Option<MetadataCommand>> {
        let row = sqlx::query("SELECT command_id,command_type,payload_hash,phase,project_id,session_id,chat_id,acp_session_id,error_code FROM metadata_commands WHERE command_id=?")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| MetadataCommand {
            command_id: r.get(0),
            command_type: r.get(1),
            payload_hash: r.get(2),
            phase: r.get(3),
            project_id: r.get(4),
            session_id: r.get(5),
            chat_id: r.get(6),
            acp_session_id: r.get(7),
            error_code: r.get(8),
        }))
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the persisted metadata command record without partial updates.
    pub async fn update_command(
        &self,
        id: &str,
        phase: &str,
        project: Option<&str>,
        session: Option<&str>,
        chat: Option<&str>,
        acp: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let result = sqlx::query("UPDATE metadata_commands SET phase=?, project_id=COALESCE(?,project_id), session_id=COALESCE(?,session_id), chat_id=COALESCE(?,chat_id), acp_session_id=COALESCE(?,acp_session_id), error_code=?, updated_at=? WHERE command_id=?")
            .bind(phase).bind(project).bind(session).bind(chat).bind(acp).bind(error).bind(now()).bind(id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(MetadataError::NotFound(format!("command {id}")));
        }
        Ok(())
    }

    pub async fn create_project(
        &self,
        id: &str,
        name: &str,
        cwd: &str,
        instance: &str,
    ) -> Result<ProjectRecord> {
        let ts = now();
        sqlx::query("INSERT INTO projects(id,name,cwd,instance_id,created_at,updated_at) VALUES(?,?,?,?,?,?)")
            .bind(id).bind(name).bind(cwd).bind(instance).bind(&ts).bind(&ts).execute(&self.pool).await?;
        self.bump_generation().await?;
        self.project(id)
            .await?
            .ok_or_else(|| MetadataError::NotFound(id.into()))
    }

    pub async fn import_project(&self, rec: &ProjectRecord) -> Result<bool> {
        let result = sqlx::query("INSERT OR IGNORE INTO projects(id,name,cwd,instance_id,created_at,updated_at,archived_at) VALUES(?,?,?,?,?,?,?)")
            .bind(&rec.id).bind(&rec.name).bind(&rec.cwd).bind(&rec.instance_id)
            .bind(&rec.created_at).bind(&rec.updated_at).bind(&rec.archived_at).execute(&self.pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn import_session(
        &self,
        id: &str,
        project_id: &str,
        acp_session_id: &str,
        title: &str,
        updated_at: &str,
    ) -> Result<bool> {
        let created = if updated_at.is_empty() {
            now()
        } else {
            updated_at.to_string()
        };
        let result = sqlx::query("INSERT OR IGNORE INTO project_sessions(id,project_id,acp_session_id,acp_title,lifecycle,created_at,updated_at,origin) VALUES(?,?,?,?,?,?,?,?)")
            .bind(id).bind(project_id).bind(acp_session_id).bind(title).bind("ready")
            .bind(&created).bind(&created).bind("legacy_hidden").execute(&self.pool).await?;
        if result.rows_affected() == 1 {
            self.bump_generation().await?;
        }
        Ok(result.rows_affected() == 1)
    }

    pub async fn archive_project(&self, id: &str) -> Result<()> {
        let ts = now();
        let result = sqlx::query(
            "UPDATE projects SET archived_at=?,updated_at=? WHERE id=? AND archived_at IS NULL",
        )
        .bind(&ts)
        .bind(&ts)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(MetadataError::NotFound(format!("project {id}")));
        }
        self.bump_generation().await
    }

    pub async fn project(&self, id: &str) -> Result<Option<ProjectRecord>> {
        let row = sqlx::query("SELECT id,name,cwd,instance_id,created_at,updated_at,archived_at FROM projects WHERE id=?")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(project_from_row))
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        Ok(sqlx::query("SELECT id,name,cwd,instance_id,created_at,updated_at,archived_at FROM projects ORDER BY updated_at DESC,id")
            .fetch_all(&self.pool).await?.into_iter().map(project_from_row).collect())
    }

    pub async fn create_pending_session(
        &self,
        id: &str,
        project_id: &str,
        title: Option<&str>,
    ) -> Result<ProjectSessionRecord> {
        let ts = now();
        sqlx::query("INSERT INTO project_sessions(id,project_id,acp_title,lifecycle,created_at,updated_at,origin) VALUES(?,?,?,?,?,?,?)")
            .bind(id).bind(project_id).bind(title).bind("pending").bind(&ts).bind(&ts).bind("hub").execute(&self.pool).await?;
        self.bump_generation().await?;
        self.session(id)
            .await?
            .ok_or_else(|| MetadataError::NotFound(id.into()))
    }

    pub async fn begin_activation(&self, session_id: &str, command_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let ts = now();
        sqlx::query("INSERT INTO session_activations(session_id,command_id,phase,started_at,updated_at) VALUES(?,?,?,?,?)")
            .bind(session_id).bind(command_id).bind("intention_durable").bind(&ts).bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE project_sessions SET lifecycle='activating',updated_at=? WHERE id=?")
            .bind(&ts)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn activation_phase(
        &self,
        session_id: &str,
        phase: &str,
        chat: Option<&str>,
        acp: Option<&str>,
    ) -> Result<()> {
        let result = sqlx::query("UPDATE session_activations SET phase=?,chat_id=COALESCE(?,chat_id),acp_session_id=COALESCE(?,acp_session_id),updated_at=? WHERE session_id=?")
            .bind(phase).bind(chat).bind(acp).bind(now()).bind(session_id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(MetadataError::NotFound(format!("activation {session_id}")));
        }
        Ok(())
    }

    pub async fn finalize_session(
        &self,
        id: &str,
        acp: &str,
        title: Option<&str>,
        chat: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let ts = now();
        sqlx::query("UPDATE project_sessions SET acp_session_id=?,acp_title=COALESCE(?,acp_title),lifecycle='ready',last_chat_id=?,last_opened_at=?,updated_at=?,failure_code=NULL WHERE id=?")
            .bind(acp).bind(title).bind(chat).bind(&ts).bind(&ts).bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM session_activations WHERE session_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        bump_generation_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn finalize_session_and_command(
        &self,
        command_id: &str,
        session_id: &str,
        project_id: &str,
        acp: &str,
        title: Option<&str>,
        chat: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let ts = now();
        let session = sqlx::query("UPDATE project_sessions SET acp_session_id=?,acp_title=COALESCE(?,acp_title),lifecycle='ready',last_chat_id=?,last_opened_at=?,updated_at=?,failure_code=NULL WHERE id=?")
            .bind(acp).bind(title).bind(chat).bind(&ts).bind(&ts).bind(session_id).execute(&mut *tx).await?;
        if session.rows_affected() != 1 {
            return Err(MetadataError::NotFound(format!("session {session_id}")));
        }
        let command = sqlx::query("UPDATE metadata_commands SET phase='projection_pending',project_id=?,session_id=?,chat_id=?,acp_session_id=?,error_code=NULL,updated_at=? WHERE command_id=?")
            .bind(project_id).bind(session_id).bind(chat).bind(acp).bind(&ts).bind(command_id).execute(&mut *tx).await?;
        if command.rows_affected() != 1 {
            return Err(MetadataError::NotFound(format!("command {command_id}")));
        }
        sqlx::query("DELETE FROM session_activations WHERE session_id=?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        bump_generation_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_reconciliation_required(&self, id: &str, code: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE project_sessions SET lifecycle='reconciliation_required',failure_code=?,updated_at=? WHERE id=?")
            .bind(code).bind(now()).bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE session_activations SET phase='reconciliation_required',updated_at=? WHERE session_id=?")
            .bind(now()).bind(id).execute(&mut *tx).await?;
        bump_generation_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn reconcile_activation_and_command(
        &self,
        session_id: &str,
        command_id: &str,
        code: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let ts = now();
        let session = sqlx::query("UPDATE project_sessions SET lifecycle='reconciliation_required',failure_code=?,updated_at=? WHERE id=?")
            .bind(code).bind(&ts).bind(session_id).execute(&mut *tx).await?;
        let command = sqlx::query("UPDATE metadata_commands SET phase='reconciliation_required',error_code=?,updated_at=? WHERE command_id=?")
            .bind(code).bind(&ts).bind(command_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE session_activations SET phase='reconciliation_required',updated_at=? WHERE session_id=?")
            .bind(&ts).bind(session_id).execute(&mut *tx).await?;
        if session.rows_affected() != 1 {
            return Err(MetadataError::NotFound(format!("session {session_id}")));
        }
        if command.rows_affected() != 1 {
            return Err(MetadataError::NotFound(format!("command {command_id}")));
        }
        bump_generation_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_session(&self, id: &str, code: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE project_sessions SET lifecycle='failed',failure_code=?,updated_at=? WHERE id=?",
        )
        .bind(code)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM session_activations WHERE session_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        bump_generation_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn touch_session_open(&self, id: &str, chat: &str) -> Result<()> {
        let ts = now();
        sqlx::query(
            "UPDATE project_sessions SET last_chat_id=?,last_opened_at=?,updated_at=? WHERE id=?",
        )
        .bind(chat)
        .bind(&ts)
        .bind(&ts)
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.bump_generation().await
    }

    pub async fn rename_session(&self, id: &str, name: &str) -> Result<()> {
        let result =
            sqlx::query("UPDATE project_sessions SET custom_name=?,updated_at=? WHERE id=?")
                .bind(name)
                .bind(now())
                .bind(id)
                .execute(&self.pool)
                .await?;
        if result.rows_affected() == 0 {
            return Err(MetadataError::NotFound(format!("session {id}")));
        }
        self.bump_generation().await
    }

    pub async fn update_acp_title(&self, acp: &str, title: &str) -> Result<()> {
        sqlx::query("UPDATE project_sessions SET acp_title=?,updated_at=? WHERE acp_session_id=?")
            .bind(title)
            .bind(now())
            .bind(acp)
            .execute(&self.pool)
            .await?;
        self.bump_generation().await
    }

    pub async fn session(&self, id: &str) -> Result<Option<ProjectSessionRecord>> {
        let row = sqlx::query("SELECT id,project_id,acp_session_id,acp_title,custom_name,lifecycle,created_at,updated_at,last_opened_at,last_chat_id,failure_code,origin FROM project_sessions WHERE id=?")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(session_from_row))
    }

    pub async fn list_sessions(&self) -> Result<Vec<ProjectSessionRecord>> {
        Ok(sqlx::query("SELECT id,project_id,acp_session_id,acp_title,custom_name,lifecycle,created_at,updated_at,last_opened_at,last_chat_id,failure_code,origin FROM project_sessions ORDER BY updated_at DESC,id")
            .fetch_all(&self.pool).await?.into_iter().map(session_from_row).collect())
    }

    pub async fn import_explicit_session(
        &self,
        id: &str,
        project_id: &str,
        acp_session_id: &str,
        title: &str,
        updated_at: &str,
    ) -> Result<ProjectSessionRecord> {
        let ts = now();
        let created = if updated_at.is_empty() {
            &ts
        } else {
            updated_at
        };
        let mut tx = self.pool.begin().await?;
        let existing =
            sqlx::query("SELECT project_id,origin FROM project_sessions WHERE acp_session_id=?")
                .bind(acp_session_id)
                .fetch_optional(&mut *tx)
                .await?;
        let changed = if let Some(existing) = existing {
            let existing_project: String = existing.get(0);
            let origin: String = existing.get(1);
            if existing_project != project_id {
                return Err(MetadataError::Conflict(format!(
                    "ACP session {acp_session_id} already belongs to project {existing_project}"
                )));
            }
            if origin == "legacy_hidden" {
                sqlx::query("UPDATE project_sessions SET acp_title=?,lifecycle='ready',updated_at=?,origin='imported' WHERE acp_session_id=?")
                    .bind(title).bind(&ts).bind(acp_session_id).execute(&mut *tx).await?;
                true
            } else {
                false
            }
        } else {
            sqlx::query(
                "INSERT INTO project_sessions(id,project_id,acp_session_id,acp_title,lifecycle,created_at,updated_at,origin) VALUES(?,?,?,?,?,?,?,'imported')",
            )
            .bind(id).bind(project_id).bind(acp_session_id).bind(title).bind("ready")
            .bind(created).bind(&ts).execute(&mut *tx).await?;
            true
        };
        if changed {
            bump_generation_tx(&mut tx).await?;
        }
        tx.commit().await?;
        self.find_by_acp_id(acp_session_id)
            .await?
            .ok_or_else(|| MetadataError::NotFound(acp_session_id.into()))
    }

    pub async fn find_by_acp_id(
        &self,
        acp_session_id: &str,
    ) -> Result<Option<ProjectSessionRecord>> {
        let row = sqlx::query("SELECT id,project_id,acp_session_id,acp_title,custom_name,lifecycle,created_at,updated_at,last_opened_at,last_chat_id,failure_code,origin FROM project_sessions WHERE acp_session_id=?")
            .bind(acp_session_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(session_from_row))
    }

    pub async fn recover_after_restart(&self) -> Result<(u64, u64)> {
        let mut tx = self.pool.begin().await?;
        let ts = now();
        // A command that never crossed the dispatch boundary has no ACP side
        // effect. Terminate it as safely retryable instead of leaving an
        // eternal in-progress dedup record.
        sqlx::query("UPDATE metadata_commands SET phase='failed',error_code='server_restart_before_dispatch_safe_retry',updated_at=? WHERE phase='intention_durable' AND command_id NOT IN (SELECT command_id FROM session_activations)")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE metadata_commands SET phase='failed',error_code='server_restart_before_dispatch_safe_retry',updated_at=? WHERE command_id IN (SELECT command_id FROM session_activations WHERE phase IN ('intention_durable','dispatch_pending'))")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE project_sessions SET lifecycle='failed',failure_code='server_restart_before_dispatch_safe_retry',updated_at=? WHERE id IN (SELECT session_id FROM session_activations WHERE phase IN ('intention_durable','dispatch_pending'))")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM session_activations WHERE phase IN ('intention_durable','dispatch_pending')")
            .execute(&mut *tx).await?;
        // Runtime chat ids are process-local hints and must never survive a restart.
        sqlx::query("UPDATE project_sessions SET last_chat_id=NULL WHERE last_chat_id IS NOT NULL")
            .execute(&mut *tx)
            .await?;
        // Once the ACP id is durable, opening can safely resume by issuing
        // session/load in a fresh chat. Earlier phases have an unknown outcome.
        let recovered = sqlx::query("UPDATE project_sessions SET acp_session_id=(SELECT acp_session_id FROM session_activations a WHERE a.session_id=project_sessions.id),lifecycle='ready',failure_code=NULL,updated_at=? WHERE id IN (SELECT session_id FROM session_activations WHERE acp_session_id IS NOT NULL)")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE metadata_commands SET phase='reconciliation_required',chat_id=NULL,error_code='server_restart_after_acp_id',acp_session_id=COALESCE(acp_session_id,(SELECT acp_session_id FROM session_activations a WHERE a.command_id=metadata_commands.command_id)),updated_at=? WHERE command_id IN (SELECT command_id FROM session_activations WHERE acp_session_id IS NOT NULL)")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM session_activations WHERE acp_session_id IS NOT NULL")
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("UPDATE project_sessions SET lifecycle='reconciliation_required',failure_code='server_restart_before_acp_id',updated_at=? WHERE id IN (SELECT session_id FROM session_activations)")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE metadata_commands SET phase='reconciliation_required',error_code='server_restart_after_dispatch',updated_at=? WHERE command_id IN (SELECT command_id FROM session_activations)")
            .bind(&ts).execute(&mut *tx).await?;
        sqlx::query("UPDATE session_activations SET phase='reconciliation_required',updated_at=?")
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() > 0 {
            bump_generation_tx(&mut tx).await?;
        }
        tx.commit().await?;
        Ok((recovered.rows_affected(), result.rows_affected()))
    }

    #[cfg(test)]
    pub async fn stale_activations_require_reconciliation(&self) -> Result<u64> {
        Ok(self.recover_after_restart().await?.1)
    }

    pub async fn import_completed(&self, source: &str) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM metadata_imports WHERE source=?")
                .bind(source)
                .fetch_optional(&self.pool)
                .await?;
        Ok(found.is_some())
    }

    pub async fn mark_import_complete(
        &self,
        source: &str,
        imported: i64,
        skipped: i64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO metadata_imports(source,completed_at,imported_count,skipped_count) VALUES(?,?,?,?)")
            .bind(source).bind(now()).bind(imported).bind(skipped).execute(&self.pool).await?;
        self.bump_generation().await
    }

    pub async fn generation(&self) -> Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT generation,projected_generation FROM projection_state WHERE singleton=1",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((row.get(0), row.get(1)))
    }

    pub async fn mark_projected(&self, generation: i64) -> Result<()> {
        sqlx::query("UPDATE projection_state SET projected_generation=?,updated_at=? WHERE singleton=1 AND generation>=?")
            .bind(generation).bind(now()).bind(generation).execute(&self.pool).await?;
        Ok(())
    }

    async fn bump_generation(&self) -> Result<()> {
        sqlx::query(
            "UPDATE projection_state SET generation=generation+1,updated_at=? WHERE singleton=1",
        )
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn bump_generation_tx(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
    sqlx::query(
        "UPDATE projection_state SET generation=generation+1,updated_at=? WHERE singleton=1",
    )
    .bind(now())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn project_from_row(r: sqlx::sqlite::SqliteRow) -> ProjectRecord {
    ProjectRecord {
        id: r.get(0),
        name: r.get(1),
        cwd: r.get(2),
        instance_id: r.get(3),
        created_at: r.get(4),
        updated_at: r.get(5),
        archived_at: r.get(6),
    }
}

fn session_from_row(r: sqlx::sqlite::SqliteRow) -> ProjectSessionRecord {
    ProjectSessionRecord {
        id: r.get(0),
        project_id: r.get(1),
        acp_session_id: r.get(2),
        acp_title: r.get(3),
        custom_name: r.get(4),
        lifecycle: r.get(5),
        created_at: r.get(6),
        updated_at: r.get(7),
        last_opened_at: r.get(8),
        last_chat_id: r.get(9),
        failure_code: r.get(10),
        origin: r.get(11),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .map_err(sqlx::Error::Io)?
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(sqlx::Error::Io)?;
    }
    Ok(())
}

pub fn payload_hash(value: &impl Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes =
        serde_json::to_vec(value).map_err(|e| MetadataError::InvalidState(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}
