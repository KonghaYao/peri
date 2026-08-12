//! long_session_study.ts — 超长会话专题研究（主分析脚本）
//!
//! 口径:
//!   超长会话(Long) = 主会话(parent_thread_id IS NULL AND hidden=0)且 message_count >= 500
//!   对照组(Base)   = 其余主会话 (message_count < 500)
//!   计数口径: 会话级统计为主, 消息级为辅; 全文统一, 分母明确
//!
//! 四维度: 行为画像 / compact 生命周期 / 缺陷模式 / 结局与对比 + 时间演变
//! 净化:   excluded 消息排除; role=system 排除; user 文本剥离 <system-reminder> 注入块
//!
//! 用法:
//!   bun run src/long_session_study.ts [--db <path>] [--min-messages 500]
//!
//! 输出:
//!   src/data/long-session-study.json  全部统计(报告数字均出自此文件,可重跑复现)

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { writeFileSync } from "fs";
import { avg, median, quantile, formatSize, formatDuration } from "./lib/utils.js";

// ── CLI ──

const argv = process.argv.slice(2);
const get = (key: string): string | undefined => {
  const i = argv.indexOf(key);
  return i >= 0 ? argv[i + 1] : undefined;
};
const DB_PATH = get("--db") ?? join(homedir(), ".peri/threads/threads.db");
const MIN_MESSAGES = Number(get("--min-messages") ?? 500);
const OUT_FILE = join(import.meta.dir, "data", "long-session-study.json");

const db = new Database(DB_PATH, { readonly: true });

// ── 消息解析(精简版,与 tool_token_consumption.ts 同语义) ──

interface ToolUse { id: string; name: string; input: any; inputBytes: number }
interface Parsed {
  role: string;
  text: string;
  toolUses: ToolUse[];
  isError?: boolean;
  toolCallId?: string;
  bytes: number;
}

function parseMessage(content: string): Parsed | null {
  let msg: any;
  try { msg = JSON.parse(content); } catch { return null; }
  if (!msg || typeof msg !== "object") return null;
  const role = msg.role;
  if (role === "assistant") {
    const blocks: any[] = Array.isArray(msg.content) ? msg.content : [];
    let text = "";
    const toolUses: ToolUse[] = [];
    let hasUseInContent = false;
    for (const b of blocks) {
      if (!b || typeof b !== "object") continue;
      if (b.type === "tool_use") {
        hasUseInContent = true;
        toolUses.push({ id: b.id, name: b.name, input: b.input, inputBytes: Buffer.byteLength(JSON.stringify(b.input ?? {}), "utf8") });
      } else if (typeof b.text === "string") text += b.text;
    }
    if (!hasUseInContent && Array.isArray(msg.tool_calls)) {
      for (const tc of msg.tool_calls) {
        if (!tc || typeof tc !== "object" || !tc.id) continue;
        const args = tc.arguments ?? {};
        toolUses.push({ id: tc.id, name: tc.name ?? "unknown", input: args, inputBytes: Buffer.byteLength(JSON.stringify(args), "utf8") });
      }
    }
    return { role, text, toolUses, bytes: Buffer.byteLength(text, "utf8") };
  }
  if (role === "tool") {
    const c = msg.content;
    const isErr = !!msg.is_error;
    return { role, text: typeof c === "string" ? c : "", toolUses: [], isError: isErr, toolCallId: msg.tool_call_id, bytes: Buffer.byteLength(typeof c === "string" ? c : JSON.stringify(c), "utf8") };
  }
  if (role === "user") {
    if (typeof msg.content === "string") return { role, text: msg.content, toolUses: [], bytes: Buffer.byteLength(msg.content, "utf8") };
    const blocks: any[] = Array.isArray(msg.content) ? msg.content : [];
    let text = "";
    for (const b of blocks) if (b && typeof b === "object" && typeof b.text === "string") text += b.text;
    return { role, text, toolUses: [], bytes: Buffer.byteLength(text, "utf8") };
  }
  if (role === "system") {
    const c = msg.content;
    const text = typeof c === "string" ? c : "";
    return { role, text, toolUses: [], bytes: Buffer.byteLength(text, "utf8") };
  }
  return null;
}

