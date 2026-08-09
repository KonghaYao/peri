//! tool_token_consumption.ts — 工具调用的 token 消耗研究(主分析脚本)
//!
//! 基于 threads.db 全量主线程消息,统计工具调用的入参/出参字节、分桶分布、
//! 浪费(白搜/重读/重复搜索/巨型输出)、时间演变、会话级消耗,
//! 并以 ~/.peri/metrics/*.jsonl 的真实 token 事件做字节→token 比率标定(方法 A/B/C 交叉验证)。
//!
//! 用法:
//!   bun run src/tool_token_consumption.ts [--db <path>] [--out-dir <dir>] [--out-name <prefix>]
//!
//! 参数:
//!   --db        数据库路径(默认 ~/.peri/threads/threads.db;可传备份库做同窗口复现)
//!   --out-dir   输出目录(默认 <repo>/src/data,源数据专用目录)
//!   --out-name  输出文件名前缀(默认 tool-token-consumption)
//!
//! 输出:
//!   <out-dir>/<prefix>.json     全部统计结果(报告数字均出自此文件,可重跑复现)

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { readdirSync, readFileSync, writeFileSync, mkdirSync, existsSync } from "fs";
import { quantile, formatSize, printHeader, printSection, printMetric, printTable } from "./lib/utils.js";

// ── CLI ──

const DEFAULT_DB = join(homedir(), ".peri/threads/threads.db");
const DEFAULT_OUT_DIR = join(import.meta.dir, "data");

