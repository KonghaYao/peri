import Database from 'better-sqlite3';
import { existsSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { createHash } from 'node:crypto';
import type { SandboxRecord, StoredSandbox } from './types.js';
import { ControlError } from './types.js';

interface Row {
  id: string;
  name: string | null;
  profile: string;
  state: StoredSandbox['state'];
  desired_state: StoredSandbox['desiredState'];
  generation: string | null;
  unit: string | null;
  mcp_url: string;
  policy_json: string;
  policy_hash: string;
  resource_json: string;
  applied_hash: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

interface Receipt {
  request_id: string;
  operation: string;
  digest: string;
  status: string;
  result: string | null;
  sandbox_id: string | null;
}
const decode = (row: Row): StoredSandbox => ({
  id: row.id,
  name: row.name ?? undefined,
  profile: row.profile,
  state: row.state,
  desiredState: row.desired_state,
  generation: row.generation ?? undefined,
  unit: row.unit ?? undefined,
  mcpUrl: row.mcp_url,
  policyJson: row.policy_json,
  policyHash: row.policy_hash,
  resourceJson: row.resource_json,
  appliedHash: row.applied_hash ?? undefined,
  lastError: row.last_error ? JSON.parse(row.last_error) : undefined,
  createdAt: row.created_at,
  updatedAt: row.updated_at,
});
export const publicRecord = ({
  policyJson: _policyJson,
  policyHash: _policyHash,
  resourceJson: _resourceJson,
  appliedHash: _appliedHash,
  unit: _unit,
  ...record
}: StoredSandbox): SandboxRecord => record;

export class Registry {
  readonly db: Database.Database;
  unhealthy = false;
  constructor(path: string) {
    const marker = join(dirname(path), '.registry-initialized');
    if (!existsSync(path) && existsSync(marker))
      throw new ControlError(
        'unsupported_environment',
        'registry database missing after initialization; restore a consistent backup',
      );
    this.db = new Database(path, { fileMustExist: existsSync(path) });
    this.db.pragma('journal_mode = WAL');
    this.db.pragma('synchronous = FULL');
    this.db.pragma('foreign_keys = ON');
    this.db.pragma('busy_timeout = 3000');
    this.db.exec(`CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS sandboxes (
        id TEXT PRIMARY KEY, name TEXT, profile TEXT NOT NULL, state TEXT NOT NULL, desired_state TEXT NOT NULL,
        generation TEXT, unit TEXT, mcp_url TEXT NOT NULL, policy_json TEXT NOT NULL, policy_hash TEXT NOT NULL, resource_json TEXT NOT NULL,
        applied_hash TEXT, last_error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS operation_receipts (
        request_id TEXT PRIMARY KEY, operation TEXT NOT NULL, digest TEXT NOT NULL, sandbox_id TEXT,
        status TEXT NOT NULL, result TEXT,
        FOREIGN KEY(sandbox_id) REFERENCES sandboxes(id)
      );
      INSERT OR IGNORE INTO schema_migrations(version) VALUES(1)`);
    const version = (
      this.db.prepare('SELECT MAX(version) AS version FROM schema_migrations').get() as { version: number }
    ).version;
    if (version !== 1) throw new ControlError('unsupported_environment', 'unknown database schema');
    if (
      !(this.db.pragma('table_info(sandboxes)') as { name: string }[]).some((column) => column.name === 'resource_json')
    ) {
      throw new ControlError(
        'unsupported_environment',
        'legacy database lacks frozen resource policy; migration requires an explicit policy review',
      );
    }
    if (!existsSync(marker)) writeFileSync(marker, 'registry schema 1\n', { flag: 'wx', mode: 0o600 });
  }

  get(id: string): StoredSandbox | undefined {
    try {
      const row = this.db.prepare('SELECT * FROM sandboxes WHERE id = ?').get(id) as Row | undefined;
      return row && decode(row);
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  list(includeDeleted = false): StoredSandbox[] {
    try {
      return (
        this.db
          .prepare(`SELECT * FROM sandboxes ${includeDeleted ? '' : "WHERE state != 'deleted'"} ORDER BY created_at`)
          .all() as Row[]
      ).map(decode);
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  rebaseMcpUrls(baseUrl: string): void {
    try {
      const records = this.list(true);
      const update = this.db.prepare('UPDATE sandboxes SET mcp_url = ? WHERE id = ?');
      this.db.transaction(() => {
        for (const record of records) update.run(`${baseUrl}/sandboxes/${record.id}/mcp`, record.id);
      })();
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  put(record: StoredSandbox): void {
    const updatedAt = new Date().toISOString();
    try {
      this.db
        .prepare(`INSERT INTO sandboxes (id,name,profile,state,desired_state,generation,unit,mcp_url,policy_json,policy_hash,resource_json,applied_hash,last_error,created_at,updated_at)
      VALUES (@id,@name,@profile,@state,@desired_state,@generation,@unit,@mcp_url,@policy_json,@policy_hash,@resource_json,@applied_hash,@last_error,@created_at,@updated_at)
      ON CONFLICT(id) DO UPDATE SET state=excluded.state,desired_state=excluded.desired_state,generation=excluded.generation,unit=excluded.unit,
      resource_json=excluded.resource_json,applied_hash=excluded.applied_hash,last_error=excluded.last_error,updated_at=excluded.updated_at`)
        .run({
          id: record.id,
          name: record.name ?? null,
          profile: record.profile,
          state: record.state,
          desired_state: record.desiredState,
          generation: record.generation ?? null,
          unit: record.unit ?? null,
          mcp_url: record.mcpUrl,
          policy_json: record.policyJson,
          policy_hash: record.policyHash,
          resource_json: record.resourceJson,
          applied_hash: record.appliedHash ?? null,
          last_error: record.lastError ? JSON.stringify(record.lastError) : null,
          created_at: record.createdAt,
          updated_at: updatedAt,
        });
      record.updatedAt = updatedAt;
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  receipt(id: string): Receipt | undefined {
    try {
      return this.db.prepare('SELECT * FROM operation_receipts WHERE request_id=?').get(id) as Receipt | undefined;
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  begin(id: string, operation: string, input: unknown, sandboxId?: string): Receipt | undefined {
    if (!id || id.length > 256) throw new ControlError('conflict', 'invalid requestId');
    const digest = createHash('sha256').update(JSON.stringify({ operation, input })).digest('hex');
    const prior = this.receipt(id);
    if (prior) {
      if (prior.operation !== operation || prior.digest !== digest)
        throw new ControlError('conflict', 'requestId reused with different input');
      return prior;
    }
    try {
      this.db
        .prepare('INSERT INTO operation_receipts(request_id,operation,digest,sandbox_id,status) VALUES(?,?,?,?,?)')
        .run(id, operation, digest, sandboxId ?? null, 'pending');
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  finish(id: string, status: 'done' | 'failed' | 'interrupted', result: unknown, sandboxId?: string): void {
    try {
      this.db
        .prepare('UPDATE operation_receipts SET status=?,result=?,sandbox_id=COALESCE(sandbox_id,?) WHERE request_id=?')
        .run(status, JSON.stringify(result), sandboxId ?? null, id);
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  recoverReceipts(): void {
    try {
      this.db
        .prepare("UPDATE operation_receipts SET status='interrupted', result=? WHERE status='pending'")
        .run(JSON.stringify({ code: 'interrupted', message: 'daemon restarted before operation resolved' }));
    } catch (error) {
      this.unhealthy = true;
      throw error;
    }
  }
  close(): void {
    this.db.close();
  }
}