/** 剥离 user 文本中的 <system-reminder> 注入块 */
function stripSystemReminder(text: string): string {
  return text.replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "").trim();
}

// ── 数据加载 ──

const threads = db.query(
  `SELECT id, title, created_at, updated_at, cwd, message_count, agent_status FROM threads
   WHERE parent_thread_id IS NULL AND hidden = 0 ORDER BY created_at ASC`
).all() as { id: string; title: string | null; created_at: string; updated_at: string; cwd: string; message_count: number; agent_status: string }[];

const longIds = new Set(threads.filter((t) => t.message_count >= MIN_MESSAGES).map((t) => t.id));

const msgRows = db.query(
  `SELECT m.thread_id, m.role, m.content, m.truncated, m.excluded, m.projection, m.rowid
   FROM messages m WHERE m.thread_id IN (SELECT id FROM threads WHERE parent_thread_id IS NULL AND hidden = 0)
   ORDER BY m.thread_id, m.rowid ASC`
).all() as { thread_id: string; role: string; content: string; truncated: number; excluded: number; projection: string | null; rowid: number }[];

// 子 agent 关联
const subRows = db.query(
  `SELECT parent_thread_id AS pid, COUNT(*) AS n FROM threads WHERE parent_thread_id IS NOT NULL GROUP BY pid`
).all() as { pid: string; n: number }[];
const subCount: Record<string, number> = {};
for (const s of subRows) subCount[s.pid] = s.n;

// metrics compact 触发旁证 (trap.compact_trigger, 按 sid 匹配; 仅覆盖 metrics 保留窗口)
let metricsCompact: Record<string, number> = {};
try {
  const { readdirSync, readFileSync } = await import("fs");
  const dir = join(homedir(), ".peri/metrics");
  for (const f of readdirSync(dir).filter((x) => x.endsWith(".jsonl"))) {
    for (const line of readFileSync(join(dir, f), "utf8").split("\n")) {
      if (!line.includes("trap.compact_trigger")) continue;
      try {
        const e = JSON.parse(line);
        if (e.sid) metricsCompact[e.sid] = (metricsCompact[e.sid] ?? 0) + 1;
      } catch { /* 单行损坏跳过 */ }
    }
  }
} catch { metricsCompact = {}; }

// ── 净化计数 ──

const purge = {
  excluded_flag: 0,
  system_role: 0,
  user_reminder_injected: 0,
  parse_fail: 0,
  empty_after_strip: 0,
};

// ── 会话级聚合 ──

interface SessionStat {
  id: string;
  title: string | null;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  agent_status: string;
  wallMinutes: number;
  msg: { user: number; assistant: number; tool: number; system: number };
  userTextBytes: number[]; // 每条净化的 user 文本字节
  toolCalls: number;
  toolNames: Record<string, number>;
  toolUseBytes: number[];   // 入参
  toolResultBytes: number[]; // 出参
  toolErrors: number;
  errByTool: Record<string, number>;
  truncated: number;
  excluded: number;
  projections: number;
  projectionExcluded: number;
  projectionEntries: number;
  firstProjectionPos: number | null; // 相对消息序号(1-based)
  toolSeq: { name: string; args: string }[]; // 缺陷检测用
  errSeq: string[];
  lastMsg: { role: string; kind: string; isError: boolean } | null;
  textBytesPerBlock: number[]; // 每 100 消息窗口的新增可见文本字节(膨胀轨迹)
  ctxBytesPerBlock: number[];  // 每 100 消息窗口末尾的累积可见文本字节(上下文规模)
  ctxBytes: number;
  reminders: number;
}

