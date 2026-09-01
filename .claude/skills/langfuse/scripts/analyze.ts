#!/usr/bin/env bun
/**
 * Langfuse trace 综合分析脚本
 *
 * 用法:
 *   bun .claude/skills/langfuse/scripts/analyze.ts [数量]           # 最近 N 条 trace 综合报告
 *   bun .claude/skills/langfuse/scripts/analyze.ts --trace-id <id> # 单条 trace 详细报告
 *   bun .claude/skills/langfuse/scripts/analyze.ts --tools [数量]   # 工具调用专项分析
 *   bun .claude/skills/langfuse/scripts/analyze.ts --growth [数量]  # 上下文涨幅分析
 *   bun .claude/skills/langfuse/scripts/analyze.ts --report [数量]  # 完整分析报告(全部维度)
 *
 * 过滤（适用于除 --trace-id 以外的模式）:
 *   --from <ISO> --to <ISO> --days <N>
 *   --tag <tag> --user <id> --session <id> --name <str>
 */

import {
  api, clampTraceLimit, fetchObservations, fetchTracesFiltered,
  parseFilterArgs, summarizeLatency, summarizeObservationLatency, fmtLatency,
  fmt, pct, bar, genTokens, totalInputTraffic, LatencySummary,
} from "./lib.ts";

// Backward-compat aliases for existing section code
const FMT = fmt, PCT = pct, BAR = bar;

function traceLabel(trace: Pick<TraceAnalysis, "id">) {
  return trace.id.slice(0, 12);
}

async function fetchAllObservations(traces: any[]) {
  const map = new Map<string, any[]>();
  for (let i = 0; i < traces.length; i += 5) {
    const batch = traces.slice(i, i + 5);
    const results = await Promise.all(batch.map((t: any) => fetchObservations(t.id)));
    for (let j = 0; j < batch.length; j++) {
      map.set(batch[j].id, results[j]);
    }
  }
  return map;
}

// ═══════════════════════════════════════════════════════════════
// Core analysis types
// ═══════════════════════════════════════════════════════════════

interface GenDetail {
  model: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  latency: number | null;
}

interface ToolDetail {
  name: string;
  latency: number | null;
  status: string;
  parentGenIdx: number;
}

interface TraceAnalysis {
  id: string;
  timestamp: string;
  sessionId: string;
  latency: LatencySummary;
  llmCalls: number;
  toolCalls: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
  totalCacheCreation: number;
  cachePct: number;
  effective: number;
  genDetails: GenDetail[];
  toolDetails: ToolDetail[];
  observations: any[];
}

// ═══════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════
// Analysis functions
// ═══════════════════════════════════════════════════════════════

function analyzeTrace(trace: any, observations: any[]): TraceAnalysis {
  const gens = observations.filter((o) => o.type === "GENERATION");
  const tools = observations.filter((o) => o.type === "TOOL");

  let totalInput = 0, totalOutput = 0, totalCache = 0, totalCacheCreation = 0;

  const genDetails: GenDetail[] = gens.map((g) => {
    const tokens = genTokens(g);
    totalInput += tokens.input;
    totalOutput += tokens.output;
    totalCache += tokens.cacheRead;
    totalCacheCreation += tokens.cacheCreate;
    return {
      model: g.providedModelName || g.internalModelId || g.model
        || (g.metadata?.attributes?.["langfuse.observation.model.name"])
        || "?",
      input: tokens.input,
      output: tokens.output,
      cacheRead: tokens.cacheRead,
      cacheCreation: tokens.cacheCreate,
      latency: summarizeObservationLatency(g),
    };
  });

  const genIds = gens.map((g) => g.id);
  const toolDetails: ToolDetail[] = tools.map((t) => {
    const parentIdx = genIds.reduce((best, gid, idx) => {
      if (t.startTime >= gens[idx].startTime) return idx;
      return best;
    }, -1);
    return {
      name: t.name || t.metadata?.toolName || "unknown",
      latency: summarizeObservationLatency(t),
      status: t.status || "success",
      parentGenIdx: parentIdx,
    };
  });

  return {
    id: trace.id,
    timestamp: trace.timestamp || trace.createdAt || "",
    sessionId: trace.sessionId || "",
    latency: summarizeLatency(observations),
    llmCalls: gens.length,
    toolCalls: tools.length,
    totalInput,
    totalOutput,
    totalCache,
    totalCacheCreation,
    cachePct: totalInputTraffic({ input: totalInput, cacheRead: totalCache, cacheCreate: totalCacheCreation }) > 0
      ? (totalCache / totalInputTraffic({ input: totalInput, cacheRead: totalCache, cacheCreate: totalCacheCreation })) * 100
      : 0,
    effective: totalInput,
    genDetails,
    toolDetails,
    observations,
  };
}

