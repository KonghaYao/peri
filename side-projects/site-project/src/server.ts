// ============ site-project 入口 ============
import { Hono } from "hono";
import { join, resolve, dirname } from "node:path";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { serve } from "@hono/node-server";

import { initWorkspace } from "./lib/workspace.js";
import { FileService } from "./services/file-service.js";
import { ScmService } from "./services/scm-service.js";
import { GraphService } from "./services/graph-service.js";
import { GetmanService } from "./services/getman-service.js";
import { WorkspaceService } from "./services/workspace-service.js";
import { registerFileRoutes } from "./routes/files.js";
import { registerScmRoutes } from "./routes/scm.js";
import { registerGraphRoutes } from "./routes/graph.js";
import { registerGetmanRoutes } from "./routes/getman.js";
import { registerWorkspaceRoutes } from "./routes/workspace.js";
import { setupTerminal, terminalSessions } from "./terminal.js";

// ---------- 配置 ----------
const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const PORT = 23566;
const PUBLIC_DIR = join(ROOT, "public");
const DOCS_ARG = process.argv[2] || join(ROOT, "docs");
const DOCS_DIR = resolve(DOCS_ARG);

if (!existsSync(DOCS_DIR)) {
  console.error(`\u274C 目录不存在: ${DOCS_DIR}`);
  process.exit(1);
}

// ---------- 初始化 ----------
const WORKSPACE_FILE = join(ROOT, "workspace.json");
initWorkspace(WORKSPACE_FILE);

// ---------- Service 层 ----------
const fileService = new FileService(DOCS_DIR);
const scmService = new ScmService(DOCS_DIR);
const graphService = new GraphService(DOCS_DIR);
const getmanService = new GetmanService();
const workspaceService = new WorkspaceService(terminalSessions);

// ---------- 应用 + 路由 ----------
const app = new Hono();
registerFileRoutes(app, fileService);
registerScmRoutes(app, scmService);
registerGraphRoutes(app, graphService);
registerGetmanRoutes(app, getmanService);
registerWorkspaceRoutes(app, workspaceService, PUBLIC_DIR);

// ---------- 启动 ----------
const nodeServer = serve({ fetch: app.fetch, port: PORT });
setupTerminal(nodeServer, PORT);

console.log(`\u{1F680} Server running at http://localhost:${PORT}`);
console.log(`\u{1F4C1} Docs directory: ${DOCS_DIR}`);
console.log(`\u2713 Terminal ready (via :${PORT}/ws)`);