const sessions: SessionStat[] = [];
const longSessions: SessionStat[] = [];
const baseSessions: SessionStat[] = [];

// 阶段1: 初始化全部主会话(含 0 消息会话), 保证会话级统计分母完整
const sessionById = new Map<string, SessionStat>();
for (const t of threads) {
  sessionById.set(t.id, {
    id: t.id,
    title: t.title,
    cwd: t.cwd,
    created_at: t.created_at,
    updated_at: t.updated_at,
    message_count: t.message_count,
    agent_status: t.agent_status,
    wallMinutes: (new Date(t.updated_at).getTime() - new Date(t.created_at).getTime()) / 60000,
    msg: { user: 0, assistant: 0, tool: 0, system: 0 },
    userTextBytes: [],
    toolCalls: 0,
    toolNames: {},
    toolUseBytes: [],
    toolResultBytes: [],
    toolErrors: 0,
    errByTool: {},
    truncated: 0,
    excluded: 0,
    projections: 0,
    projectionExcluded: 0,
    projectionEntries: 0,
    firstProjectionPos: null,
    toolSeq: [],
    errSeq: [],
    lastMsg: null,
    textBytesPerBlock: [0],
    ctxBytesPerBlock: [],
    ctxBytes: 0,
    reminders: 0,
  });
}

// 阶段2: 消息填充
let curCount = 0;
let curId = "";
for (const r of msgRows) {
  const cur = sessionById.get(r.thread_id)!;
  if (r.thread_id !== curId) { curId = r.thread_id; curCount = 0; }
  curCount++;
  const isProjection = r.projection !== null && r.projection !== "";
  if (isProjection) {
    cur.projections++;
    if (cur.firstProjectionPos === null) cur.firstProjectionPos = curCount;
    try {
      const d = JSON.parse(r.projection);
      if (Array.isArray(d.entries)) cur.projectionEntries += d.entries.length;
    } catch { /* 忽略解析失败 */ }
  }
  if (r.excluded) {
    purge.excluded_flag++;
    cur.excluded++;
    if (isProjection) cur.projectionExcluded++;
    continue;
  }
  if (r.role === "system") { purge.system_role++; cur.msg.system++; continue; }
  const p = parseMessage(r.content);
  if (!p) { purge.parse_fail++; continue; }
  cur.lastMsg = { role: r.role, kind: p.toolUses.length > 0 ? "tool_use" : r.role === "tool" ? "tool_result" : r.role === "user" ? "user" : "text", isError: !!p.isError };
  if (r.truncated) cur.truncated++;
  if (p.role === "user") {
    cur.msg.user++;
    const cleaned = stripSystemReminder(p.text);
    if (cleaned !== p.text) purge.user_reminder_injected++;
    if (cleaned.length === 0) purge.empty_after_strip++;
    cur.userTextBytes.push(Buffer.byteLength(cleaned, "utf8"));
    if (!r.truncated) { cur.ctxBytes += Buffer.byteLength(cleaned, "utf8"); cur.textBytesPerBlock[cur.textBytesPerBlock.length - 1] += Buffer.byteLength(cleaned, "utf8"); }
  } else if (p.role === "assistant") {
    cur.msg.assistant++;
    if (!r.truncated) { cur.ctxBytes += p.bytes; cur.textBytesPerBlock[cur.textBytesPerBlock.length - 1] += p.bytes; }
    for (const tu of p.toolUses) {
      cur.toolCalls++;
      cur.toolNames[tu.name] = (cur.toolNames[tu.name] ?? 0) + 1;
      cur.toolUseBytes.push(tu.inputBytes);
      cur.toolSeq.push({ name: tu.name, args: JSON.stringify(tu.input).slice(0, 200) });
    }
  } else if (p.role === "tool") {
    cur.msg.tool++;
    cur.toolResultBytes.push(p.bytes);
    if (!r.truncated) { cur.ctxBytes += p.bytes; cur.textBytesPerBlock[cur.textBytesPerBlock.length - 1] += p.bytes; }
    if (p.isError) {
      cur.toolErrors++;
      const name = cur.toolSeq.length > 0 ? cur.toolSeq[cur.toolSeq.length - 1].name : "unknown";
      cur.errByTool[name] = (cur.errByTool[name] ?? 0) + 1;
      cur.errSeq.push(name);
    }
  }
  if (curCount % 100 === 0) {
    cur.ctxBytesPerBlock.push(cur.ctxBytes);
    cur.textBytesPerBlock.push(0);
  }
}

