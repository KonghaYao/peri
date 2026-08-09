//! ratio_analysis.ts — 比例关系调查报告的计算器(只读)
//!
//! 输入: 主窗口 JSON + 备份窗口 JSON(均由 tool_token_consumption.ts 生成);
//!       错误维度(错误调用出参字节/错误率按周)JSON 中缺失, 本脚本直接从两个 DB 重算。
//! 输出: <out-dir>/ratio-analysis-calc.json, 报告《ratio-analysis.md》的所有数字均出自此文件。
//!
//! 用法:
//!   bun run src/ratio_analysis.ts [--main-json ...] [--backup-json ...] [--main-db ...] [--backup-db ...] [--out ...]

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { readFileSync, writeFileSync, mkdirSync } from "fs";

// ── CLI ──
const argv = process.argv.slice(2);
const get = (k: string) => argv[argv.indexOf(k) + 1];
const MAIN_JSON = get("--main-json") ?? join(import.meta.dir, "data/tool-token-consumption.json");
const BACKUP_JSON = get("--backup-json") ?? "/tmp/backup-window/backup.json";
const MAIN_DB = get("--main-db") ?? join(homedir(), ".peri/threads/threads.db");
const BACKUP_DB = get("--backup-db") ?? "/tmp/backup-window/backup.db";
const OUT = get("--out") ?? "/tmp/ratio-analysis-calc.json";

const pct = (n: number, d: number) => (d === 0 ? 0 : (n / d) * 100);
const relDiff = (a: number, b: number) => (a === 0 ? (b === 0 ? 0 : Infinity) : ((b - a) / a) * 100);

// ── 错误维度重算(JSON 缺失): 错误调用出参字节(按工具/按周) ──
interface ToolErrAgg { calls: number; errors: number; errBytes: number; errInBytes: number }
interface WeekErrAgg { calls: number; errors: number; errBytes: number }

function computeErrorDim(dbPath: string): { byTool: Record<string, ToolErrAgg>; byWeek: Record<string, WeekErrAgg> } {
  const db = new Database(dbPath, { readonly: true });
  const threads = db.query(
    `SELECT id, created_at FROM threads WHERE parent_thread_id IS NULL ORDER BY created_at ASC`
  ).all() as { id: string; created_at: string }[];
  const tids = threads.map((t) => t.id);
  const createdAt = new Map(threads.map((t) => [t.id, t.created_at]));
  const weekKey = (iso: string) => {
    const d = new Date(iso);
    const start = new Date(d.getTime() - ((d.getUTCDay() + 6) % 7) * 86400000);
    start.setUTCHours(0, 0, 0, 0);
    return start.toISOString().slice(0, 10);
  };
  // 与主脚本一致的净化口径: excluded 列 / system role / <system-reminder> 用户消息 / 解析失败
  const rows = db.query(
    `SELECT m.thread_id, m.role, m.content, m.excluded FROM messages m
     WHERE m.thread_id IN (SELECT id FROM threads WHERE parent_thread_id IS NULL)
     ORDER BY m.thread_id, m.rowid ASC`
  ).all() as { thread_id: string; role: string; content: string; excluded: number }[];

  const byTool: Record<string, ToolErrAgg> = {};
  const byWeek: Record<string, WeekErrAgg> = {};
  let pending = new Map<string, { name: string; inputBytes: number }>();
  const excludedUseIds = new Map<string, Set<string>>(); // 被净化 assistant 消息中的 tool_use id

  const parseUseIds = (content: string): { id: string; name: string; inputBytes: number }[] => {
    let msg: any;
    try { msg = JSON.parse(content); } catch { return []; }
    if (!msg || typeof msg !== "object") return [];
    const out: { id: string; name: string; inputBytes: number }[] = [];
    const blocks: any[] = Array.isArray(msg.content) ? msg.content : [];
    let hasUse = false;
    for (const b of blocks) {
      if (b && typeof b === "object" && b.type === "tool_use") {
        hasUse = true;
        const inputBytes = Buffer.byteLength(JSON.stringify(b.input ?? {}), "utf8");
        out.push({ id: b.id, name: b.name, inputBytes });
      }
    }
    if (!hasUse && Array.isArray(msg.tool_calls)) {
      for (const tc of msg.tool_calls) {
        if (!tc || typeof tc !== "object" || !tc.id) continue;
        const inputBytes = Buffer.byteLength(JSON.stringify(tc.arguments ?? {}), "utf8");
        out.push({ id: tc.id, name: tc.name ?? "unknown", inputBytes });
      }
    }
    return out;
  };

  for (const r of rows) {
    if (r.excluded) {
      // 记录其中 tool_use id, 用于净化对应 tool_result (与主脚本 threadExcludedUseIds 一致)
      if (r.role === "assistant" && r.content.includes("tool_use")) {
        const s = excludedUseIds.get(r.thread_id) ?? new Set<string>();
        for (const tu of parseUseIds(r.content)) s.add(tu.id);
        excludedUseIds.set(r.thread_id, s);
      }
      continue;
    }
    if (r.role === "system") continue;
    if (r.role === "user" && r.content.includes("<system-reminder>")) continue;
    let msg: any;
    try { msg = JSON.parse(r.content); } catch { continue; }
    if (!msg || typeof msg !== "object") continue;
    if (r.role === "assistant" && msg.role === "assistant") {
      for (const tu of parseUseIds(r.content)) pending.set(tu.id, { name: tu.name, inputBytes: tu.inputBytes });
    } else if (r.role === "tool" && msg.role === "tool") {
      const id = msg.tool_call_id;
      if (!id) continue;
      if (excludedUseIds.get(r.thread_id)?.has(id)) continue; // orphanExcludedResults
      const pu = pending.get(id);
      if (!pu) continue;
      pending.delete(id);
      const bytes = Buffer.byteLength(typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content), "utf8");
      const isErr = !!msg.is_error;
      const wk = weekKey(createdAt.get(r.thread_id)!);
      let ta = byTool[pu.name]; if (!ta) { ta = { calls: 0, errors: 0, errBytes: 0, errInBytes: 0 }; byTool[pu.name] = ta; }
      ta.calls++;
      if (isErr) { ta.errors++; ta.errBytes += bytes; ta.errInBytes += pu.inputBytes; }
      let wa = byWeek[wk]; if (!wa) { wa = { calls: 0, errors: 0, errBytes: 0 }; byWeek[wk] = wa; }
      wa.calls++;
      if (isErr) { wa.errors++; wa.errBytes += bytes; }
    }
  }
  db.close();
  return { byTool, byWeek };
}

