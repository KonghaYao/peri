import { lstat, open, stat, unlink, type FileHandle } from 'node:fs/promises';
import { connect, type Database } from '@tursodatabase/database';
import { MAX_RESULT_ROWS } from './constants.js';
import { assertNoExternalFileTarget, assertSingleStatement } from './single-statement.js';
import { toJsonRow, type JsonCell } from './values.js';

export type BindValue = string | number | boolean | null;

export type ExecuteResult =
  | {
      kind: 'rows';
      columns: string[];
      rows: JsonCell[][];
      returnedRowCount: number;
      totalRowCount: number;
      truncated: boolean;
    }
  | {
      kind: 'none';
      columns: string[];
      rows: JsonCell[][];
      returnedRowCount: number;
      totalRowCount: number;
      truncated: boolean;
      changes: number;
    };

export interface TableInfo {
  name: string;
  type: 'table' | 'view';
}

export interface ColumnInfo {
  name: string;
  type: string;
  notNull: boolean;
  primaryKey: boolean;
  primaryKeyPosition: number;
  defaultValue: string | null;
}

export interface TableDescription {
  name: string;
  type: 'table' | 'view';
  sql: string | null;
  columns: ColumnInfo[];
}

export class TableNotFoundError extends Error {}

async function pathExists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch {
    return false;
  }
}

interface OwnedWalSidecar {
  path: string;
  dev: bigint;
  ino: bigint;
}

/**
 * 驱动 0.8.3 的 fileMustExist 会同时关闭主文件与 `-wal` 的 Create 标志；标准
 * delete-journal 库首次读写打开因缺少 `-wal` 报 `entity not found`。这里只在主
 * 文件存在、无 `-journal` 且 `-wal` 不存在时原子创建一个空 `-wal`，然后用
 * fileMustExist 打开主文件。主文件仍由驱动 Create=false 原子保护；不覆盖已有
 * 侧车，失败时只清理自己创建且仍为空的文件。
 */
async function ensureWalSidecar(databasePath: string): Promise<OwnedWalSidecar | undefined> {
  let mainInfo;
  try {
    mainInfo = await stat(databasePath);
  } catch {
    return undefined;
  }
  if (!mainInfo.isFile()) return undefined;
  if (await pathExists(`${databasePath}-journal`)) return undefined;

  const walPath = `${databasePath}-wal`;
  let handle: FileHandle;
  try {
    handle = await open(walPath, 'wx', mainInfo.mode & 0o666 || 0o600);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'EEXIST') return undefined;
    throw error;
  }
  try {
    const info = await handle.stat({ bigint: true });
    return { path: walPath, dev: info.dev, ino: info.ino };
  } finally {
    await handle.close();
  }
}

async function cleanupOwnedWalSidecar(sidecar: OwnedWalSidecar): Promise<void> {
  try {
    const info = await lstat(sidecar.path, { bigint: true });
    if (info.isFile() && info.dev === sidecar.dev && info.ino === sidecar.ino && info.size === 0n) {
      await unlink(sidecar.path);
    }
  } catch {
    // 文件已消失、被替换或已被驱动写入时保持现状。
  }
}

async function openDatabase(databasePath: string, readonly: boolean): Promise<Database> {
  if (readonly) return connect(databasePath, { readonly: true, fileMustExist: true });

  const sidecar = await ensureWalSidecar(databasePath);
  try {
    return await connect(databasePath, { readonly: false, fileMustExist: true });
  } catch (error) {
    if (sidecar !== undefined) await cleanupOwnedWalSidecar(sidecar);
    throw error;
  }
}

/** 每次调用独立打开连接，结束即关闭；跨调用的连接与事务状态不会保留。 */
async function withDatabase<T>(
  databasePath: string,
  readonly: boolean,
  run: (database: Database) => Promise<T>,
): Promise<T> {
  const database = await openDatabase(databasePath, readonly);
  try {
    return await run(database);
  } finally {
    await database.close();
  }
}