// 阶段3: 分组
for (const s of sessionById.values()) {
  sessions.push(s);
  (longIds.has(s.id) ? longSessions : baseSessions).push(s);
}

// ── 缺陷检测启发式 ──
//
// 假阳性防护(案例验证后修正):
//   - AgentResult 重复调用 = bg 任务轮询(正常), 不计 deadLoop
//   - Read/Grep/LineEdit/Glob 交替 = 搜索工作流(正常), 不计 oscillation
//   - Edit/Write/Bash 同参数连续 3 次 = 迭代试错(弱信号), 单列 iterLoop

interface DefectCounts {
  retryChain: number;      // 同工具连续 error >= 2
  deadLoop: number;        // 非轮询工具同参数连续调用 >= 3
  iterLoop: number;        // 写型工具(Edit/Write/Bash)同参数连续 >= 3(迭代试错)
  oscillation: number;     // 非搜索工具 A→B→A→B 交替 >= 4
  hugeOutput: number;      // tool 出参 > 100KB
  hugeInput: number;       // 入参 > 50KB
  lowDiversity: boolean;   // 长会话工具种类 <= 2
}

const POLL_TOOLS = new Set(["AgentResult", "Task", "AskUserQuestion"]);
const SEARCH_TOOLS = new Set(["Read", "Grep", "Glob", "LineEdit", "WebSearch"]);

function detectDefects(s: SessionStat): DefectCounts {
  const d: DefectCounts = { retryChain: 0, deadLoop: 0, iterLoop: 0, oscillation: 0, hugeOutput: 0, hugeInput: 0, lowDiversity: false };
  // 连续失败链: 同工具连续 error >= 2
  let prev = "";
  let run = 0;
  for (const name of s.errSeq) {
    if (name === prev) {
      run++;
      if (run === 2) { d.retryChain++; run = 1; }
    } else {
      prev = name;
      run = 1;
    }
  }
  // 死循环: 非轮询工具同参数连续 >= 3
  let drun = 0;
  for (let i = 0; i < s.toolSeq.length; i++) {
    const cur = s.toolSeq[i];
    const key = `${cur.name}|${cur.args}`;
    const prevKey = i > 0 ? `${s.toolSeq[i - 1].name}|${s.toolSeq[i - 1].args}` : "";
    drun = key === prevKey ? drun + 1 : 1;
    if (drun === 3) {
      if (["Edit", "Write", "Bash"].includes(cur.name)) d.iterLoop++;
      else if (!POLL_TOOLS.has(cur.name)) d.deadLoop++;
      drun = 1;
    }
  }
  // 振荡: 非搜索工具 A→B→A→B (>=4 步, 恰好两种工具交替)
  for (let i = 0; i + 3 < s.toolSeq.length; i++) {
    const a = s.toolSeq[i].name, b = s.toolSeq[i + 1].name;
    if (a === b || SEARCH_TOOLS.has(a) || SEARCH_TOOLS.has(b)) continue;
    if (s.toolSeq[i + 2].name === a && s.toolSeq[i + 3].name === b) {
      d.oscillation++;
      i += 3;
    }
  }
  d.hugeOutput = s.toolResultBytes.filter((b) => b > 100 * 1024).length;
  d.hugeInput = s.toolUseBytes.filter((b) => b > 50 * 1024).length;
  d.lowDiversity = Object.keys(s.toolNames).length <= 2;
  return d;
}

