import { stat } from 'node:fs/promises';
import { resolve } from 'node:path';
import { connect } from '@tursodatabase/database';

export class DatabaseFileError extends Error {}

/**
 * 只接受已经存在、解析后指向普通文件的路径；不创建目标。
 *
 * 驱动侧的 `fileMustExist` 仍会在真正打开时再兜底，避免检查与打开之间的竞态
 * 让缺失文件被创建。
 */
export async function validateDatabaseFile(input: string): Promise<string> {
  const path = resolve(input);
  let info;
  try {
    info = await stat(path);
  } catch {
    throw new DatabaseFileError(`数据库文件不存在: ${path}`);
  }
  if (!info.isFile()) throw new DatabaseFileError(`数据库路径不是普通文件: ${path}`);
  return path;
}

/** 启动时用只读连接确认文件可作为 SQLite 打开，不触发写入。 */
export async function verifyDatabaseFile(input: string): Promise<string> {
  const path = await validateDatabaseFile(input);
  try {
    const database = await connect(path, { readonly: true, fileMustExist: true });
    await database.close();
  } catch {
    throw new DatabaseFileError(`无法作为 SQLite 数据库打开: ${path}`);
  }
  return path;
}