async function queryAll(database: Database, sql: string, params: readonly BindValue[] = []): Promise<unknown[]> {
  const statement = await database.prepare(sql);
  try {
    return await statement.all(...params);
  } finally {
    statement.close();
  }
}

/**
 * 执行一条 SQL。有列元数据就按行读取并完整消费游标，避免 RETURNING 写入
 * 在截断处被提前结束；只保留前 MAX_RESULT_ROWS 行到响应。
 */
export async function executeSql(
  databasePath: string,
  sql: string,
  params: readonly BindValue[] = [],
): Promise<ExecuteResult> {
  assertSingleStatement(sql);
  assertNoExternalFileTarget(sql);
  return withDatabase(databasePath, false, async (database) => {
    const statement = await database.prepare(sql);
    try {
      const columns = statement.columns().map((column) => column.name);
      if (columns.length > 0) {
        statement.raw(true);
        statement.safeIntegers(true);
        const rows: JsonCell[][] = [];
        let totalRowCount = 0;
        for await (const rawRow of statement.iterate(...params)) {
          totalRowCount++;
          if (rows.length < MAX_RESULT_ROWS) rows.push(toJsonRow(rawRow as readonly unknown[]));
        }
        return {
          kind: 'rows',
          columns,
          rows,
          returnedRowCount: rows.length,
          totalRowCount,
          truncated: totalRowCount > rows.length,
        };
      }

      const result = await statement.run(...params);
      return {
        kind: 'none',
        columns: [],
        rows: [],
        returnedRowCount: 0,
        totalRowCount: 0,
        truncated: false,
        changes: result.changes,
      };
    } finally {
      statement.close();
    }
  });
}

/**
 * 列出主数据库中的用户表和视图；SQLite 与 Turso 的内部对象不进入发现面。
 *
 * Turso 的 AUTOINCREMENT 会在 sqlite_schema 中建 `__turso_internal_*` 序列表，
 * 它不属于用户模型，按驱动前缀排除。
 */
export async function listTables(databasePath: string): Promise<TableInfo[]> {
  return withDatabase(databasePath, true, async (database) => {
    const rows = (await queryAll(
      database,
      `SELECT name, type FROM sqlite_schema
       WHERE type IN ('table', 'view')
         AND name NOT GLOB 'sqlite_*' AND name NOT GLOB '__turso_internal_*'
       ORDER BY type, name`,
    )) as Array<Record<string, unknown>>;
    return rows.flatMap((row) => {
      const name = row.name;
      const type = row.type;
      if (typeof name !== 'string' || (type !== 'table' && type !== 'view')) return [];
      return [{ name, type }];
    });
  });
}

/** 使用 PRAGMA 表值函数绑定表名，不把调用方输入拼进第二条 SQL。 */
export async function describeTable(databasePath: string, table: string): Promise<TableDescription> {
  return withDatabase(databasePath, true, async (database) => {
    const metadata = (await queryAll(
      database,
      `SELECT type, sql FROM sqlite_schema
       WHERE name = ? AND type IN ('table', 'view')
         AND name NOT GLOB 'sqlite_*' AND name NOT GLOB '__turso_internal_*'`,
      [table],
    )) as Array<Record<string, unknown>>;
    const object = metadata[0];
    if (object === undefined || (object.type !== 'table' && object.type !== 'view')) throw new TableNotFoundError();

    const fields = (await queryAll(database, 'SELECT * FROM pragma_table_info(?)', [table])) as Array<
      Record<string, unknown>
    >;
    const columns = fields.map((field) => ({
      name: typeof field.name === 'string' ? field.name : '',
      type: typeof field.type === 'string' ? field.type : '',
      notNull: Number(field.notnull) !== 0,
      primaryKey: Number(field.pk) > 0,
      primaryKeyPosition: Number(field.pk),
      defaultValue: typeof field.dflt_value === 'string' ? field.dflt_value : null,
    }));

    return {
      name: table,
      type: object.type,
      sql: typeof object.sql === 'string' ? object.sql : null,
      columns,
    };
  });
}
