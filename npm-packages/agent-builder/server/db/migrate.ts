import type { Database } from 'bun:sqlite';

const MIGRATION_V1 = `
CREATE TABLE agents (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  config      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE chats (
  chat_id    TEXT PRIMARY KEY,
  agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
  session_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_chats_agent_id ON chats(agent_id);
`;

const MIGRATION_V2 = `
CREATE TABLE providers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 80),
  type TEXT NOT NULL CHECK(type IN ('anthropic','openai')),
  base_url TEXT,
  credential_source TEXT NOT NULL CHECK(credential_source IN ('env','local')),
  api_key TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision >= 1),
  CHECK(credential_source='local' OR api_key IS NULL),
  CHECK(api_key IS NULL OR length(trim(api_key))>0)
) STRICT;
ALTER TABLE agents ADD COLUMN provider_id TEXT REFERENCES providers(id) ON DELETE RESTRICT;
ALTER TABLE agents ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>=1);
CREATE INDEX idx_agents_provider_id ON agents(provider_id);
`;

/**
 * Synthetic agent that backs sessions created from inline agent configuration. `chats.agent_id`
 * is NOT NULL with ON DELETE RESTRICT, so inline sessions need one durable row to reference
 * instead of creating an agent per session.
 *
 * The constant is a valid UUIDv7 (zero timestamp) so it satisfies the same identifier contract
 * as every other platform resource.
 */
export const SESSION_INLINE_AGENT_ID = '00000000-0000-7000-8000-000000000001';

const SESSION_INLINE_AGENT_CONFIG = '{"instructions":"","provider":null,"metaHarness":{},"mcpServers":{}}';

/**
 * Platform tables store timestamps as Unix milliseconds internally; API serializers convert
 * them to seconds. Inherited `agents` / `chats` rows keep the same millisecond convention.
 */