// ── 工具级排名/错位 ──
interface RankRow { name: string; calls: number; outBytes: number; inBytes: number; totalBytes: number; outPct: number; callPct: number }

function toolRanks(byTool: any[], totalCalls: number, totalOut: number, totalIn: number): { rows: RankRow[]; mismatch: { name: string; callRank: number; outRank: number; inRank: number; delta: number; callPct: number; outPct: number; inPct: number }[]; mismatchIn: { name: string; callRank: number; inRank: number; delta: number; callPct: number; inPct: number }[] } {
  const byCalls = [...byTool].sort((a, b) => b.calls - a.calls);
  const byOut = [...byTool].sort((a, b) => b.outBytes - a.outBytes);
  const byIn = [...byTool].sort((a, b) => b.inBytes - a.inBytes);
  const rankOf = (arr: any[], name: string) => arr.findIndex((x) => x.name === name) + 1;
  const rows: RankRow[] = byCalls.map((t) => ({
    name: t.name, calls: t.calls, outBytes: t.outBytes, inBytes: t.inBytes, totalBytes: t.totalBytes,
    outPct: pct(t.outBytes, totalOut), callPct: pct(t.calls, totalCalls),
  }));
  const mismatch = rows.filter((r) => rankOf(byOut, r.name) !== rankOf(byCalls, r.name)).map((r) => ({
    name: r.name, callRank: rankOf(byCalls, r.name), outRank: rankOf(byOut, r.name), inRank: rankOf(byIn, r.name),
    delta: rankOf(byOut, r.name) - rankOf(byCalls, r.name), callPct: r.callPct, outPct: r.outPct,
    inPct: pct(r.inBytes, totalIn),
  })).sort((a, b) => b.delta - a.delta);
  const mismatchIn = rows.filter((r) => rankOf(byIn, r.name) !== rankOf(byCalls, r.name)).map((r) => ({
    name: r.name, callRank: rankOf(byCalls, r.name), inRank: rankOf(byIn, r.name),
    delta: rankOf(byIn, r.name) - rankOf(byCalls, r.name), callPct: r.callPct, inPct: pct(r.inBytes, totalIn),
  })).sort((a, b) => b.delta - a.delta);
  return { rows, mismatch, mismatchIn };
}

