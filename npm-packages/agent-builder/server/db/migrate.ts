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

const MIGRATIONS: ReadonlyArray<(db: Database) => void> = [
  (db) => db.exec(MIGRATION_V1),
  (db) => db.exec(MIGRATION_V2),
];

export const LATEST_SCHEMA_VERSION = MIGRATIONS.length;

export function getSchemaVersion(db: Database): number {
  const row = db.query<{ user_version: number }, []>('PRAGMA user_version').get();
  return row?.user_version ?? 0;
}

export function migrate(db: Database): number {
  const current = getSchemaVersion(db);
  if (current > LATEST_SCHEMA_VERSION) {
    throw new Error(
      `Database schema version ${current} is newer than the supported version ${LATEST_SCHEMA_VERSION}`,
    );
  }

  for (let version = current + 1; version <= LATEST_SCHEMA_VERSION; version += 1) {
    const apply = MIGRATIONS[version - 1];
    if (!apply) throw new Error(`Missing migration for schema version ${version}`);
    db.transaction(() => {
      apply(db);
      db.exec(`PRAGMA user_version = ${version}`);
    })();
  }

  return getSchemaVersion(db);
}
