//! agent_dispatch_study.ts — 超长会话中 Agent 工具派发研究
//!
//! 口径:
//!   超长会话(Long) = 主会话且 message_count >= 500 (42 个)
//!   基准(Base)     = 其余主会话
//!   子 agent = threads.parent_thread_id 指向父会话的线程(全部 hidden=1)
//!   派发记录 = 父会话可见 assistant 消息中的 Agent tool_use (excluded 净化跳过)
//!
//! 维度: 派发概况 / 派发内容 / 子 agent 工作量 / 失败与重试 / 并行与节奏
//!
//! 用法:
//!   bun run src/agent_dispatch_study.ts [--min-messages 500]
//!
//! 输出:
//!   src/data/agent-dispatch-study.json

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { writeFileSync } from "fs";
import { avg, median, quantile } from "./lib/utils.js";

const argv = process.argv.slice(2);
const get = (key: string): string | undefined => {
  const i = argv.indexOf(key);
  return i >= 0 ? argv[i + 1] : undefined;
};
const DB_PATH = get("--db") ?? join(homedir(), ".peri/threads/threads.db");
const MIN_MESSAGES = Number(get("--min-messages") ?? 500);
const OUT_FILE = join(import.meta.dir, "data", "agent-dispatch-study.json");

const db = new Database(DB_PATH, { readonly: true });

// ── 加载 ──

const threads = db.query(
  `SELECT id, title, created_at, updated_at, message_count FROM threads
   WHERE parent_thread_id IS NULL AND hidden = 0 ORDER BY created_at`
).all() as { id: string; title: string | null; created_at: string; updated_at: string; message_count: number }[];

const longIds = new Set(threads.filter((t) => t.message_count >= MIN_MESSAGES).map((t) => t.id));
const L = threads.filter((t) => longIds.has(t.id));
const B = threads.filter((t) => !longIds.has(t.id));

// 父会话可见 assistant 消息中的 Agent tool_use
const msgRows = db.query(
  `SELECT m.thread_id, m.excluded, m.content, m.rowid FROM messages m
   WHERE m.thread_id IN (SELECT id FROM threads WHERE parent_thread_id IS NULL AND hidden = 0)
   AND m.role = 'assistant' ORDER BY m.thread_id, m.rowid`
).all() as { thread_id: string; excluded: number; content: string; rowid: number }[];

interface Dispatch {
  thread_id: string;
  rowid: number;
  seq: number;              // 会话内消息序号
  description: string;
  prompt: string;
  subagent_type: string;
  run_in_background: boolean;
  fork: boolean;
  promptLen: number;
  descLen: number;
}

const dispatchByThread: Record<string, Dispatch[]> = {};
const allDispatch: Dispatch[] = [];
let purge = { excluded_skipped: 0, parse_fail: 0 };

for (const r of msgRows) {
  if (r.excluded) { purge.excluded_skipped++; continue; }
  let p: any;
  try { p = JSON.parse(r.content); } catch { purge.parse_fail++; continue; }
  const blocks: any[] = Array.isArray(p.content) ? p.content : [];
  const tcs: any[] = Array.isArray(p.tool_calls) ? p.tool_calls : [];
  const uses = blocks.filter((b) => b && b.type === "tool_use" && b.name === "Agent");
  if (uses.length === 0 && tcs.length > 0) {
    // 旧格式兜底
    for (const tc of tcs) if (tc && tc.name === "Agent") uses.push({ id: tc.id, name: tc.name, input: tc.arguments ?? tc.input });
  }
  for (const u of uses) {
    const input = u.input ?? {};
    const d: Dispatch = {
      thread_id: r.thread_id,
      rowid: r.rowid,
      seq: 0, // 下面填
      description: String(input.description ?? ""),
      prompt: String(input.prompt ?? ""),
      subagent_type: String(input.subagent_type ?? input.name ?? ""),
      run_in_background: !!input.run_in_background,
      fork: !!input.fork,
      promptLen: String(input.prompt ?? "").length,
      descLen: String(input.description ?? "").length,
    };
    (dispatchByThread[r.thread_id] = dispatchByThread[r.thread_id] ?? []).push(d);
    allDispatch.push(d);
  }
}

