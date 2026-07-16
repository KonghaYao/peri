// ============ 工作区持久化 + 工具函数 + .gitignore + 文件树 ============
import { join, relative, extname, dirname } from "node:path";
import { readdir, stat, readFile } from "node:fs/promises";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import ignoreLib from "ignore";
import type { FileNode, WorkspaceState } from "../types.js";

// ---------- 工作区持久化 ----------

let workspaceFile = "";

export function initWorkspace(filePath: string) {
  workspaceFile = filePath;
  workspaceState = loadWorkspace();   // 重新加载，确保 workspace.json 内容生效
}

export function loadWorkspace(): WorkspaceState {
  try {
    if (workspaceFile && existsSync(workspaceFile)) {
      return JSON.parse(readFileSync(workspaceFile, "utf-8"));
    }
  } catch (e) { console.error("加载 workspace 失败:", e); }
  return { fileTree: { expandedDirs: [], activeFilePath: null }, ui: { sidebarWidth: 280, scmFlex: 3 } };
}

export function saveWorkspaceFile(state: WorkspaceState) {
  try {
    if (workspaceFile) writeFileSync(workspaceFile, JSON.stringify(state, null, 2), "utf-8");
  } catch (e) { console.error("保存 workspace 失败:", e); }
}

export function deepMerge(target: any, source: any): any {
  if (typeof source !== "object" || source === null || Array.isArray(source)) return source;
  if (typeof target !== "object" || target === null || Array.isArray(target)) return source;
  const result: any = { ...target };
  for (const key of Object.keys(source)) {
    result[key] = deepMerge(target[key], source[key]);
  }
  return result;
}

export let workspaceState: WorkspaceState = {
  fileTree: { expandedDirs: [], activeFilePath: null },
  ui: { sidebarWidth: 280, scmFlex: 3 },
};

// ---------- 文本 / MIME 工具 ----------

const TEXT_EXTS = new Set([
  ".md", ".txt", ".json", ".xml", ".yml", ".yaml", ".toml",
  ".js", ".ts", ".jsx", ".tsx", ".html", ".htm", ".css", ".scss", ".less",
  ".py", ".rb", ".go", ".rs", ".java", ".c", ".cpp", ".h", ".hpp",
  ".sh", ".bash", ".zsh", ".sql", ".graphql", ".svg",
  ".env", ".gitignore", ".dockerfile", ".makefile",
]);

export function isTextFile(filepath: string): boolean {
  const ext = extname(filepath).toLowerCase();
  if (ext === "" && !filepath.includes(".")) return true;
  return TEXT_EXTS.has(ext);
}

export function extToLang(ext: string): string {
  const map: Record<string, string> = {
    ".md": "markdown", ".js": "javascript", ".ts": "typescript",
    ".jsx": "jsx", ".tsx": "tsx", ".json": "json", ".html": "html",
    ".css": "css", ".py": "python", ".rb": "ruby", ".go": "go",
    ".rs": "rust", ".java": "java", ".c": "c", ".cpp": "cpp",
    ".sh": "bash", ".sql": "sql", ".xml": "xml", ".yaml": "yaml",
    ".yml": "yaml", ".toml": "toml", ".graphql": "graphql",
    ".scss": "scss", ".less": "less",
  };
  return map[ext.toLowerCase()] || "";
}

export function getMime(filepath: string): string {
  const ext = extname(filepath).toLowerCase();
  const mimes: Record<string, string> = {
    ".html": "text/html; charset=utf-8", ".htm": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8", ".js": "application/javascript; charset=utf-8",
    ".json": "application/json; charset=utf-8", ".png": "image/png",
    ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif",
    ".svg": "image/svg+xml", ".webp": "image/webp", ".ico": "image/x-icon",
    ".pdf": "application/pdf", ".woff2": "font/woff2", ".woff": "font/woff",
  };
  return mimes[ext] || "text/plain; charset=utf-8";
}

// ---------- .gitignore 加载 ----------

export async function loadGitignore(docsDir: string): Promise<ignoreLib.Ignore> {
  const ig = ignoreLib();
  ig.add([".git", "node_modules"]);

  async function walk(dir: string): Promise<void> {
    let entries;
    try { entries = await readdir(dir, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      if (entry.name === ".git" || entry.name === "node_modules") continue;
      const fullPath = join(dir, entry.name);
      if (entry.name === ".gitignore" && entry.isFile()) {
        const content = await readFile(fullPath, "utf-8");
        const relDir = relative(docsDir, dirname(fullPath));
        ig.add(relDir ? content.split("\n").map(line => {
          if (line.startsWith("!") || line.trim() === "") return line;
          return relDir + "/" + line;
        }).join("\n") : content);
      } else if (entry.isDirectory()) {
        await walk(fullPath);
      }
    }
  }

  await walk(docsDir);
  return ig;
}

// ---------- 文件树 ----------

const GITIGNORE_ENTRY_COUNT: { filter: ignoreLib.Ignore | null } = { filter: null };

export function setGitignoreFilter(filter: ignoreLib.Ignore | null) {
  GITIGNORE_ENTRY_COUNT.filter = filter;
}

export function getGitignoreFilter() {
  return GITIGNORE_ENTRY_COUNT.filter;
}

export async function hasVisibleChildren(dirPath: string, docsDir: string): Promise<boolean> {
  try {
    const entries = await readdir(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      const relPath = relative(docsDir, join(dirPath, entry.name));
      if (GITIGNORE_ENTRY_COUNT.filter?.ignores(relPath + (entry.isDirectory() ? "/" : ""))) continue;
      if (entry.isDirectory()) {
        if (await hasVisibleChildren(join(dirPath, entry.name), docsDir)) return true;
      } else {
        return true;
      }
    }
  } catch { /* ignore */ }
  return false;
}

export async function listDir(dirPath: string, docsDir: string): Promise<FileNode[]> {
  let entries;
  try {
    entries = await readdir(dirPath, { withFileTypes: true });
  } catch (e: any) {
    if (e?.code === 'ENOENT') return [];   // 目录不存在时返回空数组
    throw e;
  }
  const nodes: FileNode[] = [];

  for (const entry of entries) {
    const relPath = relative(docsDir, join(dirPath, entry.name));
    if (GITIGNORE_ENTRY_COUNT.filter?.ignores(relPath + (entry.isDirectory() ? "/" : ""))) continue;
    if (entry.isDirectory()) {
      nodes.push({
        name: entry.name, path: relPath, type: "directory",
        hasChildren: await hasVisibleChildren(join(dirPath, entry.name), docsDir),
      });
    } else {
      nodes.push({ name: entry.name, path: relPath, type: "file" });
    }
  }

  nodes.sort((a, b) => {
    if (a.type !== b.type) return a.type === "directory" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  return nodes;
}

// ---------- 子键读写（供子键路由调用） ----------

export function getWorkspaceKey(key: string) {
  return (workspaceState as any)[key];
}

export function setWorkspaceKey(key: string, patch: any) {
  const current = (workspaceState as any)[key] ?? {};
  // 对象 deepMerge；primitive / array 直接覆盖
  (workspaceState as any)[key] = deepMerge(current, patch);
  saveWorkspaceFile(workspaceState);
  return (workspaceState as any)[key];
}

export function deleteWorkspaceKey(key: string) {
  delete (workspaceState as any)[key];
  saveWorkspaceFile(workspaceState);
}