function parseArgs(): { db: string; outDir: string; outName: string } {
  const argv = process.argv.slice(2);
  const get = (key: string): string | undefined => {
    const i = argv.indexOf(key);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  return {
    db: get("--db") ?? DEFAULT_DB,
    outDir: get("--out-dir") ?? DEFAULT_OUT_DIR,
    outName: get("--out-name") ?? "tool-token-consumption",
  };
}

const { db: DB_PATH, outDir: OUT_DIR, outName: OUT_NAME } = parseArgs();
const METRICS_DIR = join(homedir(), ".peri/metrics");
const OUT_FILE = join(OUT_DIR, `${OUT_NAME}.json`);

const db = new Database(DB_PATH, { readonly: true });

// ── 统计工具 ──

function pctNum(n: number, total: number): number {
  return total === 0 ? 0 : (n / total) * 100;
}

// ── 消息解析 ──

interface ToolUse {
  id: string;
  name: string;
  input: any;
  inputBytes: number;
}
interface Parsed {
  role: string;
  kind: "tool_use" | "tool_result" | "text" | "reasoning" | "other" | "user-string";
  text: string;
  toolCallId?: string;
  toolUses?: ToolUse[];
  isError?: boolean;
  bytes: number; // 解析后内容字节(真实文本)
}

function parseMessage(content: string): Parsed | null {
  let msg: any;
  try {
    msg = JSON.parse(content);
  } catch {
    return null;
  }
  if (!msg || typeof msg !== "object") return null;
  const role = msg.role;
  if (role === "assistant") {
    const blocks: any[] = Array.isArray(msg.content) ? msg.content : [];
    let text = "";
    let toolUses: ToolUse[] = [];
    let hasUseInContent = false;
    for (const b of blocks) {
      if (!b || typeof b !== "object") continue;
      if (b.type === "tool_use") {
        hasUseInContent = true;
        const inputBytes = Buffer.byteLength(JSON.stringify(b.input ?? {}), "utf8");
        toolUses.push({ id: b.id, name: b.name, input: b.input, inputBytes });
      } else if (typeof b.text === "string") {
        text += b.text;
      }
    }
    // 旧格式: content 无 tool_use 时, 从 tool_calls 字段提取 (arguments 而非 input)
    if (!hasUseInContent && Array.isArray(msg.tool_calls)) {
      for (const tc of msg.tool_calls) {
        if (!tc || typeof tc !== "object" || !tc.id) continue;
        const args = tc.arguments ?? {};
        const inputBytes = Buffer.byteLength(JSON.stringify(args), "utf8");
        toolUses.push({ id: tc.id, name: tc.name ?? "unknown", input: args, inputBytes });
      }
    }
    return {
      role,
      kind: toolUses.length > 0 ? "tool_use" : "other",
      text,
      toolUses,
      bytes: Buffer.byteLength(text, "utf8"),
    };
  }
  if (role === "tool") {
    const c = msg.content;
    const bytes = Buffer.byteLength(typeof c === "string" ? c : JSON.stringify(c), "utf8");
    return {
      role,
      kind: "tool_result",
      text: typeof c === "string" ? c : "",
      toolCallId: msg.tool_call_id,
      isError: !!msg.is_error,
      bytes,
    };
  }
  if (role === "user") {
    if (typeof msg.content === "string") {
      return { role, kind: "user-string", text: msg.content, bytes: Buffer.byteLength(msg.content, "utf8") };
    }
    const blocks: any[] = Array.isArray(msg.content) ? msg.content : [];
    let text = "";
    for (const b of blocks) {
      if (b && typeof b === "object" && typeof b.text === "string") text += b.text;
    }
    return { role, kind: "other", text, bytes: Buffer.byteLength(text, "utf8") };
  }
  if (role === "system") {
    const c = msg.content;
    const text = typeof c === "string" ? c : "";
    return { role, kind: "other", text, bytes: Buffer.byteLength(text, "utf8") };
  }
  return null;
}

// ── 数据加载 ──

interface ThreadInfo {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  cwd: string;
  message_count: number;
}

const threads = db.query(
  `SELECT id, title, created_at, updated_at, cwd, message_count FROM threads WHERE parent_thread_id IS NULL ORDER BY created_at ASC`
).all() as ThreadInfo[];

const threadIds = threads.map((t) => t.id);

// 预加载所有主线程消息 (总量 20 万级, 内存可控)
const msgRows = db.query(
  `SELECT m.thread_id, m.role, m.content, m.truncated, m.excluded
   FROM messages m WHERE m.thread_id IN (SELECT id FROM threads WHERE parent_thread_id IS NULL)
   ORDER BY m.thread_id, m.rowid ASC`
).all() as { thread_id: string; role: string; content: string; truncated: number; excluded: number }[];

// 净化计数
const excl: Record<string, number> = {
  excluded_flag: 0, // excluded=1
  system_role: 0, // role=system
  compact_summary: 0, // user 消息含 <system-reminder>
  parse_fail: 0,
};

interface MsgRec {
  role: string;
  parsed: Parsed | null;
  truncated: boolean;
  excluded: boolean;
  rawBytes: number;
}

const perThread: Record<string, MsgRec[]> = {};
const threadExcludedUseIds: Record<string, Set<string>> = {}; // 被净化排除的 assistant 消息中的 tool_use id
for (const r of msgRows) {
  const rec: MsgRec = { role: r.role, parsed: null, truncated: !!r.truncated, excluded: !!r.excluded, rawBytes: Buffer.byteLength(r.content, "utf8") };
  if (r.excluded) {
    excl.excluded_flag++;
    // 记录其中可能存在的 tool_use id (用于净化 tool_result)
    if (r.role === "assistant" && r.content.includes("tool_use")) {
      const p = parseMessage(r.content);
      if (p?.toolUses) {
        const s = (threadExcludedUseIds[r.thread_id] = threadExcludedUseIds[r.thread_id] || new Set());
        for (const tu of p.toolUses) s.add(tu.id);
      }
    }
    continue;
  }
  if (r.role === "system") { excl.system_role++; continue; }
  if (r.role === "user" && r.content.includes("<system-reminder>")) { excl.compact_summary++; continue; }
  const p = parseMessage(r.content);
  if (!p) { excl.parse_fail++; continue; }
  rec.parsed = p;
  (perThread[r.thread_id] = perThread[r.thread_id] || []).push(rec);
}

// ── 主分析 ──

interface ToolAgg {
  calls: number;
  sessions: Set<string>;
  inBytes: number;
  outBytes: number;
  outList: number[];
  errors: number;
}

const toolAgg = new Map<string, ToolAgg>();
let totalCalls = 0;
let totalInBytes = 0;
let totalOutBytes = 0;
let totalErrors = 0;
let matchedResults = 0;
let unmatchedToolUses = 0;
let orphanExcludedResults = 0; // tool_result 属于被净化 assistant 消息的 tool_use
let orphanUnknownResults = 0; // 无任何对应 tool_use 的 tool_result

// 出参分桶
const buckets = [
  { name: "<1KB", min: 0, max: 1024 },
  { name: "1-5KB", min: 1024, max: 5 * 1024 },
  { name: "5-20KB", min: 5 * 1024, max: 20 * 1024 },
  { name: "20-50KB", min: 20 * 1024, max: 50 * 1024 },
  { name: "50-100KB", min: 50 * 1024, max: 100 * 1024 },
  { name: ">100KB", min: 100 * 1024, max: Infinity },
];
const bucketCounts: Record<string, number> = {};
const bucketSessions: Record<string, Set<string>> = {};
const bucketBytes: Record<string, number> = {};
for (const b of buckets) { bucketCounts[b.name] = 0; bucketSessions[b.name] = new Set(); bucketBytes[b.name] = 0; }

// 巨型输出清单
const giantOutputs: { thread_id: string; name: string; bytes: number; ts: string }[] = [];
// Bash 100KB 窄带检查
const bashNarrowband: { bytes: number }[] = [];

// 会话级工具消耗
const sessionCost: Record<string, { inBytes: number; outBytes: number; calls: number }> = {};

// 全量出参分布
const allOutBytes: number[] = [];
// 浪费并集 (每线程内被标记为浪费的调用序号 → outBytes 并集)
let wasteUnionBytes = 0;

// 时间演变 (按线程 created_at 归周)
const weekKeys: string[] = [];
function weekKey(iso: string): string {
  const d = new Date(iso);
  const start = new Date(d.getTime() - ((d.getUTCDay() + 6) % 7) * 86400000);
  start.setUTCHours(0, 0, 0, 0);
  return start.toISOString().slice(0, 10);
}
const weekAgg = new Map<string, { calls: number; outBytes: number; inBytes: number; outList: number[]; sessions: Set<string> }>();

// 会话内序列分析 (按线程遍历)
const threadCreatedAt = new Map(threads.map((t) => [t.id, t.created_at]));
const threadTitle = new Map(threads.map((t) => [t.id, t.title ?? ""]));

interface SeqItem {
  tool: string;
  file?: string; // Read/Edit/Write 的 path, Grep/Glob 的 path 或 pattern
  pattern?: string;
  query?: string;
  outBytes: number;
  inBytes: number;
  err: boolean;
  giant: boolean;
}

const waste = {
  whiteSearchK1: 0,
  whiteSearchK3: 0,
  whiteSearchK5: 0,
  whiteSearchSessionsK3: new Set<string>(),
  reread: 0,
  rereadBytes: 0,
  rereadSessions: new Set<string>(),
  dupSearch: 0,
  dupSearchBytes: 0,
  dupSearchSessions: new Set<string>(),
  giantCount: 0,
  giantBytes: 0,
  bashNarrowband: 0,
};

const searchTools = new Set(["Grep", "Glob", "WebSearch"]);
const consumeTools = new Set(["Read", "Edit", "Write", "MultiEdit", "WebFetch"]);

for (const tid of threadIds) {
  const recs = perThread[tid] || [];
  if (recs.length === 0) continue;

  // 1) 构建工具调用序列: 配对 tool_use → tool_result
  const seq: SeqItem[] = [];
  const pending = new Map<string, { name: string; input: any; inputBytes: number }>();
  for (const rec of recs) {
    const p = rec.parsed;
    if (!p) continue;
    if (p.kind === "tool_use" && p.toolUses) {
      for (const tu of p.toolUses) {
        pending.set(tu.id, { name: tu.name, input: tu.input, inputBytes: tu.inputBytes });
        totalInBytes += tu.inputBytes;
      }
    } else if (p.kind === "tool_result") {
      const key = p.toolCallId;
      if (!key) continue;
      const excludedIds = threadExcludedUseIds[tid];
      if (excludedIds && excludedIds.has(key)) { orphanExcludedResults++; continue; }
      const pu = pending.get(key);
      if (!pu) { unmatchedToolUses++; orphanUnknownResults++; continue; }
      pending.delete(key);
      matchedResults++;
      const name = pu.name;
      const outBytes = p.bytes;
      totalOutBytes += outBytes;
      if (p.isError) totalErrors++;

      let ag = toolAgg.get(name);
      if (!ag) { ag = { calls: 0, sessions: new Set(), inBytes: 0, outBytes: 0, outList: [], errors: 0 }; toolAgg.set(name, ag); }
      ag.calls++;
      ag.sessions.add(tid);
      ag.inBytes += pu.inputBytes;
      ag.outBytes += outBytes;
      ag.outList.push(outBytes);
      if (p.isError) ag.errors++;
      totalCalls++;

      // 分桶
      for (const b of buckets) {
        if (outBytes >= b.min && outBytes < b.max) {
          bucketCounts[b.name]++;
          bucketSessions[b.name].add(tid);
          bucketBytes[b.name] += outBytes;
          break;
        }
      }

      // 巨型输出
      if (outBytes > 100 * 1024) {
        giantOutputs.push({ thread_id: tid, name, bytes: outBytes, ts: threadCreatedAt.get(tid)! });
        waste.giantCount++;
        waste.giantBytes += outBytes;
      }
      allOutBytes.push(outBytes);
      // Bash 窄带
      if (name === "Bash" && outBytes >= 97 * 1024 && outBytes < 100.5 * 1024) {
        bashNarrowband.push({ bytes: outBytes });
        waste.bashNarrowband++;
      }

      // 会话消耗
      const sc = (sessionCost[tid] = sessionCost[tid] || { inBytes: 0, outBytes: 0, calls: 0 });
      sc.inBytes += pu.inputBytes;
      sc.outBytes += outBytes;
      sc.calls++;

      // 周桶
      const wk = weekKey(threadCreatedAt.get(tid)!);
      if (!weekKeys.includes(wk)) weekKeys.push(wk);
      let wa = weekAgg.get(wk); if (!wa) { wa = { calls: 0, outBytes: 0, inBytes: 0, outList: [], sessions: new Set() }; weekAgg.set(wk, wa); }
      wa.calls++;
      wa.outBytes += outBytes;
      wa.inBytes += pu.inputBytes;
      wa.outList.push(outBytes);
      wa.sessions.add(tid);

      // 序列项 (用于浪费分析)
      const item: SeqItem = { tool: name, outBytes, inBytes: pu.inputBytes, err: !!p.isError, giant: outBytes > 100 * 1024 };
      const input = pu.input || {};
      if (name === "Read" || name === "Edit" || name === "Write" || name === "MultiEdit") {
        item.file = typeof input.path === "string" ? input.path : typeof input.file_path === "string" ? input.file_path : undefined;
      } else if (name === "Grep" || name === "Glob") {
        item.pattern = typeof input.pattern === "string" ? input.pattern : undefined;
        if (typeof input.path === "string") item.file = input.path;
        else if (typeof input.file_path === "string") item.file = input.file_path;
      } else if (name === "WebSearch") {
        item.query = typeof input.query === "string" ? input.query : typeof input.q === "string" ? input.q : undefined;
      }
      seq.push(item);
    }
  }
  // 未配对的 tool_use: pending 剩余 (重复 id 被覆盖 / 无对应 tool_result)
  for (const k of pending.keys()) unmatchedToolUses++;

  // 2) 浪费分析 (按会话)
  // 白搜: Grep/Glob 后 K 次调用内无 Read/Edit/Write/WebFetch
  const wasteFlag = new Array<boolean>(seq.length).fill(false);
  for (let i = 0; i < seq.length; i++) {
    const s = seq[i];
    if (s.tool !== "Grep" && s.tool !== "Glob") continue;
    for (const K of [1, 3, 5]) {
      let consumed = false;
      for (let j = i + 1; j <= Math.min(seq.length - 1, i + K); j++) {
        if (consumeTools.has(seq[j].tool)) { consumed = true; break; }
      }
      if (!consumed) {
        if (K === 1) waste.whiteSearchK1++;
        if (K === 3) { waste.whiteSearchK3++; waste.whiteSearchSessionsK3.add(tid); wasteFlag[i] = true; }
        if (K === 5) waste.whiteSearchK5++;
      }
    }
  }

  // 重读 (定义 B): 同会话同文件相邻 Read 之间无 Edit/Write
  const lastWriteSeq = new Map<string, number>();
  const lastReadSeq = new Map<string, number>();
  for (let i = 0; i < seq.length; i++) {
    const s = seq[i];
    if (s.tool === "Edit" || s.tool === "Write" || s.tool === "MultiEdit") {
      if (s.file) lastWriteSeq.set(s.file, i);
    } else if (s.tool === "Read" && s.file) {
      const lw = lastWriteSeq.get(s.file) ?? -1;
      const lr = lastReadSeq.get(s.file) ?? -1;
      if (lr > lw) {
        // 自上次读以来无写入 → 冗余
        waste.reread++;
        waste.rereadBytes += s.outBytes;
        waste.rereadSessions.add(tid);
        wasteFlag[i] = true;
      }
      lastReadSeq.set(s.file, i);
    }
  }

  // 重复搜索: 同会话同工具相同 pattern/query 的第二次及以后
  const seenSearch = new Map<string, number>();
  seq.forEach((s, i) => {
    if (searchTools.has(s.tool)) {
      const key = s.tool + "|" + (s.pattern ?? s.query ?? "");
      if (key === s.tool + "|" && s.tool !== "WebSearch") return;
      const n = seenSearch.get(key) ?? 0;
      seenSearch.set(key, n + 1);
      if (n >= 1) {
        waste.dupSearch++;
        waste.dupSearchBytes += s.outBytes;
        waste.dupSearchSessions.add(tid);
        wasteFlag[i] = true;
      }
    }
  });

  // 浪费并集: 白搜(K3) ∪ 重读 ∪ 重复搜索 ∪ 巨型
  for (let i = 0; i < seq.length; i++) {
    if (wasteFlag[i] || seq[i].giant) wasteUnionBytes += seq[i].outBytes;
  }
}

// ── token 标定 (metrics 旁证) ──

// 解析 metrics: 提取 sid ∈ threads.id 的 agent_turn_end, 每线程最后一次事件的 input_tokens
interface TurnEnd { sid: string; ts: string; input: number; output: number; iterations: number }
const turnEnds: TurnEnd[] = [];
const compactTriggers: { ts: string; percentage: number; tokens_total: number; tokens_used: number; sid: string }[] = [];
const tokenSpikes: { ts: string; input: number; output: number }[] = [];
const cacheAnomalies: { ts: string; cacheRead: number; input: number; sid: string }[] = [];
const turnEndAll: TurnEnd[] = [];

if (existsSync(METRICS_DIR)) {
  const metricFiles = readdirSync(METRICS_DIR).filter((f) => f.endsWith(".jsonl")).sort();
  for (const f of metricFiles) {
    for (const line of readFileSync(join(METRICS_DIR, f), "utf8").split("\n")) {
      if (!line.trim()) continue;
      let e: any;
      try { e = JSON.parse(line); } catch { continue; }
      const ev = e.event;
      if (ev === "sample.agent_turn_end" && typeof e.data?.total_input_tokens === "number") {
        const rec = { sid: e.sid ?? "", ts: e.ts ?? "", input: e.data.total_input_tokens, output: e.data.total_output_tokens ?? 0, iterations: e.data.iterations ?? 0 };
        turnEndAll.push(rec);
        if (e.sid) turnEnds.push(rec);
      } else if (ev === "trap.compact_trigger") {
        compactTriggers.push({ ts: e.ts ?? "", percentage: e.data?.percentage ?? 0, tokens_total: e.data?.tokens_total ?? 0, tokens_used: e.data?.tokens_used ?? 0, sid: e.sid ?? "" });
      } else if (ev === "threshold.token_spike") {
        tokenSpikes.push({ ts: e.ts ?? "", input: e.data?.input_tokens ?? 0, output: e.data?.output_tokens ?? 0 });
      } else if (ev === "trap.cache_anomaly") {
        cacheAnomalies.push({ ts: e.ts ?? "", cacheRead: e.data?.total_cache_read_tokens ?? 0, input: e.data?.total_input_tokens ?? 0, sid: e.sid ?? "" });
      }
    }
  }
}

// 每线程上下文总字节 (净化后消息文本 + tool_use 入参)
const threadCtxBytes: Record<string, number> = {};
for (const tid of threadIds) {
  let b = 0;
  for (const rec of perThread[tid] || []) {
    if (!rec.parsed) continue;
    b += rec.parsed.bytes + (rec.parsed.toolUses?.reduce((s, tu) => s + tu.inputBytes, 0) ?? 0);
  }
  threadCtxBytes[tid] = b;
}

// ── 标定: 三种方法交叉验证 字节→token 比率 ──
// 方法 A: agent_turn_end —— total_input_tokens 是该 turn 内所有 LLM 调用输入之和,
//          单次调用输入 ≈ input / iterations (iterations>0), 每线程取最大单次输入
// 方法 B: trap.cache_anomaly —— total_input_tokens 为单次请求输入 (含 cache read), 每线程取最大
// 方法 C: trap.compact_trigger —— tokens_used 为触发时上下文实际占用, 每线程取最大
// 共性偏差: 事件时刻的上下文 ≤ 线程全程消息字节 (分母偏大) → 比率是下界

function calibPair(events: { sid: string; tokens: number }[]): { pairs: number; globalRatio: number; median: number; p90: number; tokensTotal: number; bytesTotal: number } {
  const best = new Map<string, number>();
  for (const ev of events) {
    if (!threadCreatedAt.has(ev.sid)) continue;
    if (ev.tokens <= 0) continue;
    const prev = best.get(ev.sid) ?? 0;
    if (ev.tokens > prev) best.set(ev.sid, ev.tokens);
  }
  const ratios: number[] = [];
  let tokensTotal = 0, bytesTotal = 0;
  for (const [sid, tokens] of best) {
    const bytes = threadCtxBytes[sid];
    if (bytes <= 0) continue;
    ratios.push(tokens / bytes);
    tokensTotal += tokens;
    bytesTotal += bytes;
  }
  ratios.sort((a, b) => a - b);
  return {
    pairs: ratios.length,
    globalRatio: bytesTotal > 0 ? tokensTotal / bytesTotal : 0,
    median: quantile(ratios, 0.5),
    p90: quantile(ratios, 0.9),
    tokensTotal,
    bytesTotal,
  };
}

const calibA = calibPair(turnEndAll.filter((t) => t.iterations > 0).map((t) => ({ sid: t.sid, tokens: t.input / t.iterations })));
const calibB = calibPair(cacheAnomalies.map((c) => ({ sid: c.sid, tokens: c.input })));
const calibC = calibPair(compactTriggers.map((c) => ({ sid: c.sid, tokens: c.tokens_used })));

// 命中线程数 (sid ∈ threads.id 且有非零 token 事件)
const hitThreads = new Set<string>();
for (const t of turnEndAll) if (t.input > 0 && threadCreatedAt.has(t.sid)) hitThreads.add(t.sid);
for (const c of cacheAnomalies) if (c.input > 0 && threadCreatedAt.has(c.sid)) hitThreads.add(c.sid);
for (const c of compactTriggers) if (c.tokens_used > 0 && threadCreatedAt.has(c.sid)) hitThreads.add(c.sid);

// ── 汇总输出 ──

function toolRows() {
  const rows = [...toolAgg.entries()].map(([name, ag]) => {
    const sorted = [...ag.outList].sort((a, b) => a - b);
    return {
      name,
      calls: ag.calls,
      sessions: ag.sessions.size,
      inBytes: ag.inBytes,
      outBytes: ag.outBytes,
      totalBytes: ag.inBytes + ag.outBytes,
      mean: ag.outList.length ? ag.outList.reduce((a, b) => a + b, 0) / ag.outList.length : 0,
      p50: quantile(sorted, 0.5),
      p95: quantile(sorted, 0.95),
      max: sorted.length ? sorted[sorted.length - 1] : 0,
      errors: ag.errors,
    };
  });
  rows.sort((a, b) => b.outBytes - a.outBytes);
  return rows;
}

const toolRowsArr = toolRows();

// 长尾集中度: top N 工具出参字节占比
const sortedByOut = [...toolRowsArr].sort((a, b) => b.outBytes - a.outBytes);
const top3Out = sortedByOut.slice(0, 3).reduce((s, r) => s + r.outBytes, 0);
const top5Out = sortedByOut.slice(0, 5).reduce((s, r) => s + r.outBytes, 0);
const top1Out = sortedByOut.slice(0, 1).reduce((s, r) => s + r.outBytes, 0);

// 出参 Top 会话
const sessionRows = Object.entries(sessionCost)
  .map(([tid, c]) => ({ tid, title: threadTitle.get(tid) ?? "", created: threadCreatedAt.get(tid) ?? "", ...c, total: c.inBytes + c.outBytes }))
  .sort((a, b) => b.total - a.total);

// 会话分布形状
const sessionTotals = sessionRows.map((s) => s.total).sort((a, b) => a - b);
const sessionCalls = Object.values(sessionCost).map((c) => c.calls).sort((a, b) => a - b);

// 周演变 (排序)
weekKeys.sort();
const weekRows = weekKeys.map((wk) => {
  const wa = weekAgg.get(wk)!;
  const sorted = [...wa.outList].sort((a, b) => a - b);
  return {
    week: wk,
    calls: wa.calls,
    sessions: wa.sessions.size,
    outBytes: wa.outBytes,
    inBytes: wa.inBytes,
    p50: quantile(sorted, 0.5),
    p95: quantile(sorted, 0.95),
    mean: wa.outList.length ? wa.outList.reduce((a, b) => a + b, 0) / wa.outList.length : 0,
  };
});

// metrics 周演变 (compact / turn_end)
const mWeek = new Map<string, { compact: number; turnEnd: number; turnInput: number; turnOutput: number; spike: number; cacheRead: number; cacheInput: number }>();
for (const c of compactTriggers) { const k = weekKey(c.ts); const w = mWeek.get(k) || { compact: 0, turnEnd: 0, turnInput: 0, turnOutput: 0, spike: 0, cacheRead: 0, cacheInput: 0 }; w.compact++; mWeek.set(k, w); }
for (const t of turnEndAll) { const k = weekKey(t.ts); const w = mWeek.get(k) || { compact: 0, turnEnd: 0, turnInput: 0, turnOutput: 0, spike: 0, cacheRead: 0, cacheInput: 0 }; w.turnEnd++; w.turnInput += t.input; w.turnOutput += t.output; mWeek.set(k, w); }
for (const t of tokenSpikes) { const k = weekKey(t.ts); const w = mWeek.get(k) || { compact: 0, turnEnd: 0, turnInput: 0, turnOutput: 0, spike: 0, cacheRead: 0, cacheInput: 0 }; w.spike++; mWeek.set(k, w); }
for (const t of cacheAnomalies) { const k = weekKey(t.ts); const w = mWeek.get(k) || { compact: 0, turnEnd: 0, turnInput: 0, turnOutput: 0, spike: 0, cacheRead: 0, cacheInput: 0 }; w.cacheRead += t.cacheRead; w.cacheInput += t.input; mWeek.set(k, w); }
const mWeekRows = [...mWeek.entries()].sort((a, b) => a[0].localeCompare(b[0])).map(([k, w]) => ({ week: k, ...w }));

// 输入侧: 巨型入参 (Write/Edit input 大的)
const giantInputs = [...toolAgg.entries()]
  .map(([name, ag]) => ({ name, inBytes: ag.inBytes, calls: ag.calls, meanIn: ag.calls ? ag.inBytes / ag.calls : 0 }))
  .sort((a, b) => b.inBytes - a.inBytes);

// 分桶汇总
const bucketRows = buckets.map((b) => ({
  name: b.name,
  calls: bucketCounts[b.name],
  sessions: bucketSessions[b.name].size,
  bytes: bucketBytes[b.name],
}));

// ── 衍生计算: 占比 / token 换算 / 敏感性 ──
// 主标定: 方法 A 全局比率 (token/字节); 敏感性: 方法 C 全局 + 经验值 250/300/350 per KB
const RATE_A = calibA.globalRatio;
const RATE_C = calibC.globalRatio;
const rates = {
  methodA: RATE_A,
  methodC: RATE_C,
  kb250: 250 / 1024,
  kb300: 300 / 1024,
  kb350: 350 / 1024,
};
const tokensAt = (bytes: number, r: number) => bytes * r;

// 工具级 token 换算 (出参+入参)
interface ToolTokenRow {
  name: string;
  calls: number;
  sessions: number;
  outBytes: number;
  inBytes: number;
  totalBytes: number;
  methodA: number;
  methodC: number;
  kb250: number;
  kb300: number;
  kb350: number;
}
const toolTokenRows: ToolTokenRow[] = toolRowsArr.map((r) => {
  const perRate: Record<string, number> = {};
  for (const [k, v] of Object.entries(rates)) perRate[k] = tokensAt(r.inBytes + r.outBytes, v);
  return {
    name: r.name, calls: r.calls, sessions: r.sessions, outBytes: r.outBytes, inBytes: r.inBytes, totalBytes: r.totalBytes,
    methodA: perRate.methodA, methodC: perRate.methodC, kb250: perRate.kb250, kb300: perRate.kb300, kb350: perRate.kb350,
  };
});
toolTokenRows.sort((a, b) => b.methodA - a.methodA);

// 浪费项 token 成本
const wasteTokens: Record<string, number> = {
  whiteSearchK3_bytes: 0,
  reread_bytes: waste.rereadBytes,
  dupSearch_bytes: waste.dupSearchBytes,
  giant_bytes: waste.giantBytes,
};
// 白搜字节: 白搜的 Grep/Glob 调用出参字节之和 (K=3)
{
  let b = 0;
  for (const tid of threadIds) {
    const recs = perThread[tid] || [];
    const seq2: SeqItem[] = [];
    const pending2 = new Map<string, { name: string; input: any; inputBytes: number }>();
    for (const rec of recs) {
      const p = rec.parsed;
      if (!p) continue;
      if (p.kind === "tool_use" && p.toolUses) {
        for (const tu of p.toolUses) pending2.set(tu.id, { name: tu.name, input: tu.input, inputBytes: tu.inputBytes });
      } else if (p.kind === "tool_result") {
        const key2 = p.toolCallId;
        if (!key2) continue;
        const pu = pending2.get(key2);
        if (!pu) continue;
        pending2.delete(key2);
        const input = pu.input || {};
        const item: SeqItem = { tool: pu.name, outBytes: p.bytes, inBytes: pu.inputBytes, err: !!p.isError, giant: false };
        if (pu.name === "Read" || pu.name === "Edit" || pu.name === "Write" || pu.name === "MultiEdit") {
          item.file = typeof input.path === "string" ? input.path : typeof input.file_path === "string" ? input.file_path : undefined;
        } else if (pu.name === "Grep" || pu.name === "Glob") {
          item.pattern = typeof input.pattern === "string" ? input.pattern : undefined;
        }
        seq2.push(item);
      }
    }
    for (let i = 0; i < seq2.length; i++) {
      const s = seq2[i];
      if (s.tool !== "Grep" && s.tool !== "Glob") continue;
      let consumed = false;
      for (let j = i + 1; j <= Math.min(seq2.length - 1, i + 3); j++) {
        if (consumeTools.has(seq2[j].tool)) { consumed = true; break; }
      }
      if (!consumed) b += s.outBytes;
    }
  }
  wasteTokens.whiteSearchK3_bytes = b;
}
const wasteTokenRows = Object.fromEntries(
  Object.entries(wasteTokens).map(([k, bytes]) => {
    const perRate: Record<string, number> = {};
    for (const [rk, rv] of Object.entries(rates)) perRate[rk] = tokensAt(bytes, rv);
    return [k, { bytes, ...perRate }];
  })
);

// 重读 / 白搜 占比
const readCalls = toolAgg.get("Read")?.calls ?? 0;
const grepCalls = toolAgg.get("Grep")?.calls ?? 0;
const globCalls = toolAgg.get("Glob")?.calls ?? 0;
const wastePct = {
  rereadOfReadCalls: pctNum(waste.reread, readCalls),
  rereadBytesOfReadBytes: pctNum(waste.rereadBytes, toolAgg.get("Read")?.outBytes ?? 0),
  whiteSearchOfSearchCalls: pctNum(waste.whiteSearchK3, grepCalls + globCalls),
  dupSearchOfSearchCalls: pctNum(waste.dupSearch, grepCalls + globCalls + (toolAgg.get("WebSearch")?.calls ?? 0)),
};

// 出参占比 (按工具)
const outShare = toolRowsArr.map((r) => ({ name: r.name, outPct: pctNum(r.outBytes, totalOutBytes), callPct: pctNum(r.calls, totalCalls) }));

// 分桶 token 化
const bucketTokenRows = bucketRows.map((b) => {
  const perRate: Record<string, number> = {};
  for (const [k, v] of Object.entries(rates)) perRate[k] = tokensAt(b.bytes, v);
  return { name: b.name, calls: b.calls, sessions: b.sessions, bytes: b.bytes, ...perRate };
});

// 会话级 token (Top10 用 methodA)
const sessionTokenRows = sessionRows.slice(0, 10).map((s) => ({ ...s, tokensA: tokensAt(s.total, RATE_A), outTokensA: tokensAt(s.outBytes, RATE_A) }));

// 周演变 token 化
const weekTokenRows = weekRows.map((w) => {
  const perRate: Record<string, number> = {};
  for (const [k, v] of Object.entries(rates)) perRate[k] = tokensAt(w.outBytes, v);
  return { ...w, ...perRate };
});

// 巨型输出工具分布
const giantByTool: Record<string, { count: number; bytes: number }> = {};
for (const g of giantOutputs) {
  const e = (giantByTool[g.name] = giantByTool[g.name] || { count: 0, bytes: 0 });
  e.count++; e.bytes += g.bytes;
}

const result = {
  meta: {
    generated_at: new Date().toISOString(),
    db_path: DB_PATH,
    metrics_dir: METRICS_DIR,
  },
  sample: {
    totalThreads: threads.length,
    totalMainMessages: msgRows.length,
    subagentThreads: (db.query("SELECT COUNT(*) AS c FROM threads WHERE parent_thread_id IS NOT NULL").get() as any).c,
    subagentMessages: (db.query("SELECT COUNT(*) AS c FROM messages m JOIN threads t ON m.thread_id=t.id WHERE t.parent_thread_id IS NOT NULL").get() as any).c,
    window: { min: threads[0]?.created_at, max: threads[threads.length - 1]?.updated_at },
    excluded: excl,
    mainThreadsWithNoParsedMsg: threadIds.filter((t) => (perThread[t] || []).length === 0).length,
  },
  exclusion_detail: {
    excluded_flag: excl.excluded_flag,
    system_role: excl.system_role,
    compact_summary: excl.compact_summary,
    parse_fail: excl.parse_fail,
    total_excluded_msgs: excl.excluded_flag + excl.system_role + excl.compact_summary + excl.parse_fail,
    truncated_kept: msgRows.filter((r) => r.truncated && !r.excluded).length,
  },
  totals: {
    calls: totalCalls,
    inBytes: totalInBytes,
    outBytes: totalOutBytes,
    totalBytes: totalInBytes + totalOutBytes,
    errors: totalErrors,
    matchedResults,
    unmatchedToolUses,
    orphanExcludedResults,
    orphanUnknownResults,
  },
  byTool: toolRowsArr,
  top3OutPct: pctNum(top3Out, totalOutBytes),
  top5OutPct: pctNum(top5Out, totalOutBytes),
  top1OutPct: pctNum(top1Out, totalOutBytes),
  buckets: bucketRows,
  giantOutputs: giantOutputs.slice(0, 50),
  giantCount: waste.giantCount,
  giantBytes: waste.giantBytes,
  bashNarrowbandCount: waste.bashNarrowband,
  bashNarrowbandList: bashNarrowband.slice(0, 20),
  waste: {
    whiteSearchK1: waste.whiteSearchK1,
    whiteSearchK3: waste.whiteSearchK3,
    whiteSearchK5: waste.whiteSearchK5,
    whiteSearchSessionsK3: waste.whiteSearchSessionsK3.size,
    reread: waste.reread,
    rereadBytes: waste.rereadBytes,
    rereadSessions: waste.rereadSessions.size,
    dupSearch: waste.dupSearch,
    dupSearchBytes: waste.dupSearchBytes,
    dupSearchSessions: waste.dupSearchSessions.size,
    unionBytes: wasteUnionBytes,
  },
  calibration: {
    hitThreads: hitThreads.size,
    methodA_turnEnd: calibA,
    methodB_cacheAnomaly: calibB,
    methodC_compact: calibC,
    turnEndZero: turnEndAll.filter((t) => t.input === 0).length,
  },
  metrics: {
    window: { min: "", max: "" },
    turnEndTotal: turnEndAll.length,
    turnEndWithSid: turnEnds.length,
    turnEndZero: turnEndAll.filter((t) => t.input === 0).length,
    compactTriggers: compactTriggers.length,
    compactSidHitThreads: new Set(compactTriggers.filter((c) => threadCreatedAt.has(c.sid)).map((c) => c.sid)).size,
    compactPctP50: quantile(compactTriggers.map((c) => c.percentage).sort((a, b) => a - b), 0.5),
    compactPctP95: quantile(compactTriggers.map((c) => c.percentage).sort((a, b) => a - b), 0.95),
    compactTokensTotalP50: quantile(compactTriggers.map((c) => c.tokens_total).sort((a, b) => a - b), 0.5),
    compactTokensUsedP50: quantile(compactTriggers.map((c) => c.tokens_used).sort((a, b) => a - b), 0.5),
    tokenSpikes: tokenSpikes.length,
    cacheAnomalies: cacheAnomalies.length,
    cacheAnomalySidHitThreads: new Set(cacheAnomalies.filter((c) => threadCreatedAt.has(c.sid)).map((c) => c.sid)).size,
    turnInputSum: turnEndAll.reduce((s, t) => s + t.input, 0),
    turnOutputSum: turnEndAll.reduce((s, t) => s + t.output, 0),
    turnInputP50: quantile(turnEndAll.map((t) => t.input).sort((a, b) => a - b), 0.5),
    turnInputP95: quantile(turnEndAll.map((t) => t.input).sort((a, b) => a - b), 0.95),
    turnOutputP50: quantile(turnEndAll.map((t) => t.output).sort((a, b) => a - b), 0.5),
    turnOutputP95: quantile(turnEndAll.map((t) => t.output).sort((a, b) => a - b), 0.95),
    weekly: mWeekRows,
  },
  weekly: weekRows,
  sessions: {
    top10: sessionRows.slice(0, 10),
    p50: quantile(sessionTotals, 0.5),
    p95: quantile(sessionTotals, 0.95),
    max: sessionTotals.length ? sessionTotals[sessionTotals.length - 1] : 0,
    withCalls: sessionRows.length,
    callsP50: quantile(sessionCalls, 0.5),
    callsP95: quantile(sessionCalls, 0.95),
  },
  giantInputs,
  derived: {
    rates,
    toolTokens: toolTokenRows,
    outShare,
    bucketTokens: bucketTokenRows,
    wasteTokens: wasteTokenRows,
    wastePct,
    sessionTokensTop10: sessionTokenRows,
    weekTokens: weekTokenRows,
    giantByTool,
    totalOutDist: {
      p50: quantile([...allOutBytes].sort((a, b) => a - b), 0.5),
      p95: quantile([...allOutBytes].sort((a, b) => a - b), 0.95),
      max: allOutBytes.length ? Math.max(...allOutBytes) : 0,
      unionWasteBytes: wasteUnionBytes,
    },
  },
};

// ── 写出 ──

mkdirSync(OUT_DIR, { recursive: true });
writeFileSync(OUT_FILE, JSON.stringify(result, null, 2));
db.close();

// ── 控制台摘要 ──

printHeader(`工具调用的 token 消耗研究 — ${OUT_NAME}`);
printSection("样本");
printMetric("主线程", result.sample.totalThreads, "个");
printMetric("主线程消息", result.sample.totalMainMessages, "条");
printMetric("subagent 线程", result.sample.subagentThreads, "个");
printMetric("窗口", `${result.sample.window.min?.slice(0, 10)} ~ ${result.sample.window.max?.slice(0, 10)}`);
printMetric("净化排除", result.exclusion_detail.total_excluded_msgs, "条");
printMetric("truncated 保留", result.exclusion_detail.truncated_kept, "条");

printSection("总量");
printMetric("工具调用", result.totals.calls, "次");
printMetric("出参", formatSize(result.totals.outBytes));
printMetric("入参", formatSize(result.totals.inBytes));
printMetric("合计", formatSize(result.totals.totalBytes));
printMetric("错误出参", result.totals.errors, `次 (${result.totals.errors / result.totals.calls * 100}%)`);

printSection("token 标定(方法 A/B/C)");
printMetric("A: agent_turn_end", calibA.pairs ? `${calibA.globalRatio.toFixed(3)} token/B (${calibA.pairs} 线程)` : "不可用");
printMetric("B: cache_anomaly", calibB.pairs ? `${calibB.globalRatio.toFixed(3)} token/B (${calibB.pairs} 线程)` : "不可用(sid 零命中)");
printMetric("C: compact_trigger", calibC.pairs ? `${calibC.globalRatio.toFixed(3)} token/B (${calibC.pairs} 线程)` : "不可用");
printMetric("主口径估算 token", `${Math.round(result.totals.totalBytes * RATE_A / 1e6)} 百万 (methodA)`);

printSection("工具出参 Top 10");
printTable(
  ["工具", "调用", "会话", "出参", "入参", "P50", "P95", "出参占%"],
  toolRowsArr.slice(0, 10).map((r) => [
    r.name,
    String(r.calls),
    String(r.sessions),
    formatSize(r.outBytes),
    formatSize(r.inBytes),
    formatSize(r.p50),
    formatSize(r.p95),
    pctNum(r.outBytes, totalOutBytes).toFixed(1),
  ])
);

printSection("浪费");
printMetric("白搜 K1/K3/K5", `${waste.whiteSearchK1} / ${waste.whiteSearchK3} / ${waste.whiteSearchK5} 次`);
printMetric("重读(定义 B)", `${waste.reread} 次 / ${formatSize(waste.rereadBytes)} (${waste.rereadSessions.size} 会话)`);
printMetric("重复搜索", `${waste.dupSearch} 次 / ${formatSize(waste.dupSearchBytes)}`);
printMetric("巨型输出 >100KB", `${waste.giantCount} 次 / ${formatSize(waste.giantBytes)}`);
printMetric("浪费并集", formatSize(wasteUnionBytes), `(占出参 ${pctNum(wasteUnionBytes, totalOutBytes).toFixed(1)}%)`);
printMetric("Bash ~100KB 窄带", waste.bashNarrowband, "次");

printSection("周演变(出参)");
printTable(
  ["周", "调用", "会话", "出参", "P50", "P95"],
  weekRows.map((w) => [w.week, String(w.calls), String(w.sessions), formatSize(w.outBytes), formatSize(w.p50), formatSize(w.p95)])
);

console.log(`\n结果已写入: ${OUT_FILE}`);