const MIGRATION_V3 = `
ALTER TABLE agents ADD COLUMN model TEXT;
ALTER TABLE agents ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
ALTER TABLE agents ADD COLUMN tools TEXT NOT NULL DEFAULT '[]';
ALTER TABLE agents ADD COLUMN multi_agent TEXT;
ALTER TABLE agents ADD COLUMN reasoning TEXT;
ALTER TABLE agents ADD COLUMN text_config TEXT;
ALTER TABLE agents ADD COLUMN service_tier TEXT;
ALTER TABLE agents ADD COLUMN compaction TEXT;
ALTER TABLE agents ADD COLUMN experimental TEXT;
ALTER TABLE agents ADD COLUMN wire_name TEXT;

CREATE TABLE platform_sessions (
  id                    TEXT PRIMARY KEY,
  agent_id              TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
  agent_config          TEXT NOT NULL,
  environment_type      TEXT NOT NULL CHECK(environment_type IN ('none','self_hosted','local')),
  environment_id        TEXT,
  metadata              TEXT NOT NULL DEFAULT '{}',
  vault_ids             TEXT NOT NULL DEFAULT '[]',
  status                TEXT NOT NULL CHECK(status IN ('idle','in_progress','requires_action','failed')),
  error                 TEXT,
  required_actions      TEXT NOT NULL DEFAULT '[]',
  usage                 TEXT,
  prompt_cache_options  TEXT,
  review                TEXT,
  environment_reset_count INTEGER NOT NULL DEFAULT 0,
  created_at            INTEGER NOT NULL,
  last_active_at        INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_turns (
  id                   TEXT PRIMARY KEY,
  session_id           TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  agent_id             TEXT NOT NULL,
  subagent_id          TEXT,
  runtime_turn_id      TEXT NOT NULL,
  binding_revision     INTEGER NOT NULL,
  status               TEXT NOT NULL CHECK(status IN ('queued','in_progress','waiting','completed','failed','cancelled')),
  created_at           INTEGER NOT NULL,
  started_at           INTEGER,
  completed_at         INTEGER,
  error_code           TEXT,
  error_message        TEXT,
  stop_reason          TEXT,
  usage                TEXT,
  prompt_cache_options TEXT,
  UNIQUE(session_id, binding_revision, runtime_turn_id)
) STRICT;

CREATE TABLE platform_items (
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  turn_id      TEXT NOT NULL REFERENCES platform_turns(id) ON DELETE CASCADE,
  output_index INTEGER NOT NULL,
  type         TEXT NOT NULL,
  status       TEXT NOT NULL,
  call_id      TEXT,
  payload      TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  completed_at INTEGER,
  UNIQUE(turn_id, output_index)
) STRICT;

CREATE TABLE platform_events (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id   TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  type       TEXT NOT NULL,
  turn_id    TEXT,
  item_id    TEXT,
  data       TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_subagents (
  id              TEXT PRIMARY KEY,
  session_id      TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  name            TEXT NOT NULL,
  instructions    TEXT,
  parent_agent_id TEXT NOT NULL,
  status          TEXT NOT NULL CHECK(status IN ('active','closed')),
  opened_at       INTEGER NOT NULL,
  closed_at       INTEGER
) STRICT;

CREATE TABLE platform_input_receipts (
  session_id      TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  idempotency_key TEXT NOT NULL,
  digest          TEXT NOT NULL,
  request_id      TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY(session_id, idempotency_key)
) STRICT;

CREATE TABLE platform_vaults (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  metadata   TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_credentials (
  id            TEXT PRIMARY KEY,
  vault_id      TEXT NOT NULL REFERENCES platform_vaults(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  auth_kind     TEXT NOT NULL CHECK(auth_kind IN ('static_bearer','environment_variable')),
  auth          TEXT NOT NULL,
  secret_cipher BLOB NOT NULL,
  secret_iv     BLOB NOT NULL,
  secret_tag    BLOB NOT NULL,
  key_id        TEXT NOT NULL,
  metadata      TEXT NOT NULL DEFAULT '{}',
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_environments (
  id         TEXT PRIMARY KEY,
  type       TEXT NOT NULL CHECK(type IN ('none','self_hosted','local')),
  status     TEXT NOT NULL CHECK(status IN ('pending','ready','connected','disconnected','failed')),
  name       TEXT NOT NULL,
  config     TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_environment_templates (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  config     TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_environment_files (
  id                 TEXT PRIMARY KEY,
  environment_id     TEXT NOT NULL REFERENCES platform_environments(id) ON DELETE CASCADE,
  path               TEXT NOT NULL,
  content_ciphertext BLOB NOT NULL,
  size_bytes         INTEGER NOT NULL,
  created_at         INTEGER NOT NULL,
  UNIQUE(environment_id, path)
) STRICT;

CREATE TABLE platform_automations (
  id           TEXT PRIMARY KEY,
  type         TEXT NOT NULL CHECK(type IN ('periodic','once')),
  name         TEXT NOT NULL,
  target       TEXT NOT NULL,
  metadata     TEXT NOT NULL DEFAULT '{}',
  expression   TEXT,
  timezone     TEXT,
  run_at       INTEGER,
  status       TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','paused','completed')),
  pause_reason TEXT,
  next_run_at  INTEGER,
  created_by   TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_automation_invocations (
  id            TEXT PRIMARY KEY,
  automation_id TEXT NOT NULL REFERENCES platform_automations(id) ON DELETE CASCADE,
  source        TEXT NOT NULL CHECK(source IN ('schedule','manual')),
  scheduled_at  INTEGER,
  session_id    TEXT,
  status        TEXT NOT NULL CHECK(status IN ('queued','completed','failed','skipped')),
  skip_reason   TEXT,
  error_code    TEXT,
  error_message TEXT,
  created_at    INTEGER NOT NULL,
  completed_at  INTEGER,
  UNIQUE(automation_id, scheduled_at)
) STRICT;

CREATE TABLE platform_artifacts (
  id             TEXT PRIMARY KEY,
  session_id     TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  environment_id TEXT,
  turn_id        TEXT,
  path           TEXT NOT NULL,
  media_type     TEXT,
  size_bytes     INTEGER NOT NULL,
  created_at     INTEGER NOT NULL
) STRICT;

CREATE TABLE platform_traces (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES platform_sessions(id) ON DELETE CASCADE,
  turn_id    TEXT,
  kind       TEXT NOT NULL,
  data       TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_platform_sessions_created ON platform_sessions(created_at, id);
CREATE INDEX idx_platform_sessions_agent_id ON platform_sessions(agent_id);
CREATE INDEX idx_platform_turns_session_created ON platform_turns(session_id, created_at, id);
CREATE INDEX idx_platform_items_turn_output ON platform_items(turn_id, output_index);
CREATE INDEX idx_platform_events_session_seq ON platform_events(session_id, seq);
CREATE INDEX idx_platform_subagents_session_id ON platform_subagents(session_id);
CREATE INDEX idx_platform_automation_invocations_automation_created
  ON platform_automation_invocations(automation_id, created_at);
CREATE INDEX idx_platform_automations_status_next_run ON platform_automations(status, next_run_at);
CREATE INDEX idx_platform_credentials_vault_id ON platform_credentials(vault_id);
CREATE INDEX idx_platform_environment_files_environment_path
  ON platform_environment_files(environment_id, path);
CREATE INDEX idx_platform_artifacts_session_id ON platform_artifacts(session_id);
CREATE INDEX idx_platform_traces_session_id ON platform_traces(session_id, created_at);

INSERT INTO agents (id, name, description, config, provider_id, revision, created_at, updated_at)
VALUES (
  '${SESSION_INLINE_AGENT_ID}',
  'session-inline',
  '',
  '${SESSION_INLINE_AGENT_CONFIG}',
  NULL,
  1,
  0,
  0
);
`;

const MIGRATIONS: ReadonlyArray<(db: Database) => void> = [
  (db) => db.exec(MIGRATION_V1),
  (db) => db.exec(MIGRATION_V2),
  (db) => db.exec(MIGRATION_V3),
];

export const LATEST_SCHEMA_VERSION = MIGRATIONS.length;

export function getSchemaVersion(db: Database): number {
  const row = db.query<{ user_version: number }, []>('PRAGMA user_version').get();
  return row?.user_version ?? 0;
}

/**
 * Applies migrations up to `targetVersion` (default: the latest schema).
 * Tests use a lower target to build an older database before exercising an upgrade.
 */
export function migrate(db: Database, targetVersion: number = LATEST_SCHEMA_VERSION): number {
  const current = getSchemaVersion(db);
  if (current > LATEST_SCHEMA_VERSION) {
    throw new Error(
      `Database schema version ${current} is newer than the supported version ${LATEST_SCHEMA_VERSION}`,
    );
  }
  if (targetVersion < current) {
    throw new Error(`Cannot migrate database down from version ${current} to ${targetVersion}`);
  }
  if (targetVersion > LATEST_SCHEMA_VERSION) {
    throw new Error(
      `Requested schema version ${targetVersion} is newer than the supported version ${LATEST_SCHEMA_VERSION}`,
    );
  }

  for (let version = current + 1; version <= targetVersion; version += 1) {
    const apply = MIGRATIONS[version - 1];
    if (!apply) throw new Error(`Missing migration for schema version ${version}`);
    db.transaction(() => {
      apply(db);
      db.exec(`PRAGMA user_version = ${version}`);
    })();
  }

  return getSchemaVersion(db);
}