function inOutDim(byTool: any[], totalIn: number, totalOut: number, weekly: any[]): any {
  const rows = byTool.map((t) => ({
    name: t.name, calls: t.calls, inBytes: t.inBytes, outBytes: t.outBytes,
    ratio: t.outBytes === 0 ? Infinity : t.inBytes / t.outBytes,
    inPctOfAll: pct(t.inBytes, totalIn), outPctOfAll: pct(t.outBytes, totalOut),
  }));
  const readLike = ["Read", "Grep", "Glob", "WebFetch", "WebSearch", "SkillTool", "Skill", "folder_operations", "AgentResult", "SearchExtraTools"];
  const writeLike = ["Edit", "Write", "LineEdit", "HashlineEdit", "TodoWrite", "ExecuteExtraTool", "AskUserQuestion", "Agent"];
  const grp = (names: string[]) => {
    const sel = rows.filter((r) => names.includes(r.name));
    return {
      inBytes: sel.reduce((s, r) => s + r.inBytes, 0), outBytes: sel.reduce((s, r) => s + r.outBytes, 0),
      inPct: pct(sel.reduce((s, r) => s + r.inBytes, 0), totalIn), outPct: pct(sel.reduce((s, r) => s + r.outBytes, 0), totalOut),
    };
  };
  const weeklyInOut = weekly.map((w) => ({
    week: w.week, calls: w.calls, inBytes: w.inBytes, outBytes: w.outBytes,
    ratio: w.outBytes === 0 ? Infinity : w.inBytes / w.outBytes, inPct: pct(w.inBytes, w.inBytes + w.outBytes),
  }));
  return {
    overall: { inBytes: totalIn, outBytes: totalOut, ratio: totalIn / totalOut, inPctOfTotal: pct(totalIn, totalIn + totalOut) },
    readLike: grp(readLike), writeLike: grp(writeLike),
    topInOut: [...rows].sort((a, b) => (b.ratio === Infinity ? 1 : 0) - (a.ratio === Infinity ? 1 : 0) || b.ratio - a.ratio).slice(0, 8).map((r) => ({ name: r.name, ratio: r.ratio === Infinity ? "∞" : +r.ratio.toFixed(2), inPct: +r.inPctOfAll.toFixed(2), outPct: +r.outPctOfAll.toFixed(2) })),
    topOutIn: rows.filter((r) => r.outBytes > 0).sort((a, b) => a.ratio - b.ratio).slice(0, 8).map((r) => ({ name: r.name, ratio: +r.ratio.toFixed(2), inPct: +r.inPctOfAll.toFixed(2), outPct: +r.outPctOfAll.toFixed(2) })),
    weekly: weeklyInOut,
  };
}

function wasteDim(j: any): any {
  const w = j.waste, t = j.totals, wt = j.derived?.wasteTokens ?? {}, wp = j.derived?.wastePct ?? {};
  const whiteBytes = wt.whiteSearchK3_bytes?.bytes ?? 0;
  const giantCount = j.giantCount ?? 0;
  const giantBytes = j.giantBytes ?? 0;
  const searchCalls = (j.byTool.find((x: any) => x.name === "Grep")?.calls ?? 0) + (j.byTool.find((x: any) => x.name === "Glob")?.calls ?? 0);
  const allSearchCalls = searchCalls + (j.byTool.find((x: any) => x.name === "WebSearch")?.calls ?? 0);
  const readCalls = j.byTool.find((x: any) => x.name === "Read")?.calls ?? 0;
  const readBytes = j.byTool.find((x: any) => x.name === "Read")?.outBytes ?? 0;
  return {
    ofCalls: {
      rereadOfReadCalls: pct(w.reread, readCalls),
      rereadOfAllCalls: pct(w.reread, t.calls),
      whiteOfSearchCalls: pct(w.whiteSearchK3, searchCalls),
      whiteOfAllCalls: pct(w.whiteSearchK3, t.calls),
      dupOfSearchCalls: pct(w.dupSearch, allSearchCalls),
      dupOfAllCalls: pct(w.dupSearch, t.calls),
      giantOfAllCalls: pct(giantCount, t.calls),
    },
    ofOut: {
      reread: pct(w.rereadBytes, t.outBytes),
      whiteK3: pct(whiteBytes, t.outBytes),
      dup: pct(w.dupSearchBytes, t.outBytes),
      giant: pct(giantBytes, t.outBytes),
      union: pct(w.unionBytes, t.outBytes),
    },
    ofUnion: {
      reread: pct(w.rereadBytes, w.unionBytes),
      whiteK3: pct(whiteBytes, w.unionBytes),
      dup: pct(w.dupSearchBytes, w.unionBytes),
      giant: pct(giantBytes, w.unionBytes),
    },
    readBytesCovered: pct(w.rereadBytes, readBytes),
    wastePct: wp,
  };
}