// ── 统计输出 ──

function summarizeStats(arr: number[]): { n: number; avg: number; p50: number; p90: number; p99: number; max: number; sum: number } {
  return { n: arr.length, avg: avg(arr), p50: median(arr), p90: quantile(arr, 0.9), p99: quantile(arr, 0.99), max: arr.length ? Math.max(...arr) : 0, sum: arr.reduce((a, b) => a + b, 0) };
}

function groupStats(arr: number[]): { n: number; avg: number; p50: number; p90: number; p99: number; max: number } {
  return { n: arr.length, avg: avg(arr), p50: median(arr), p90: quantile(arr, 0.9), p99: quantile(arr, 0.99), max: arr.length ? Math.max(...arr) : 0 };
}

function topTools(toolNames: Record<string, number>): [string, number][] {
  return Object.entries(toolNames).sort((a, b) => b[1] - a[1]).slice(0, 15);
}

const L = longSessions;
const B = baseSessions;

// ── A. 行为画像 ──

const longMsgRoles = { user: 0, assistant: 0, tool: 0, system: 0 };
for (const s of L) { for (const k of Object.keys(longMsgRoles) as (keyof typeof longMsgRoles)[]) longMsgRoles[k] += s.msg[k]; }
const baseMsgRoles = { user: 0, assistant: 0, tool: 0, system: 0 };
for (const s of B) { for (const k of Object.keys(baseMsgRoles) as (keyof typeof baseMsgRoles)[]) baseMsgRoles[k] += s.msg[k]; }

function mergeToolCounts(ss: SessionStat[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const s of ss) for (const [k, v] of Object.entries(s.toolNames)) out[k] = (out[k] ?? 0) + v;
  return out;
}

const longTopTools = topTools(mergeToolCounts(L));
const baseTopTools = topTools(mergeToolCounts(B));

function per100(n: number, msgCount: number): number {
  return msgCount === 0 ? 0 : (n / msgCount) * 100;
}
const longToolCallsPer100 = L.map((s) => per100(s.toolCalls, s.message_count));
const baseToolCallsPer100 = B.map((s) => per100(s.toolCalls, s.message_count));

// ── B. compact 生命周期 ──

const compactStats = {
  long: {
    n: L.length,
    withProjection: L.filter((s) => s.projections > 0).length,
    projections: summarizeStats(L.map((s) => s.projections)),
    projectionExcludedPct: avg(L.filter((s) => s.projections > 0).map((s) => (s.projectionExcluded / s.projections) * 100)),
    projectionEntries: summarizeStats(L.map((s) => s.projectionEntries)),
    firstProjectionPos: groupStats(L.filter((s) => s.firstProjectionPos !== null).map((s) => s.firstProjectionPos!)),
    truncatedPerSession: groupStats(L.map((s) => s.truncated)),
    excludedPerSession: groupStats(L.map((s) => s.excluded)),
    truncatedPctOfMsg: avg(L.map((s) => (s.truncated / s.message_count) * 100)),
    metricsCompactTriggers: { sessions: L.filter((s) => (metricsCompact[s.id] ?? 0) > 0).length, total: L.reduce((a, s) => a + (metricsCompact[s.id] ?? 0), 0) },
  },
  base: {
    n: B.length,
    withProjection: B.filter((s) => s.projections > 0).length,
    projections: summarizeStats(B.map((s) => s.projections)),
    truncatedPerSession: groupStats(B.map((s) => s.truncated)),
    excludedPerSession: groupStats(B.map((s) => s.excluded)),
    metricsCompactTriggers: { sessions: B.filter((s) => (metricsCompact[s.id] ?? 0) > 0).length, total: B.reduce((a, s) => a + (metricsCompact[s.id] ?? 0), 0) },
  },
};

