// ============ 文件系统 ============

export interface FileNode {
  name: string;
  path: string;
  type: "file" | "directory";
  hasChildren?: boolean;
  children?: FileNode[];
}

// ============ 工作区 ============

export interface WorkspaceState {
  fileTree: { expandedDirs: string[]; activeFilePath: string | null };
  ui: { sidebarWidth: number; scmFlex: number; terminalFlex?: number; theme?: string };
}

export interface TerminalSessionMeta {
  id: string;
  cols: number;
  rows: number;
  createdAt: number;
}

export interface TerminalSession {
  proc: any;          // pty.IPty
  cols: number;
  rows: number;
  buffer: string[];   // ring buffer 用于重连回放
  createdAt: number;
  alive: boolean;
  ws: any;            // WebSocket 实例
}

// ============ SCM ============

export interface GitResult {
  stdout: string;
  stderr: string;
  exitCode: number;
}

export interface ScmFile {
  path: string;
  index: string;
  worktree: string;
}

export interface ScmStatus {
  branch: string;
  ahead: number;
  behind: number;
  staged: ScmFile[];
  unstaged: ScmFile[];
}

// ============ Git Graph ============

export interface GraphCommit {
  hash: string;
  shortHash: string;
  parents: string[];
  author: string;
  date: string;
  subject: string;
  refs: string[];
  head: boolean;
  uncommitted?: boolean;
  uncommittedFiles?: string[];
  uncommittedStats?: string;
  stash?: boolean;
  stashIndex?: number;
}

// ============ Getman ============

export interface ParsedCurl {
  method: string;
  url: string;
  headers: Record<string, string>;
  body?: string;
  bodyType: "none" | "json" | "x-www-form-urlencoded" | "form-data";
  params: Array<{ key: string; value: string; enabled: boolean }>;
  formFields: Array<{ key: string; value: string; enabled: boolean }>;
  authType: "none" | "bearer" | "basic";
  authBasicUser: string;
  authBasicPass: string;
  authBearer: string;
}

export interface GetmanProxyBody {
  method: string;
  url: string;
  headers: Record<string, string>;
  body: string | null;
  formFields: Array<{ key: string; value: string; enabled?: boolean }> | null;
}
