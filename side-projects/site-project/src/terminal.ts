// ============ 终端 WebSocket + PTY ============
import { WebSocketServer, WebSocket } from "ws";
import * as pty from "node-pty";
import { execSync } from "node:child_process";
import type { Server } from "node:http";
import type { TerminalSession } from "./types.js";

const MAX_BUFFER_LINES = 5000;
const STATS_INTERVAL_MS = 2000;

export const terminalSessions = new Map<string, TerminalSession>();

const wsDataMap = new WeakMap<WebSocket, { session?: TerminalSession }>();

function createSession(ws: WebSocket, cols: number, rows: number): TerminalSession {
  const proc = pty.spawn(process.env.SHELL || "/bin/bash", ["-i"], {
    name: "xterm-256color",
    cols,
    rows,
    env: { ...process.env, TERM: "xterm-256color" },
  });

  const session: TerminalSession = {
    proc,
    cols,
    rows,
    buffer: [],
    createdAt: Date.now(),
    alive: true,
    ws,
  };

  proc.onData((data: string) => {
    session.buffer.push(data);
    if (session.buffer.length > MAX_BUFFER_LINES) session.buffer.shift();
    if (session.ws && session.ws.readyState === WebSocket.OPEN) session.ws.send(data);
  });

  proc.onExit(() => {
    session.alive = false;
    try { session.ws?.close(); } catch {}
  });

  // 进程监控：每 STATS_INTERVAL_MS 收集 CPU/Mem 并推送给前端
  const statsTimer = setInterval(() => {
    if (!session.alive) { clearInterval(statsTimer); return; }
    const stats = getProcessStats(proc.pid);
    if (stats && session.ws && session.ws.readyState === WebSocket.OPEN) {
      try { session.ws.send(JSON.stringify({ type: "stats", ...stats })); } catch {}
    }
  }, STATS_INTERVAL_MS);

  return session;
}

/** 收集进程及其子进程的 CPU% + RSS */
function getProcessStats(pid: number): { cpu: number; memKB: number } | null {
  try {
    // 获取所有子孙 PID
    const pids = getDescendantPids(pid);
    if (pids.length === 0) return null;

    let totalCpu = 0;
    let totalRss = 0;

    for (const p of pids) {
      const out = execSync(
        `ps -p ${p} -o pcpu= -o rss= 2>/dev/null`,
        { timeout: 500, encoding: "utf-8" }
      ).trim();
      if (!out) continue;
      const parts = out.split(/\s+/);
      totalCpu += parseFloat(parts[0]) || 0;
      totalRss += parseInt(parts[1], 10) || 0;
    }

    return { cpu: Math.round(totalCpu * 10) / 10, memKB: totalRss };
  } catch {
    return null;
  }
}

/** 递归获取进程的所有子孙 PID */
function getDescendantPids(pid: number): number[] {
  try {
    const out = execSync(
      `pgrep -P ${pid} 2>/dev/null`,
      { timeout: 500, encoding: "utf-8" }
    ).trim();
    const children = out ? out.split("\n").map(Number).filter((n) => n > 0) : [];
    const all = [pid, ...children];
    for (const c of children) {
      all.push(...getDescendantPids(c).filter((p) => p !== pid && !all.includes(p)));
    }
    return [...new Set(all)];
  } catch {
    return [pid];
  }
}

export function setupTerminal(nodeServer: Server, port: number) {
  const wss = new WebSocketServer({ noServer: true });

  wss.on("connection", (ws, req) => {
    const url = new URL(req.url!, `http://localhost:${port}`);
    const sessionId = url.searchParams.get("session") || undefined;
    const cols = parseInt(url.searchParams.get("cols") || "80");
    const rows = parseInt(url.searchParams.get("rows") || "24");

    let session: TerminalSession;

    // 尝试重连已有终端会话
    if (sessionId && terminalSessions.has(sessionId)) {
      const existing = terminalSessions.get(sessionId)!;
      if (existing.alive) {
        // 回放缓冲
        for (const line of existing.buffer) {
          if (ws.readyState === WebSocket.OPEN) ws.send(line);
        }
        existing.ws = ws;
        existing.cols = cols;
        existing.rows = rows;
        try { existing.proc.resize(cols, rows); } catch {}
        session = existing;
      } else {
        session = createSession(ws, cols, rows);
        const newId = crypto.randomUUID();
        terminalSessions.set(newId, session);
      }
    } else {
      session = createSession(ws, cols, rows);
      const newId = sessionId || crypto.randomUUID();
      terminalSessions.set(newId, session);
    }

    wsDataMap.set(ws, { session });

    // 通知前端 session ID（仅新会话发送）
    if (!sessionId || !terminalSessions.has(sessionId)) {
      const sid = [...terminalSessions.entries()].find(([, v]) => v === session)?.[0];
      if (sid) try { ws.send(JSON.stringify({ type: "session", id: sid })); } catch {}
    }

    // 输入处理：ws 默认发送 Buffer，需同时处理 string 和 Buffer
    ws.on("message", (msg) => {
      const raw = typeof msg === "string" ? msg
        : Buffer.isBuffer(msg) ? msg.toString("utf-8")
        : Array.isArray(msg) ? Buffer.concat(msg).toString("utf-8")
        : String(msg);

      const s = wsDataMap.get(ws)?.session;
      if (!s?.alive) return;

      // 检测 resize 控制消息
      try {
        const parsed = JSON.parse(raw);
        if (parsed.type === "resize" && typeof parsed.cols === "number" && typeof parsed.rows === "number") {
          s.cols = parsed.cols;
          s.rows = parsed.rows;
          try { s.proc.resize(parsed.cols, parsed.rows); } catch {}
          return;
        }
      } catch {}

      // 普通输入 → PTY
      try { s.proc.write(raw); } catch {}
    });

    ws.on("close", () => {
      const s = wsDataMap.get(ws)?.session;
      if (s) s.ws = null;
      wsDataMap.delete(ws);
    });

    ws.on("error", () => {
      const s = wsDataMap.get(ws)?.session;
      if (s) s.ws = null;
      wsDataMap.delete(ws);
    });
  });

  // WebSocket 升级路由
  nodeServer.on("upgrade", (request, socket, head) => {
    const url = new URL(request.url!, `http://localhost:${port}`);
    if (url.pathname === "/ws") {
      wss.handleUpgrade(request, socket, head, (ws) => {
        wss.emit("connection", ws, request);
      });
    } else {
      socket.destroy();
    }
  });
}
