// ============ 文件系统业务逻辑层 ============
import { join, extname } from "node:path";
import { stat, readFile } from "node:fs/promises";
import {
  loadGitignore, setGitignoreFilter, getGitignoreFilter,
  listDir, isTextFile, extToLang, getMime,
} from "../lib/workspace.js";

export class FileService {
  constructor(private docsDir: string) {}

  async ensureGitignoreLoaded() {
    if (!getGitignoreFilter()) {
      setGitignoreFilter(await loadGitignore(this.docsDir));
    }
  }

  validatePath(subPath: string): string {
    const fullPath = join(this.docsDir, subPath);
    if (!fullPath.startsWith(this.docsDir)) throw { status: 403, message: "非法路径" };
    return fullPath;
  }

  async getTree(subPath: string = "") {
    await this.ensureGitignoreLoaded();
    const targetDir = subPath ? join(this.docsDir, subPath) : this.docsDir;
    if (!targetDir.startsWith(this.docsDir)) return { error: "非法路径" };
    const nodes = await listDir(targetDir, this.docsDir);
    return nodes;
  }

  async getFile(filePath: string) {
    if (!filePath) return { error: "缺少 path 参数" };
    const fullPath = join(this.docsDir, filePath);
    if (!fullPath.startsWith(this.docsDir)) return { error: "非法路径" };

    let info;
    try {
      info = await stat(fullPath);
    } catch {
      return { error: "文件不存在" };
    }
    if (info.isDirectory()) return { error: "不能读取目录" };

    if (isTextFile(filePath)) {
      const text = await readFile(fullPath, "utf-8");
      return { content: text, language: extToLang(extname(filePath)), size: info.size, mtime: info.mtimeMs };
    }

    const buf = await readFile(fullPath);
    return { binary: buf, mime: getMime(filePath) };
  }

  async getStat(filePath: string) {
    if (!filePath) return { error: "缺少 path 参数" };
    const fullPath = join(this.docsDir, filePath);
    if (!fullPath.startsWith(this.docsDir)) return { error: "非法路径" };

    let info;
    try {
      info = await stat(fullPath);
    } catch {
      return { error: "文件不存在" };
    }
    if (info.isDirectory()) return { error: "不支持目录" };
    return { mtime: info.mtimeMs, size: info.size };
  }
}
