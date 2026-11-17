import { Database } from 'bun:sqlite';
import { chmodSync, closeSync, lstatSync, openSync, type Stats } from 'node:fs';
import path from 'node:path';
import { ensurePrivateDirectory } from '../filesystem';

const DATABASE_FILE_MODE = 0o600;

function isMemoryDatabase(databasePath: string): boolean {
  return databasePath === ':memory:' || databasePath.startsWith('file::memory:');
}

function lstatIfExists(file: string): Stats | undefined {
  return lstatSync(file, { throwIfNoEntry: false });
}

export function secureDatabaseFiles(databasePath: string): void {
  if (isMemoryDatabase(databasePath)) return;
  for (const candidate of [databasePath, `${databasePath}-wal`, `${databasePath}-shm`]) {
    const stat = lstatIfExists(candidate);
    if (stat === undefined) continue;
    // lstat also rejects dangling symlinks, which existsSync would silently follow as missing.
    if (!stat.isFile() || stat.isSymbolicLink())
      throw new Error('Database files must be regular private files');
    chmodSync(candidate, DATABASE_FILE_MODE);
  }
}

export function openDatabase(databasePath: string): Database {
  if (!isMemoryDatabase(databasePath)) {
    ensurePrivateDirectory(path.dirname(databasePath));
    secureDatabaseFiles(databasePath);
    try {
      closeSync(openSync(databasePath, 'wx', DATABASE_FILE_MODE));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    }
    secureDatabaseFiles(databasePath);
  }

  const db = new Database(databasePath, { create: true });
  try {
    db.exec('PRAGMA journal_mode = WAL;');
    db.exec('PRAGMA foreign_keys = ON;');
    db.exec('PRAGMA busy_timeout = 5000;');
    secureDatabaseFiles(databasePath);
    return db;
  } catch (error) {
    db.close();
    throw error;
  }
}