// 会话内序号: 每条 assistant 消息的全局位置 → 需要按 thread 计数
// 简化: 派发记录按 rowid 排序, seq 用该 thread 内消息计数近似(遍历填充)
{
  const counters: Record<string, number> = {};
  for (const r of msgRows) {
    counters[r.thread_id] = (counters[r.thread_id] ?? 0) + 1;
    if (!r.excluded) {
      // 只对可见消息计数, 与消息序号口径一致
    }
  }
  // 重新算: seq = 该会话内第几条可见消息
  const visCounters: Record<string, number> = {};
  for (const r of msgRows) {
    if (!r.excluded) visCounters[r.thread_id] = (visCounters[r.thread_id] ?? 0) + 1;
    // 匹配 dispatch: 用 rowid 找到对应消息
    const list = dispatchByThread[r.thread_id];
    if (list) {
      for (const d of list) {
        if (d.rowid === r.rowid && d.seq === 0) d.seq = visCounters[r.thread_id];
      }
    }
  }
}

// 子 agent 线程
const subRows = db.query(
  `SELECT id, parent_thread_id, title, created_at, updated_at, message_count, agent_status
   FROM threads WHERE parent_thread_id IS NOT NULL ORDER BY created_at`
).all() as { id: string; parent_thread_id: string; title: string | null; created_at: string; updated_at: string; message_count: number; agent_status: string }[];

const subsByParent: Record<string, typeof subRows> = {};
for (const s of subRows) (subsByParent[s.parent_thread_id] = subsByParent[s.parent_thread_id] ?? []).push(s);

// metrics: Agent 相关 tool.error
let metricsAgentError = 0;
let metricsAgentErrorByType: Record<string, number> = {};
try {
  const { readdirSync, readFileSync } = await import("fs");
  const dir = join(homedir(), ".peri/metrics");
  for (const f of readdirSync(dir).filter((x) => x.endsWith(".jsonl"))) {
    for (const line of readFileSync(join(dir, f), "utf8").split("\n")) {
      if (!line.includes("tool.error")) continue;
      try {
        const e = JSON.parse(line);
        if (e.data?.name === "Agent" || e.data?.name === "Task") {
          metricsAgentError++;
          metricsAgentErrorByType[e.data.name] = (metricsAgentErrorByType[e.data.name] ?? 0) + 1;
        }
      } catch { /* 忽略 */ }
    }
  }
} catch { /* metrics 不可用 */ }

// ── 统计 ──

function groupStats(arr: number[]): { n: number; avg: number; p50: number; p90: number; p99: number; max: number; sum: number } {
  return { n: arr.length, avg: avg(arr), p50: median(arr), p90: quantile(arr, 0.9), p99: quantile(arr, 0.99), max: arr.length ? Math.max(...arr) : 0, sum: arr.reduce((a, b) => a + b, 0) };
}

const longDispatch = allDispatch.filter((d) => longIds.has(d.thread_id));
const baseDispatch = allDispatch.filter((d) => !longIds.has(d.thread_id));

// A. 派发概况
const A = {
  long: {
    sessions: L.length,
    withDispatch: L.filter((t) => (dispatchByThread[t.id]?.length ?? 0) > 0).length,
    dispatchPerSession: groupStats(L.map((t) => dispatchByThread[t.id]?.length ?? 0)),
    dispatchTotal: longDispatch.length,
    per100msgs: groupStats(L.map((t) => ((dispatchByThread[t.id]?.length ?? 0) / t.message_count) * 100)),
    bgRatio: longDispatch.filter((d) => d.run_in_background).length / longDispatch.length,
    forkCount: longDispatch.filter((d) => d.fork).length,
    typeDist: Object.entries(longDispatch.reduce<Record<string, number>>((a, d) => ((a[d.subagent_type] = (a[d.subagent_type] ?? 0) + 1), a), {})).sort((a, b) => b[1] - a[1]),
  },
  base: {
    sessions: B.length,
    withDispatch: B.filter((t) => (dispatchByThread[t.id]?.length ?? 0) > 0).length,
    dispatchPerSession: groupStats(B.map((t) => dispatchByThread[t.id]?.length ?? 0)),
    dispatchTotal: baseDispatch.length,
    per100msgs: groupStats(B.map((t) => ((dispatchByThread[t.id]?.length ?? 0) / Math.max(1, t.message_count)) * 100)),
    bgRatio: baseDispatch.filter((d) => d.run_in_background).length / Math.max(1, baseDispatch.length),
    typeDist: Object.entries(baseDispatch.reduce<Record<string, number>>((a, d) => ((a[d.subagent_type] = (a[d.subagent_type] ?? 0) + 1), a), {})).sort((a, b) => b[1] - a[1]),
  },
};