function concentration(j: any): any {
  const t = j.totals;
  const byOut = [...j.byTool].sort((a, b) => b.outBytes - a.outBytes);
  const topOut = (n: number) => pct(byOut.slice(0, n).reduce((s, x) => s + x.outBytes, 0), t.outBytes);
  const top10Sessions = j.sessions.top10;
  const sTop10Total = top10Sessions.reduce((s: number, x: any) => s + x.total, 0);
  const sTop10Calls = top10Sessions.reduce((s: number, x: any) => s + x.calls, 0);
  return {
    tool: {
      top1: topOut(1), top3: topOut(3), top5: topOut(5), top10: topOut(10),
      top10Names: byOut.slice(0, 10).map((x: any) => x.name),
    },
    session: {
      top10PctOfTotal: pct(sTop10Total, t.totalBytes),
      top10CallsPct: pct(sTop10Calls, t.calls),
      top1Total: top10Sessions[0]?.total ?? 0, top1Calls: top10Sessions[0]?.calls ?? 0,
      top10OfTop1: pct(top10Sessions[0]?.total ?? 0, sTop10Total),
      p50: j.sessions.p50, p95: j.sessions.p95, max: j.sessions.max, withCalls: j.sessions.withCalls,
    },
  };
}

function crossWindowDim(j: any): any {
  const s = j.sample, t = j.totals;
  const msgsPerThread = s.totalMainMessages / s.totalThreads;
  const callsPerThread = t.calls / s.totalThreads;
  const callsPerActive = t.calls / j.sessions.withCalls;
  const byOut = [...j.byTool].sort((a, b) => b.outBytes - a.outBytes);
  const topIn = [...j.byTool].sort((a, b) => b.inBytes - a.inBytes);
  const readLike = ["Read", "Grep", "Glob", "WebFetch", "WebSearch", "SkillTool", "Skill", "folder_operations", "AgentResult", "SearchExtraTools"];
  const writeLike = ["Edit", "Write", "LineEdit", "HashlineEdit", "TodoWrite", "ExecuteExtraTool", "AskUserQuestion", "Agent"];
  const grpPct = (names: string[]) => {
    const sel = byOut.filter((x: any) => names.includes(x.name));
    return { outPct: pct(sel.reduce((s2, x) => s2 + x.outBytes, 0), t.outBytes), inPct: pct(sel.reduce((s2, x) => s2 + x.inBytes, 0), t.inBytes) };
  };
  return {
    msgsPerThread, callsPerThread, callsPerActive,
    threads: s.totalThreads, msgs: s.totalMainMessages, calls: t.calls,
    noParsedThreadsPct: pct(s.mainThreadsWithNoParsedMsg, s.totalThreads),
    inOutRatio: t.inBytes / t.outBytes, inPctOfTotal: pct(t.inBytes, t.totalBytes),
    errRate: pct(t.errors, t.calls),
    top1Tool: { name: byOut[0]?.name, outPct: pct(byOut[0]?.outBytes ?? 0, t.outBytes) },
    top3ToolOutPct: pct(byOut.slice(0, 3).reduce((s2, x) => s2 + x.outBytes, 0), t.outBytes),
    readLike: grpPct(readLike), writeLike: grpPct(writeLike),
    topInTool: { name: topIn[0]?.name, inPct: pct(topIn[0]?.inBytes ?? 0, t.inBytes) },
    writeInPct: pct((topIn.find((x: any) => x.name === "Write")?.inBytes ?? 0) + (topIn.find((x: any) => x.name === "Edit")?.inBytes ?? 0), t.inBytes),
    bucketCallsPct: Object.fromEntries(j.buckets.map((b: any) => [b.name, pct(b.calls, t.calls)])),
    bucketBytesPct: Object.fromEntries(j.buckets.map((b: any) => [b.name, pct(b.bytes, t.outBytes)])),
    wasteUnionPct: pct(j.waste.unionBytes, t.outBytes),
  };
}