// 上下文规模轨迹: 长会话每 100 消息窗口末尾的累积可见文本字节(均值/中位)
const maxBlocks = Math.max(...L.map((s) => s.ctxBytesPerBlock.length));
const inflation = {
  maxBlocks,
  perBlockAvg: [] as number[],
  perBlockP50: [] as number[],
  perBlockMax: [] as number[],
};
for (let i = 0; i < maxBlocks; i++) {
  const vals = L.filter((s) => s.ctxBytesPerBlock[i] !== undefined).map((s) => s.ctxBytesPerBlock[i]);
  inflation.perBlockAvg.push(avg(vals));
  inflation.perBlockP50.push(median(vals));
  inflation.perBlockMax.push(Math.max(...vals));
}

// ── C. 缺陷模式 ──

const longDefects = L.map((s) => ({ id: s.id, title: s.title, d: detectDefects(s) }));
const baseDefects = B.map((s) => detectDefects(s));

function defectSummary(ds: { d: DefectCounts }[], msgCounts: number[]): Record<string, { total: number; per100Avg: number; sessionsWith: number }> {
  const keys: (keyof DefectCounts)[] = ["retryChain", "deadLoop", "iterLoop", "oscillation", "hugeOutput", "hugeInput"];
  const out: Record<string, { total: number; per100Avg: number; sessionsWith: number }> = {};
  for (const k of keys) {
    const per100Arr = ds.map((x, i) => per100(x.d[k], msgCounts[i] ?? 1));
    out[k] = { total: ds.reduce((a, x) => a + x.d[k], 0), per100Avg: avg(per100Arr), sessionsWith: ds.filter((x) => x.d[k] > 0).length };
  }
  return out;
}

// ── D. 结局 ──

function endingBreakdown(ss: SessionStat[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const s of ss) {
    const lm = s.lastMsg;
    let kind = "none";
    if (lm) {
      if (lm.role === "tool" && lm.isError) kind = "tool_error";
      else if (lm.role === "user") kind = "user_question";
      else if (lm.role === "assistant" && lm.kind === "tool_use") kind = "assistant_tool_use";
      else if (lm.role === "assistant") kind = "assistant_text";
      else kind = lm.role;
    }
    out[kind] = (out[kind] ?? 0) + 1;
  }
  return out;
}

const longStatus: Record<string, number> = {};
for (const s of L) longStatus[s.agent_status] = (longStatus[s.agent_status] ?? 0) + 1;
const baseStatus: Record<string, number> = {};
for (const s of B) baseStatus[s.agent_status] = (baseStatus[s.agent_status] ?? 0) + 1;

// ── E. 时间演变(按周) ──

function isoWeek(d: Date): string {
  const date = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()));
  const dayNum = date.getUTCDay() || 7;
  date.setUTCDate(date.getUTCDate() + 4 - dayNum);
  const yearStart = new Date(Date.UTC(date.getUTCFullYear(), 0, 1));
  const weekNo = Math.ceil((((date.getTime() - yearStart.getTime()) / 86400000) + 1) / 7);
  return `${date.getUTCFullYear()}-W${String(weekNo).padStart(2, "0")}`;
}

const weekAgg: Record<string, { created: number; long: number; longCounts: number[] }> = {};
for (const t of threads) {
  const w = isoWeek(new Date(t.created_at));
  const a = (weekAgg[w] = weekAgg[w] ?? { created: 0, long: 0, longCounts: [] });
  a.created++;
  if (t.message_count >= MIN_MESSAGES) { a.long++; a.longCounts.push(t.message_count); }
}

// ── 输出 ──