// B. 派发内容
const B_ = {
  long: {
    descLen: groupStats(longDispatch.map((d) => d.descLen)),
    promptLen: groupStats(longDispatch.map((d) => d.promptLen)),
    longPromptPct: longDispatch.filter((d) => d.promptLen > 2000).length / longDispatch.length,
    promptWithContext: longDispatch.filter((d) => /(file_path|path|\.rs|\.ts|\.md|\/[\w.-]+\/)/.test(d.prompt)).length / longDispatch.length,
    promptWithQuote: longDispatch.filter((d) => /[「"'\x60]/.test(d.prompt)).length / longDispatch.length,
  },
  base: {
    descLen: groupStats(baseDispatch.map((d) => d.descLen)),
    promptLen: groupStats(baseDispatch.map((d) => d.promptLen)),
    longPromptPct: baseDispatch.filter((d) => d.promptLen > 2000).length / Math.max(1, baseDispatch.length),
  },
};

// C. 子 agent 工作量
const longSubs = subRows.filter((s) => longIds.has(s.parent_thread_id));
const baseSubs = subRows.filter((s) => !longIds.has(s.parent_thread_id));

const C = {
  long: {
    subsTotal: longSubs.length,
    subsPerSession: groupStats(L.map((t) => (subsByParent[t.id]?.length ?? 0))),
    subMessages: groupStats(longSubs.map((s) => s.message_count)),
    subWallMinutes: groupStats(longSubs.map((s) => (new Date(s.updated_at).getTime() - new Date(s.created_at).getTime()) / 60000)),
    subStatus: Object.entries(longSubs.reduce<Record<string, number>>((a, s) => ((a[s.agent_status] = (a[s.agent_status] ?? 0) + 1), a), {})).sort((a, b) => b[1] - a[1]),
    errorSubs: longSubs.filter((s) => s.agent_status === "error").length,
    cancelledSubs: longSubs.filter((s) => s.agent_status === "cancelled").length,
    zeroMsgSubs: longSubs.filter((s) => s.message_count === 0).length,
    subPer100: groupStats(L.map((t) => ((subsByParent[t.id]?.length ?? 0) / t.message_count) * 100)),
  },
  base: {
    subsTotal: baseSubs.length,
    subMessages: groupStats(baseSubs.map((s) => s.message_count)),
    subStatus: Object.entries(baseSubs.reduce<Record<string, number>>((a, s) => ((a[s.agent_status] = (a[s.agent_status] ?? 0) + 1), a), {})).sort((a, b) => b[1] - a[1]),
    errorSubs: baseSubs.filter((s) => s.agent_status === "error").length,
  },
};

// D. 失败与重试: 同 description 在同一会话重复派发
function dupCount(ds: Dispatch[]): { dupGroups: number; dupDispatches: number; maxDup: number } {
  const byDesc: Record<string, number> = {};
  for (const d of ds) byDesc[d.description] = (byDesc[d.description] ?? 0) + 1;
  let dupGroups = 0, dupDispatches = 0, maxDup = 0;
  for (const [desc, n] of Object.entries(byDesc)) {
    if (n > 1) {
      dupGroups++;
      dupDispatches += n;
      maxDup = Math.max(maxDup, n);
    }
  }
  return { dupGroups, dupDispatches, maxDup };
}

const D = {
  metricsAgentError,
  metricsAgentErrorByType,
  longDup: dupCount(longDispatch),
  baseDup: dupCount(baseDispatch),
};

// E. 并行度: 同一父会话下子 agent 时间重叠
function maxConcurrency(subs: { created_at: string; updated_at: string }[]): number {
  // 事件扫描
  const events: { t: number; delta: number }[] = [];
  for (const s of subs) {
    events.push({ t: new Date(s.created_at).getTime(), delta: 1 });
    events.push({ t: new Date(s.updated_at).getTime(), delta: -1 });
  }
  events.sort((a, b) => a.t - b.t);
  let cur = 0, max = 0;
  for (const e of events) { cur += e.delta; max = Math.max(max, cur); }
  return max;
}

const E = {
  long: {
    maxConcurrency: groupStats(L.map((t) => maxConcurrency(subsByParent[t.id] ?? []))),
    // 派发相位(时间口径, 基于子 agent 创建时间, 不受消息压缩影响)
    phaseDist: (() => {
      const phases = { early: 0, mid: 0, late: 0 };
      for (const t of L) {
        for (const s of subsByParent[t.id] ?? []) {
          const span = (new Date(t.updated_at).getTime() - new Date(t.created_at).getTime()) + 1;
          const ratio = (new Date(s.created_at).getTime() - new Date(t.created_at).getTime()) / span;
          if (ratio < 0.33) phases.early++;
          else if (ratio < 0.67) phases.mid++;
          else phases.late++;
        }
      }
      return phases;
    })(),
    // 连续派发间隔(分钟)
    dispatchGaps: groupStats((() => {
      const gaps: number[] = [];
      for (const t of L) {
        const arr = [...(subsByParent[t.id] ?? [])].sort((a, b) => a.created_at.localeCompare(b.created_at));
        for (let i = 1; i < arr.length; i++) gaps.push((new Date(arr[i].created_at).getTime() - new Date(arr[i - 1].created_at).getTime()) / 60000);
      }
      return gaps;
    })()),
    // 压缩影响量化: 有子 agent 但可见派发为 0 的会话
    hiddenDispatch: L.filter((t) => (subsByParent[t.id]?.length ?? 0) > 0 && (dispatchByThread[t.id]?.length ?? 0) === 0).length,
  },
  base: {
    maxConcurrency: groupStats(B.map((t) => maxConcurrency(subsByParent[t.id] ?? []))),
  },
};

// ── 输出 ──

const result = { meta: { db: DB_PATH, min_messages: MIN_MESSAGES, generated_at: new Date().toISOString() }, purge, A, B: B_, C, D, E };
writeFileSync(OUT_FILE, JSON.stringify(result, null, 2));
db.close();

// ── 终端摘要 ──

console.log(`═══ Agent 工具派发研究(≥${MIN_MESSAGES} 消息) ═══`);
console.log(`派发记录: 长 ${longDispatch.length} 次 / ${A.long.sessions} 会话(有派发 ${A.long.withDispatch}) | 基准 ${baseDispatch.length} 次 / ${B.length} 会话`);
console.log(`每会话派发: 长 P50=${A.long.dispatchPerSession.p50} avg=${A.long.dispatchPerSession.avg.toFixed(1)} max=${A.long.dispatchPerSession.max} | 基准 P50=${A.base.dispatchPerSession.p50}`);
console.log(`每100消息派发: 长 ${A.long.per100msgs.avg.toFixed(2)} vs 基准 ${A.base.per100msgs.avg.toFixed(2)}`);
console.log(`后台比例: 长 ${(A.long.bgRatio * 100).toFixed(0)}% vs 基准 ${(A.base.bgRatio * 100).toFixed(0)}% | fork: ${A.long.forkCount}`);
console.log(`subagent_type 分布(长): ${A.long.typeDist.map(([t, n]) => `${t}(${n})`).join(" ")}`);
console.log(`  基准: ${A.base.typeDist.map(([t, n]) => `${t}(${n})`).join(" ")}`);
console.log(`desc 长度: 长 P50=${B_.long.descLen.p50} P90=${B_.long.descLen.p90} | prompt 长度: P50=${B_.long.promptLen.p50} P90=${B_.long.promptLen.p90}`);
console.log(`子 agent: 长 ${C.long.subsTotal} 个(${C.long.subsPerSession.avg.toFixed(1)}/会话) | 消息 P50=${C.long.subMessages.p50} 时长P50=${(C.long.subWallMinutes.p50).toFixed(0)}m`);
console.log(`子 agent 状态(长): ${C.long.subStatus.map(([s, n]) => `${s}=${n}`).join(" ")}`);
console.log(`  基准: ${C.base.subStatus.map(([s, n]) => `${s}=${n}`).join(" ")}`);
console.log(`metrics Agent tool.error: ${metricsAgentError} 次`);
console.log(`重复 description 派发(长): ${D.longDup.dupGroups} 组 / ${D.longDup.dupDispatches} 次 (max ${D.longDup.maxDup})`);
console.log(`最大并行子agent: 长 P50=${E.long.maxConcurrency.p50} max=${E.long.maxConcurrency.max} | 基准 max=${E.base.maxConcurrency.max}`);
console.log(`派发相位(时间口径, 子agent创建时间): ${JSON.stringify(E.long.phaseDist)}`);
console.log(`派发间隔(分钟): P50=${Math.round(E.long.dispatchGaps.p50)} P90=${Math.round(E.long.dispatchGaps.p90)}`);
console.log(`压缩影响: ${E.long.hiddenDispatch} 个会话有子agent但可见派发=0(派发记录被compact吞掉)`);
console.log(`输出: ${OUT_FILE}`);
