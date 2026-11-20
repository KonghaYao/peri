import type { Database } from 'bun:sqlite';
import type { EncryptedSecret } from '../hashing';
import { newPlatformId } from '../ids';
import { EMPTY_JSON_OBJECT } from '../json';
import type { ListQuery } from './shared';
import { buildListStatement, nowMillis, removeById } from './shared';

export interface PlatformVaultRow {
  readonly id: string;
  readonly name: string;
  readonly metadata: string;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformVault {
  readonly id?: string;
  readonly name: string;
  readonly metadata?: string;
}

export interface PlatformVaultUpdate {
  readonly name: string | null;
  readonly metadata: string | null;
}

const VAULT_COLUMNS = 'id, name, metadata, created_at, updated_at';

export interface PlatformVaultsRepositoryOptions {
  readonly now?: () => number;
}

export class PlatformVaultsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformVaultsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  list(query: ListQuery): PlatformVaultRow[] {
    const { sql, params } = buildListStatement('platform_vaults', VAULT_COLUMNS, query);
    return this.db.query<PlatformVaultRow, (string | number)[]>(sql).all(...params);
  }

  get(id: string): PlatformVaultRow | null {
    const row = this.db
      .query<PlatformVaultRow, [string]>(`SELECT ${VAULT_COLUMNS} FROM platform_vaults WHERE id = ?`)
      .get(id);
    return row ?? null;
  }

