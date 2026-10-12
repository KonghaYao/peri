/**
 * 浏览器虚拟文件系统（VFS）：一棵挂在 IndexedDB 上的文件树，刷新页面或重开标签页
 * 都能读回上次写入的内容。底层是 lightning-fs（MIT），目录树与文件分别是
 * `<库名>_files` 商店里的 `!root` 与 inode 记录。
 *
 * 对外只暴露本应用用得到的文本文件语义（读、写、删、列目录），不把底层 Node 风格
 * API 外溢；路径一律是以 `/` 开头的绝对路径。
 *
 * 落盘时机：底层默认靠 500ms 去抖或空闲 500ms 才提交目录树，写完立刻关页面会丢
 * 目录；因此每次变更都显式 `flush()`，让「写入返回即已落盘」在调用方成立。
 */

import LightningFS from "@isomorphic-git/lightning-fs";

/** IndexedDB 数据库名；底层在其下建 `<库名>_files` 商店。 */
export const VFS_NAME = "peri-vfs";

/**
 * 浏览器侧文件系统：所有方法都是异步的（IndexedDB 没有同步读），
 * 失败照常抛出，只有「文件不存在」被收敛成返回值。
 */
export interface Vfs {
  /** 读文本文件；文件不存在返回 null，其它失败抛出。 */
  readText(path: string): Promise<string | null>;
  /** 写文本文件，父目录按需创建；返回时文件与目录树都已落盘。 */
  writeText(path: string, text: string): Promise<void>;
  /** 删除文件；文件不存在时抛出。 */
  deleteFile(path: string): Promise<void>;
  /** 列出目录下的条目名（按字典序）；目录不存在时抛出。 */
  list(dir: string): Promise<string[]>;
}

/** 底层的错误以 `code` 携带 errno 文本。 */
function codeOf(error: unknown): string {
  return error instanceof Error ? String((error as { code?: unknown }).code) : "";
}

/** 路径约定是绝对路径；越界传入在这里就以明确错误拦下，而不是让底层抛出难懂的错误。 */
function assertAbsolute(path: string): void {
  if (!path.startsWith("/") || path.endsWith("/")) throw new Error(`VFS 路径必须是绝对路径且不带结尾斜杠：${path}`);
}

/** POSIX 父目录；根目录下的文件返回 `/`。 */
function parentDirectory(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

/** 逐级建目录（底层 `mkdir` 不递归，且不建父目录）。 */
async function ensureDirectory(fs: LightningFS, directory: string): Promise<void> {
  let current = "";
  for (const part of directory.split("/").filter(Boolean)) {
    current += `/${part}`;
    try {
      await fs.promises.mkdir(current);
    } catch (error) {
      if (codeOf(error) !== "EEXIST") throw error;
    }
  }
}

export function createVfs(name: string = VFS_NAME): Vfs {
  const fs = new LightningFS(name);
  return {
    async readText(path) {
      assertAbsolute(path);
      try {
        return await fs.promises.readFile(path, "utf8");
      } catch (error) {
        if (codeOf(error) === "ENOENT") return null;
        throw error;
      }
    },
    async writeText(path, text) {
      assertAbsolute(path);
      const directory = parentDirectory(path);
      if (directory !== "/") await ensureDirectory(fs, directory);
      await fs.promises.writeFile(path, text);
      await fs.promises.flush();
    },
    async deleteFile(path) {
      assertAbsolute(path);
      await fs.promises.unlink(path);
      await fs.promises.flush();
    },
    async list(dir) {
      assertAbsolute(dir);
      return (await fs.promises.readdir(dir)).sort();
    },
  };
}