const result = {
  meta: {
    db: DB_PATH,
    generated_at: new Date().toISOString(),
    min_messages: MIN_MESSAGES,
    date_range: { min: threads[0]?.created_at, max: threads[threads.length - 1]?.updated_at },
  },
  scope: {
    main_threads_total: threads.length,
    long: L.length,
    base: B.length,
    long_message_share: (L.reduce((a, s) => a + s.message_count, 0) / threads.reduce((a, t) => a + t.message_count, 0)) * 100,
    purge,
  },
  A_behavior: {
    long: {
      msgRoles: longMsgRoles,
      wallMinutes: groupStats(L.map((s) => s.wallMinutes)),
      userTextBytes: groupStats(L.flatMap((s) => s.userTextBytes)),
      userMsgCount: summarizeStats(L.map((s) => s.msg.user)),
      toolCallsPerSession: summarizeStats(L.map((s) => s.toolCalls)),
      toolKinds: groupStats(L.map((s) => Object.keys(s.toolNames).length)),
      toolCallsPer100: groupStats(longToolCallsPer100),
      topTools: longTopTools,
      subAgentCount: summarizeStats(L.map((s) => subCount[s.id] ?? 0)),
    },
    base: {
      msgRoles: baseMsgRoles,
      wallMinutes: groupStats(B.map((s) => s.wallMinutes)),
      userTextBytes: groupStats(B.flatMap((s) => s.userTextBytes)),
      toolCallsPerSession: summarizeStats(B.map((s) => s.toolCalls)),
      toolKinds: groupStats(B.map((s) => Object.keys(s.toolNames).length)),
      toolCallsPer100: groupStats(baseToolCallsPer100),
      topTools: baseTopTools,
      subAgentCount: summarizeStats(B.map((s) => subCount[s.id] ?? 0)),
    },
  },
  B_compact: compactStats,
  B_inflation: inflation,
  B_strategy_evolution: Object.entries(
    L.reduce<Record<string, { n: number; excluded: number; projection: number; truncated: number }>>((acc, s) => {
      const w = isoWeek(new Date(s.created_at));
      const a = (acc[w] = acc[w] ?? { n: 0, excluded: 0, projection: 0, truncated: 0 });
      a.n++;
      if (s.excluded > 0) a.excluded++;
      if (s.projections > 0) a.projection++;
      if (s.truncated > 0) a.truncated++;
      return acc;
    }, {})
  ).sort((a, b) => a[0].localeCompare(b[0])).map(([week, v]) => ({ week, ...v })),
  C_defects: {
    long: defectSummary(longDefects, L.map((s) => s.message_count)),
    base: defectSummary(baseDefects.map((d) => ({ d })), B.map((s) => s.message_count)),
    longLowDiversity: L.filter((s) => detectDefects(s).lowDiversity).length,
    baseLowDiversity: B.filter((s) => detectDefects(s).lowDiversity).length,
  },
  D_ending: {
    long: endingBreakdown(L),
    base: endingBreakdown(B),
    longStatus,
    baseStatus,
  },
  E_timeline: Object.entries(weekAgg).sort((a, b) => a[0].localeCompare(b[0])).map(([week, v]) => ({ week, created: v.created, long: v.long, longPct: (v.long / v.created) * 100, longCounts: v.longCounts })),
  top_long_sessions: L.map((s) => ({ id: s.id, title: s.title, cwd: s.cwd, messages: s.message_count, wallMinutes: Math.round(s.wallMinutes), toolCalls: s.toolCalls, toolKinds: Object.keys(s.toolNames).length, projections: s.projections, truncated: s.truncated, excluded: s.excluded, subAgents: subCount[s.id] ?? 0, errors: s.toolErrors, status: s.agent_status })).sort((a, b) => b.messages - a.messages).slice(0, 42),
};

writeFileSync(OUT_FILE, JSON.stringify(result, null, 2));
db.close();

// ── 终端摘要 ──