// ── 主流程 ──
const main = JSON.parse(readFileSync(MAIN_JSON, "utf8"));
const backup = JSON.parse(readFileSync(BACKUP_JSON, "utf8"));

const mainErr = computeErrorDim(MAIN_DB);
const backupErr = computeErrorDim(BACKUP_DB);

const errDim = (j: any, err: { byTool: Record<string, ToolErrAgg>; byWeek: Record<string, WeekErrAgg> }) => {
  const t = j.totals;
  const byToolRows = Object.entries(err.byTool)
    .filter(([, v]) => v.errors > 0)
    .map(([name, v]) => ({ name, calls: v.calls, errors: v.errors, errRate: pct(v.errors, v.calls), errBytes: v.errBytes, errBytesPctOfToolOut: pct(v.errBytes, j.byTool.find((x: any) => x.name === name)?.outBytes ?? 0), errInBytes: v.errInBytes }))
    .sort((a, b) => b.errBytes - a.errBytes);
  const totalErrBytes = Object.values(err.byTool).reduce((s, v) => s + v.errBytes, 0);
  const totalErrInBytes = Object.values(err.byTool).reduce((s, v) => s + v.errInBytes, 0);
  const byWeek = Object.entries(err.byWeek)
    .map(([week, v]) => ({ week, calls: v.calls, errors: v.errors, errRate: pct(v.errors, v.calls), errBytes: v.errBytes, errBytesPctOfWeek: pct(v.errBytes, v.calls > 0 ? v.calls : 1) }))
    .sort((a, b) => a.week.localeCompare(b.week));
  return {
    totalErrors: t.errors, errRate: pct(t.errors, t.calls),
    totalErrBytes, errBytesPctOfOut: pct(totalErrBytes, t.outBytes),
    totalErrInBytes, errInBytesPctOfIn: pct(totalErrInBytes, t.inBytes),
    byTool: byToolRows,
    byWeek,
  };
};