  create(input: NewPlatformVault): PlatformVaultRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_vaults (id, name, metadata, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)`,
      )
      .run(id, input.name, input.metadata ?? EMPTY_JSON_OBJECT, now, now);
    const row = this.get(id);
    if (!row) throw new Error(`Created vault ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformVaultUpdate): PlatformVaultRow | null {
    this.db
      .query(
        `UPDATE platform_vaults SET
          name = COALESCE(?, name),
          metadata = COALESCE(?, metadata),
          updated_at = ?
        WHERE id = ?`,
      )
      .run(input.name, input.metadata, nowMillis(this.#now), id);
    return this.get(id);
  }

  /** Deletes the vault and its credentials atomically; credentials never outlive their vault. */
  delete(id: string): boolean {
    const removeVault = this.db.transaction((vaultId: string) => {
      this.db.query('DELETE FROM platform_credentials WHERE vault_id = ?').run(vaultId);
      return removeById(this.db, 'platform_vaults', vaultId);
    });
    return removeVault(id);
  }
}

export type PlatformCredentialAuthKind = 'static_bearer' | 'environment_variable';

export interface PlatformCredentialRow {
  readonly id: string;
  readonly vault_id: string;
  readonly name: string;
  readonly auth_kind: PlatformCredentialAuthKind;
  readonly auth: string;
  readonly secret_cipher: Uint8Array;
  readonly secret_iv: Uint8Array;
  readonly secret_tag: Uint8Array;
  readonly key_id: string;
  readonly metadata: string;
  readonly created_at: number;
  readonly updated_at: number;
}

export interface NewPlatformCredential {
  readonly id?: string;
  readonly vaultId: string;
  readonly name: string;
  readonly authKind: PlatformCredentialAuthKind;
  readonly auth: string;
  readonly secret: EncryptedSecret;
  readonly metadata?: string;
}

export interface PlatformCredentialUpdate {
  readonly name: string | null;
  readonly authKind: PlatformCredentialAuthKind | null;
  readonly auth: string | null;
  readonly secret: EncryptedSecret | null;
  readonly metadata: string | null;
}

const CREDENTIAL_COLUMNS =
  'id, vault_id, name, auth_kind, auth, secret_cipher, secret_iv, secret_tag, key_id, metadata, created_at, updated_at';

export class PlatformCredentialsRepository {
  readonly db: Database;
  readonly #now: () => number;

  constructor(db: Database, options: PlatformVaultsRepositoryOptions = {}) {
    this.db = db;
    this.#now = options.now ?? Date.now;
  }

  /**
   * Without a page request the vault's credentials come back in creation order; with one,
   * IDs are used as the keyset cursor so `after` stays consistent with `order`.
   */
  listByVault(vaultId: string, query?: ListQuery): PlatformCredentialRow[] {
    if (query === undefined) {
      return this.db
        .query<PlatformCredentialRow, [string]>(
          `SELECT ${CREDENTIAL_COLUMNS} FROM platform_credentials
           WHERE vault_id = ? ORDER BY created_at ASC, id ASC`,
        )
        .all(vaultId);
    }
    const direction = query.order === 'asc' ? 'ASC' : 'DESC';
    const params: (string | number)[] = [vaultId];
    let sql = `SELECT ${CREDENTIAL_COLUMNS} FROM platform_credentials WHERE vault_id = ?`;
    if (query.after !== null) {
      sql += ` AND id ${query.order === 'asc' ? '>' : '<'} ?`;
      params.push(query.after);
    }
    sql += ` ORDER BY id ${direction} LIMIT ?`;
    params.push(query.limit);
    return this.db.query<PlatformCredentialRow, (string | number)[]>(sql).all(...params);
  }

  get(id: string): PlatformCredentialRow | null {
    const row = this.db
      .query<PlatformCredentialRow, [string]>(
        `SELECT ${CREDENTIAL_COLUMNS} FROM platform_credentials WHERE id = ?`,
      )
      .get(id);
    return row ?? null;
  }

  /** Reads a credential only when it belongs to the addressed vault. */
  getForVault(vaultId: string, id: string): PlatformCredentialRow | null {
    const row = this.db
      .query<PlatformCredentialRow, [string, string]>(
        `SELECT ${CREDENTIAL_COLUMNS} FROM platform_credentials WHERE id = ? AND vault_id = ?`,
      )
      .get(id, vaultId);
    return row ?? null;
  }

  create(input: NewPlatformCredential): PlatformCredentialRow {
    const id = input.id ?? newPlatformId();
    const now = nowMillis(this.#now);
    this.db
      .query(
        `INSERT INTO platform_credentials (
          id, vault_id, name, auth_kind, auth, secret_cipher, secret_iv, secret_tag, key_id,
          metadata, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        id,
        input.vaultId,
        input.name,
        input.authKind,
        input.auth,
        input.secret.cipher,
        input.secret.iv,
        input.secret.tag,
        input.secret.keyId,
        input.metadata ?? EMPTY_JSON_OBJECT,
        now,
        now,
      );
    const row = this.get(id);
    if (!row) throw new Error(`Created credential ${id} could not be read back`);
    return row;
  }

  update(id: string, input: PlatformCredentialUpdate): PlatformCredentialRow | null {
    this.db
      .query(
        `UPDATE platform_credentials SET
          name = COALESCE(?, name),
          auth_kind = COALESCE(?, auth_kind),
          auth = COALESCE(?, auth),
          secret_cipher = COALESCE(?, secret_cipher),
          secret_iv = COALESCE(?, secret_iv),
          secret_tag = COALESCE(?, secret_tag),
          key_id = COALESCE(?, key_id),
          metadata = COALESCE(?, metadata),
          updated_at = ?
        WHERE id = ?`,
      )
      .run(
        input.name,
        input.authKind,
        input.auth,
        input.secret?.cipher ?? null,
        input.secret?.iv ?? null,
        input.secret?.tag ?? null,
        input.secret?.keyId ?? null,
        input.metadata,
        nowMillis(this.#now),
        id,
      );
    return this.get(id);
  }

  delete(id: string): boolean {
    return removeById(this.db, 'platform_credentials', id);
  }

  /** Deletes a credential only when it belongs to the addressed vault. */
  deleteForVault(vaultId: string, id: string): boolean {
    return (
      this.db.query('DELETE FROM platform_credentials WHERE id = ? AND vault_id = ?').run(id, vaultId)
        .changes > 0
    );
  }
}