// ═══════════════════════════════════════════════════════════════
// Report sections
// ═══════════════════════════════════════════════════════════════

function sectionOverview(traces: TraceAnalysis[]) {
  console.log("## 1. Overview\n");
  let aggIn = 0, aggOut = 0, aggCache = 0, aggCacheCreation = 0, aggLLM = 0, aggTool = 0;
  for (const t of traces) {
    aggIn += t.totalInput; aggOut += t.totalOutput; aggCache += t.totalCache; aggCacheCreation += t.totalCacheCreation;
    aggLLM += t.llmCalls; aggTool += t.toolCalls;
  }
  console.log(`  Traces:         ${traces.length}`);
  console.log(`  LLM calls:      ${aggLLM}`);
  console.log(`  Tool calls:     ${aggTool}`);
  console.log(`  Total input:    ${FMT(aggIn)} tokens`);
  console.log(`  Total output:   ${FMT(aggOut)} tokens`);
  console.log(`  Cache read:     ${FMT(aggCache)} tokens (${PCT(aggCache, totalInputTraffic({ input: aggIn, cacheRead: aggCache, cacheCreate: aggCacheCreation }))} of input traffic)`);
  console.log(`  Cache creation: ${FMT(aggCacheCreation)} tokens`);
  console.log(`  Effective new:  ${FMT(aggIn)} tokens`);
  console.log(`  Output/Input:   ${PCT(aggOut, aggIn)}`);
  console.log(`  Avg LLM/trace:  ${(aggLLM / traces.length).toFixed(1)}`);
  console.log(`  Avg Tool/trace: ${(aggTool / traces.length).toFixed(1)}`);
}

function sectionTraceTable(traces: TraceAnalysis[]) {
  console.log("\n## 2. Per-Trace Breakdown\n");
  console.log("| # | Trace | LLM | Tool | Input tok | Out tok | Cache% | Eff.new | Latency |");
  console.log("|--:|:------|----:|-----:|----------:|--------:|-------:|--------:|--------:|");
  for (let i = 0; i < traces.length; i++) {
    const t = traces[i];
    const label = traceLabel(t);
    console.log(
      `| ${i + 1} | ${label} | ${t.llmCalls} | ${t.toolCalls} | ${FMT(t.totalInput)} | ${FMT(t.totalOutput)} | ${t.cachePct.toFixed(1)}% | ${FMT(t.effective)} | ${fmtLatency(t.latency)} |`
    );
  }
}