const result = {
  meta: {
    mainJson: MAIN_JSON, backupJson: BACKUP_JSON,
    mainWindow: main.sample.window, backupWindow: backup.sample.window,
    mainGeneratedAt: main.meta.generated_at, backupGeneratedAt: backup.meta.generated_at,
  },
  main: {
    totals: main.totals,
    ranks: toolRanks(main.byTool, main.totals.calls, main.totals.outBytes, main.totals.inBytes),
    inOut: inOutDim(main.byTool, main.totals.inBytes, main.totals.outBytes, main.weekly),
    errors: errDim(main, mainErr),
    waste: wasteDim(main),
    concentration: concentration(main),
    cross: crossWindowDim(main),
  },
  backup: {
    totals: backup.totals,
    ranks: toolRanks(backup.byTool, backup.totals.calls, backup.totals.outBytes, backup.totals.inBytes),
    inOut: inOutDim(backup.byTool, backup.totals.inBytes, backup.totals.outBytes, backup.weekly),
    errors: errDim(backup, backupErr),
    waste: wasteDim(backup),
    concentration: concentration(backup),
    cross: crossWindowDim(backup),
  },
  compare: {
    // 稳定指纹候选: [main, backup, relDiff(main→backup)]
    msgsPerThread: [main.sample.totalMainMessages / main.sample.totalThreads, backup.sample.totalMainMessages / backup.sample.totalThreads],
    callsPerThread: [main.totals.calls / main.sample.totalThreads, backup.totals.calls / backup.sample.totalThreads],
    callsPerActive: [main.totals.calls / main.sessions.withCalls, backup.totals.calls / backup.sessions.withCalls],
    inOutRatio: [main.totals.inBytes / main.totals.outBytes, backup.totals.inBytes / backup.totals.outBytes],
    inPctOfTotal: [pct(main.totals.inBytes, main.totals.totalBytes), pct(backup.totals.inBytes, backup.totals.totalBytes)],
    errRate: [pct(main.totals.errors, main.totals.calls), pct(backup.totals.errors, backup.totals.calls)],
    errBytesPctOfOut: [0, 0],
    rereadOfReadCalls: [main.derived.wastePct.rereadOfReadCalls, backup.derived.wastePct.rereadOfReadCalls],
    rereadBytesOfReadBytes: [main.derived.wastePct.rereadBytesOfReadBytes, backup.derived.wastePct.rereadBytesOfReadBytes],
    whiteOfSearchCalls: [main.derived.wastePct.whiteSearchOfSearchCalls, backup.derived.wastePct.whiteSearchOfSearchCalls],
    dupOfSearchCalls: [main.derived.wastePct.dupSearchOfSearchCalls, backup.derived.wastePct.dupSearchOfSearchCalls],
    wasteUnionPct: [pct(main.waste.unionBytes, main.totals.outBytes), pct(backup.waste.unionBytes, backup.totals.outBytes)],
    toolTop1OutPct: [main.top1OutPct, backup.top1OutPct],
    toolTop3OutPct: [main.top3OutPct, backup.top3OutPct],
    sessionTop10Pct: [pct(main.sessions.top10.reduce((s: number, x: any) => s + x.total, 0), main.totals.totalBytes), pct(backup.sessions.top10.reduce((s: number, x: any) => s + x.total, 0), backup.totals.totalBytes)],
    sessionP50: [main.sessions.p50, backup.sessions.p50],
    sessionP95: [main.sessions.p95, backup.sessions.p95],
    readLikeOutPct: [0, 0],
    writeInPct: [0, 0],
    giantOfAllCalls: [pct(main.waste.giantCount ?? 0, main.totals.calls), pct(backup.waste.giantCount ?? 0, backup.totals.calls)],
  },
};

// 修正 compare 中需要后算的项
{
  const m = result.main, b = result.backup;
  const p = result.compare as any;
  const errBytesOfOut = (j: any, err: any) => pct(Object.values(err.byTool).reduce((s: number, v: any) => s + v.errBytes, 0), j.totals.outBytes);
  p.errBytesPctOfOut = [errBytesOfOut(main, mainErr), errBytesOfOut(backup, backupErr)];
  const grpOut = (j: any, names: string[]) => pct(j.byTool.filter((x: any) => names.includes(x.name)).reduce((s: number, x: any) => s + x.outBytes, 0), j.totals.outBytes);
  const grpIn = (j: any, names: string[]) => pct(j.byTool.filter((x: any) => names.includes(x.name)).reduce((s: number, x: any) => s + x.inBytes, 0), j.totals.inBytes);
  const readLike = ["Read", "Grep", "Glob", "WebFetch", "WebSearch", "SkillTool", "Skill", "folder_operations", "AgentResult", "SearchExtraTools"];
  const writeLike = ["Edit", "Write", "LineEdit", "HashlineEdit", "TodoWrite", "ExecuteExtraTool", "AskUserQuestion", "Agent"];
  p.readLikeOutPct = [grpOut(main, readLike), grpOut(backup, readLike)];
  p.writeLikeInPct = [grpIn(main, writeLike), grpIn(backup, writeLike)];
  p.writeInPct = [
    pct((main.byTool.find((x: any) => x.name === "Write")?.inBytes ?? 0) + (main.byTool.find((x: any) => x.name === "Edit")?.inBytes ?? 0), main.totals.inBytes),
    pct((backup.byTool.find((x: any) => x.name === "Write")?.inBytes ?? 0) + (backup.byTool.find((x: any) => x.name === "Edit")?.inBytes ?? 0), backup.totals.inBytes),
  ];
  p.giantOfAllCalls = [pct(main.giantCount ?? 0, main.totals.calls), pct(backup.giantCount ?? 0, backup.totals.calls)];
}

writeFileSync(OUT, JSON.stringify(result, null, 2));
console.log("written:", OUT);
console.log("main window:", main.sample.window);
console.log("backup window:", backup.sample.window);