const c = (s: string) => s;
console.log(c(`\n═══ 超长会话研究(≥${MIN_MESSAGES} 消息) ═══`));
console.log(c(`范围: ${threads.length} 主会话 → 超长 ${L.length} 个 / 基准 ${B.length} 个`));
console.log(c(`消息占比: 超长会话占总消息 ${result.scope.long_message_share.toFixed(1)}%`));
console.log(c(`净化: ${JSON.stringify(purge)}`));
console.log(c(`\n[A] 行为画像`));
console.log(c(`  长会话消息构成 user/assistant/tool: ${longMsgRoles.user}/${longMsgRoles.assistant}/${longMsgRoles.tool}`));
console.log(c(`  基准会话同口径: ${baseMsgRoles.user}/${baseMsgRoles.assistant}/${baseMsgRoles.tool}`));
console.log(c(`  长: 墙钟 ${formatDuration(result.A_behavior.long.wallMinutes.p50)} (P50) | 工具种类 ${result.A_behavior.long.toolKinds.p50} | 每100消息工具调用 ${result.A_behavior.long.toolCallsPer100.p50.toFixed(0)}`));
console.log(c(`  基准: 墙钟 ${formatDuration(result.A_behavior.base.wallMinutes.p50)} (P50) | 工具种类 ${result.A_behavior.base.toolKinds.p50} | 每100消息工具调用 ${result.A_behavior.base.toolCallsPer100.p50.toFixed(0)}`));
console.log(c(`  长会话 Top 工具: ${longTopTools.slice(0, 8).map(([n, v]) => `${n}(${v})`).join(" ")}`));
console.log(c(`  长会话子 agent 数 P50: ${result.A_behavior.long.subAgentCount.p50} (基准 ${result.A_behavior.base.subAgentCount.p50})`));
console.log(c(`\n[B] compact 生命周期`));
console.log(c(`  长会话含 projection: ${compactStats.long.withProjection}/${L.length} | 平均投影消息 ${compactStats.long.projections.avg.toFixed(1)} (基准含投影 ${compactStats.base.withProjection}/${B.length})`));
console.log(c(`  projection 首现位置 P50: 第 ${compactStats.long.firstProjectionPos.p50} 条消息`));
console.log(c(`  metrics compact_trigger: 长 ${compactStats.long.metricsCompactTriggers.total} 次/${compactStats.long.metricsCompactTriggers.sessions} 会话 (基准 ${compactStats.base.metricsCompactTriggers.total} 次/${compactStats.base.metricsCompactTriggers.sessions} 会话)`));
console.log(c(`  膨胀轨迹块数: ${inflation.maxBlocks} (每块100消息)`));
console.log(c(`\n[C] 缺陷模式 (每100消息密度)`));
for (const k of ["retryChain", "deadLoop", "iterLoop", "oscillation", "hugeOutput", "hugeInput"] as const) {
  console.log(c(`  ${k}: 长 ${result.C_defects.long[k].per100Avg.toFixed(2)} (总量${result.C_defects.long[k].total}) vs 基准 ${result.C_defects.base[k].per100Avg.toFixed(2)} (总量${result.C_defects.base[k].total})`));
}
console.log(c(`  低工具多样性(≤2种): 长 ${result.C_defects.longLowDiversity}/${L.length} vs 基准 ${result.C_defects.baseLowDiversity}/${B.length}`));
console.log(c(`\n[D] 结局`));
console.log(c(`  长会话结尾: ${JSON.stringify(result.D_ending.long)}`));
console.log(c(`  基准结尾: ${JSON.stringify(result.D_ending.base)}`));
console.log(c(`  agent_status 长: ${JSON.stringify(longStatus)} | 基准: ${JSON.stringify(baseStatus)}`));
console.log(c(`\n[E] 时间演变(最近8周)`));
for (const w of result.E_timeline.slice(-8)) {
  console.log(c(`  ${w.week}: 创建 ${w.created} | 超长 ${w.long} (${w.longPct.toFixed(1)}%)`));
}
console.log(c(`\n输出: ${OUT_FILE}`));