function sectionToolAnalysis(traces: TraceAnalysis[]) {
  console.log("\n## 3. Tool Call Analysis\n");

  const toolMap = new Map<string, { count: number; totalLatency: number; latencySamples: number; errors: number }>();
  for (const t of traces) {
    for (const tool of t.toolDetails) {
      const existing = toolMap.get(tool.name) || { count: 0, totalLatency: 0, latencySamples: 0, errors: 0 };
      existing.count++;
      if (tool.latency !== null) {
        existing.totalLatency += tool.latency;
        existing.latencySamples++;
      }
      if (tool.status !== "success") existing.errors++;
      toolMap.set(tool.name, existing);
    }
  }

  const tools = [...toolMap.entries()]
    .map(([name, stats]) => ({ name, ...stats, avgLatency: stats.latencySamples ? stats.totalLatency / stats.latencySamples : null }))
    .sort((a, b) => b.count - a.count);
  const totalCalls = tools.reduce((s, t) => s + t.count, 0);

  console.log("### 3.1 Tool Frequency\n");
  console.log("| Tool | Calls | % of Total | Avg Latency | Errors |");
  console.log("|------|------:|-----------:|------------:|-------:|");
  for (const t of tools) {
    console.log(
      `| ${t.name} | ${t.count} | ${PCT(t.count, totalCalls)} | ${t.avgLatency === null ? "N/A" : `${t.avgLatency.toFixed(2)}s`} | ${t.errors} |`
    );
  }

  console.log("\n### 3.2 Tool \u2192 Context Growth\n");
  for (const t of traces) {
    if (t.genDetails.length < 2) continue;
    console.log(`**Trace**: ${traceLabel(t)}\n`);
    console.log("| Step | Gen Input | Delta from prev | Tools between | Tool names |");
    console.log("|-----:|----------:|----------------:|--------------:|------------|");
    for (let i = 0; i < t.genDetails.length; i++) {
      const gen = t.genDetails[i];
      const delta = i > 0 ? gen.input - t.genDetails[i - 1].input : gen.input;
      const betweenTools = t.toolDetails.filter((td) => td.parentGenIdx === i - 1);
      const toolNames = betweenTools.map((td) => td.name).join(", ") || "-";
      const deltaStr = delta >= 0 ? `+${FMT(delta)}` : FMT(delta);
      console.log(
        `| ${i + 1} | ${FMT(gen.input)} | ${deltaStr} | ${betweenTools.length} | ${toolNames} |`
      );
    }
    console.log();
    break;
  }

  console.log("### 3.3 Potential Redundancy\n");
  for (const t of traces) {
    const names = t.toolDetails.map((td) => td.name);
    const seen = new Map<string, number>();
    for (const n of names) seen.set(n, (seen.get(n) || 0) + 1);
    const dupes = [...seen.entries()].filter(([, c]) => c > 1);
    if (dupes.length > 0) {
      console.log(`  ${traceLabel(t)}: ${dupes.map(([n, c]) => `${n}×${c}`).join(", ")}`);
    }
  }
}

function sectionContextGrowth(traces: TraceAnalysis[]) {
  console.log("\n## 4. Context Growth Trend\n");

  console.log("### 4.1 Per-Trace Input Token Growth\n");
  for (const t of traces) {
    if (t.genDetails.length < 2) continue;
    const g = t.genDetails;
    const maxTraffic = Math.max(...g.map((tokens) => totalInputTraffic({ input: tokens.input, cacheRead: tokens.cacheRead, cacheCreate: tokens.cacheCreation })));
    const firstInput = g[0].input;
    const lastInput = g[g.length - 1].input;
    const growth = lastInput - firstInput;
    const growthPct = firstInput > 0 ? ((growth / firstInput) * 100).toFixed(1) : "0";

    console.log(`**${traceLabel(t)}** (${g.length} LLM calls)`);
    console.log(`  Start: ${FMT(firstInput)} \u2192 End: ${FMT(lastInput)} (growth: ${growthPct}%)\n`);

    for (let i = 0; i < g.length; i++) {
      const barWidth = 30;
      const tokens = g[i];
      const rawWidth = maxTraffic > 0 ? (tokens.input / maxTraffic) * barWidth : 0;
      const cacheReadWidth = maxTraffic > 0 ? (tokens.cacheRead / maxTraffic) * barWidth : 0;
      const cacheCreateWidth = maxTraffic > 0 ? (tokens.cacheCreation / maxTraffic) * barWidth : 0;
      const rawCells = Math.round(rawWidth);
      const cacheReadCells = Math.min(Math.round(cacheReadWidth), barWidth - rawCells);
      const cacheCreateCells = Math.min(Math.round(cacheCreateWidth), barWidth - rawCells - cacheReadCells);
      const usedWidth = rawCells + cacheReadCells + cacheCreateCells;
      const bar = "█".repeat(rawCells) + "░".repeat(cacheReadCells) + "▒".repeat(cacheCreateCells) + " ".repeat(barWidth - usedWidth);
      console.log(`  ${String(i + 1).padStart(2)} |${bar}| raw=${FMT(tokens.input)} cache-read=${FMT(tokens.cacheRead)} cache-create=${FMT(tokens.cacheCreation)}`);
    }
    console.log(`  Legend: █=raw input ░=cache read ▒=cache creation\n`);
  }

  console.log("### 4.2 Session Accumulation\n");
  const sessionMap = new Map<string, { traces: number; totalInput: number; totalOutput: number }>();
  for (const t of traces) {
    const sid = t.sessionId || "unknown";
    const entry = sessionMap.get(sid) || { traces: 0, totalInput: 0, totalOutput: 0 };
    entry.traces++;
    entry.totalInput += t.totalInput;
    entry.totalOutput += t.totalOutput;
    sessionMap.set(sid, entry);
  }
  console.log("| Session | Traces | Total Input | Total Output | Avg Input/trace |");
  console.log("|---------|-------:|------------:|-------------:|----------------:|");
  for (const [sid, s] of sessionMap) {
    const label = sid.length > 20 ? sid.slice(0, 8) + "..." + sid.slice(-6) : sid;
    const avg = s.traces > 0 ? Math.round(s.totalInput / s.traces) : 0;
    console.log(`| ${label} | ${s.traces} | ${FMT(s.totalInput)} | ${FMT(s.totalOutput)} | ${FMT(avg)} |`);
  }

  console.log("\n### 4.3 Cross-Trace Growth Rate\n");
  const sorted = [...traces];
  if (sorted.length >= 2) {
    console.log("| From \u2192 To | Input delta | Growth rate |");
    console.log("|-----------|------------:|------------:|");
    for (let i = 1; i < sorted.length; i++) {
      const prev = sorted[i - 1];
      const curr = sorted[i];
      const delta = curr.totalInput - prev.totalInput;
      const rate = prev.totalInput > 0 ? ((delta / prev.totalInput) * 100).toFixed(1) : "N/A";
      const prevLabel = prev.input.slice(0, 15).replace(/\|/g, "");
      const currLabel = curr.input.slice(0, 15).replace(/\|/g, "");
      const sign = delta >= 0 ? "+" : "";
      console.log(`| ${prevLabel} \u2192 ${currLabel} | ${sign}${FMT(delta)} | ${rate}% |`);
    }
  }
}

function sectionSystemPrompt(_traces: TraceAnalysis[]) {
  console.log("\n## 5. System Prompt Occupancy\n");
  console.log("为避免输出或派生 prompt 内容，此脚本不分析 prompt 组成；请使用仅输出计数的 trace-messages.ts。\n");
}

function sectionExpensiveTrace(traces: TraceAnalysis[]) {
  const expensive = traces.reduce((a, b) => (a.totalInput > b.totalInput ? a : b), traces[0]);
  console.log(`\n## 6. Most Expensive Trace Detail\n`);
  console.log(`Trace: ${traceLabel(expensive)}`);
  console.log(`Latency: ${fmtLatency(expensive.latency)}\n`);
  console.log("| # | Model | Input | Output | Cache Read | Delta | Latency |");
  console.log("|--:|-------|------:|-------:|-----------:|------:|--------:|");
  for (let i = 0; i < expensive.genDetails.length; i++) {
    const g = expensive.genDetails[i];
    const delta = i > 0 ? g.input - expensive.genDetails[i - 1].input : g.input;
    const sign = delta >= 0 ? "+" : "";
    console.log(
      `| ${i + 1} | ${g.model} | ${FMT(g.input)} | ${FMT(g.output)} | ${FMT(g.cacheRead)} | ${sign}${FMT(delta)} | ${g.latency === null ? "N/A" : `${g.latency.toFixed(1)}s`} |`
    );
  }
}

function sectionSummary(traces: TraceAnalysis[]) {
  console.log("\n## 7. Summary & Flags\n");
  let aggIn = 0, aggOut = 0, aggCache = 0, aggCacheCreation = 0;
  const flags: string[] = [];

  for (const t of traces) {
    aggIn += t.totalInput; aggOut += t.totalOutput; aggCache += t.totalCache; aggCacheCreation += t.totalCacheCreation;

    if (t.cachePct < 90 && t.llmCalls > 1)
      flags.push(`低缓存（${t.cachePct.toFixed(0)}%）：${traceLabel(t)}`);
    if (t.effective > 20000)
      flags.push(`有效新增 token 偏高（${FMT(t.effective)}）：${traceLabel(t)}`);
    if (t.llmCalls > 10)
      flags.push(`LLM 调用较多（${t.llmCalls}）：${traceLabel(t)}`);
    const slowGen = t.genDetails.find((g) => g.latency !== null && g.latency > 60);
    if (slowGen)
      flags.push(`LLM 调用较慢（${slowGen.latency!.toFixed(0)}s）：${traceLabel(t)}`);

    const toolCounts = new Map<string, number>();
    for (const td of t.toolDetails) toolCounts.set(td.name, (toolCounts.get(td.name) || 0) + 1);
    for (const [name, count] of [...toolCounts.entries()].filter(([, c]) => c > 2)) {
      flags.push(`重复工具：${name} 调用 ${count} 次（${traceLabel(t)}）`);
    }
  }

  if (flags.length === 0) {
    console.log("  No issues detected. All metrics look healthy.");
  } else {
    for (const f of flags) console.log(`  ${f}`);
  }

  const inputTraffic = totalInputTraffic({ input: aggIn, cacheRead: aggCache, cacheCreate: aggCacheCreation });
  console.log(`\n  Cache read share: ${PCT(aggCache, inputTraffic)} of input traffic`);
  console.log(`  Output/Input:     ${PCT(aggOut, aggIn)}`);
  console.log(`  Avg eff./trace:   ${FMT(Math.round(aggIn / traces.length))} tokens`);
}

// ═══════════════════════════════════════════════════════════════
// Mode dispatch
// ═══════════════════════════════════════════════════════════════

type Mode = "overview" | "tools" | "growth" | "report";

async function run(mode: Mode, limit: number, singleTraceId?: string, filters?: ReturnType<typeof parseFilterArgs>) {
  let traces: TraceAnalysis[];

  if (singleTraceId) {
    const [trace, obs] = await Promise.all([
      api(`/api/public/traces/${singleTraceId}`),
      fetchObservations(singleTraceId),
    ]);
    traces = [analyzeTrace(trace, obs)];
  } else {
    const { traces: raw } = await fetchTracesFiltered({
      limit,
      fromTimestamp: filters?.time.from,
      toTimestamp: filters?.time.to,
      tags: filters?.tag ? [filters.tag] : undefined,
      userId: filters?.userId,
      sessionId: filters?.sessionId,
      name: filters?.name,
    });
    if (raw.length === 0) {
      console.log("No traces found.");
      process.exit(0);
    }
    console.log(`Analyzing ${raw.length} traces...\n`);
    const obsMap = await fetchAllObservations(raw);
    traces = raw.map((t: any) => analyzeTrace(t, obsMap.get(t.id) || []));
  }

  const sorted = [...traces].sort((a, b) => a.timestamp.localeCompare(b.timestamp));

  switch (mode) {
    case "overview":
      sectionOverview(sorted);
      sectionTraceTable(sorted);
      sectionSummary(sorted);
      break;
    case "tools":
      sectionToolAnalysis(sorted);
      break;
    case "growth":
      sectionContextGrowth(sorted);
      break;
    case "report":
      sectionOverview(sorted);
      sectionTraceTable(sorted);
      sectionToolAnalysis(sorted);
      sectionContextGrowth(sorted);
      sectionSystemPrompt(sorted);
      sectionExpensiveTrace(sorted);
      sectionSummary(sorted);
      break;
  }
}

// ═══════════════════════════════════════════════════════════════
// CLI
// ═══════════════════════════════════════════════════════════════

async function main() {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log(`用法: bun analyze.ts [数量] [--tools|--growth|--report] [--trace-id <id>] [过滤选项]

过滤选项: --from <ISO> --to <ISO> --days <N> --tag <tag> --user <id> --session <id> --name <str> --limit <N>`);
    return;
  }
  let limit = 10;
  let mode: Mode = "overview";
  let singleTraceId = "";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--trace-id": singleTraceId = args[++i]; break;
      case "--tools": mode = "tools"; break;
      case "--growth": mode = "growth"; break;
      case "--report": mode = "report"; break;
      case "--limit": {
        const n = parseInt(args[++i]);
        if (!isNaN(n) && n > 0) limit = clampTraceLimit(n);
        break;
      }
      case "--days": case "--from": case "--to":
      case "--tag": case "--user": case "--session":
      case "--name": case "--model":
        i++; // 跳过选项值，避免 --days 7 之类的数字被误当作 limit
        break;
      default: {
        const n = parseInt(args[i]);
        if (!isNaN(n) && n > 0) limit = clampTraceLimit(n);
      }
    }
  }

  const filters = singleTraceId ? undefined : parseFilterArgs(args);
  if (filters?.time.from) {
    console.error(`Time range: ${filters.time.from} → ${filters.time.to || "now"}`);
  }
  await run(mode, limit, singleTraceId || undefined, filters);
}

main().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
