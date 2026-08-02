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
  api, fetchObservations, fetchTracesFiltered,
  parseFilterArgs, parseTimeWindow,
  summarizeTokens, genTokens, estimateCost, fmtCost,
  detectAnomalies, fmt, pct, ms, isoToLocal, bar,
} from "./lib.ts";

// Backward-compat aliases for existing section code
const FMT = fmt, PCT = pct, BAR = bar;

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
  latency: number;
}

interface ToolDetail {
  name: string;
  traceInput: string;
  latency: number;
  status: string;
  parentGenIdx: number;
}

interface TraceAnalysis {
  id: string;
  timestamp: string;
  input: string;
  output: string;
  sessionId: string;
  latency: number;
  llmCalls: number;
  toolCalls: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
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

  let totalInput = 0, totalOutput = 0, totalCache = 0;

  const genDetails: GenDetail[] = gens.map((g) => {
    const u = g.usageDetails || g.usage || {};
    const inputTokens: number = u.input || u.prompt_tokens || 0;
    const outputTokens: number = u.output || u.completion_tokens || 0;
    const cacheRead: number = u.cache_read_input_tokens || u.cache_read || 0;
    const cacheCreation: number = u.cache_creation_input_tokens || 0;
    totalInput += inputTokens;
    totalOutput += outputTokens;
    totalCache += cacheRead;
    return {
      model: g.providedModelName || g.internalModelId || g.model
        || (g.metadata?.attributes?.["langfuse.observation.model.name"])
        || "?",
      input: inputTokens,
      output: outputTokens,
      cacheRead,
      cacheCreation,
      latency: g.latency || 0,
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
      traceInput: (trace.input as string)?.slice(0, 40) || "",
      latency: t.latency || 0,
      status: t.status || "success",
      parentGenIdx: parentIdx,
    };
  });

  return {
    id: trace.id,
    timestamp: trace.timestamp || trace.createdAt || "",
    input: (trace.input as string)?.slice(0, 80) || "",
    output: (trace.output as string)?.slice(0, 80) || "",
    sessionId: trace.sessionId || "",
    latency: trace.latency || 0,
    llmCalls: gens.length,
    toolCalls: tools.length,
    totalInput,
    totalOutput,
    totalCache,
    cachePct: totalInput > 0 ? (totalCache / totalInput) * 100 : 0,
    effective: totalInput - totalCache,
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
  let aggIn = 0, aggOut = 0, aggCache = 0, aggLLM = 0, aggTool = 0;
  for (const t of traces) {
    aggIn += t.totalInput; aggOut += t.totalOutput; aggCache += t.totalCache;
    aggLLM += t.llmCalls; aggTool += t.toolCalls;
  }
  console.log(`  Traces:         ${traces.length}`);
  console.log(`  LLM calls:      ${aggLLM}`);
  console.log(`  Tool calls:     ${aggTool}`);
  console.log(`  Total input:    ${FMT(aggIn)} tokens`);
  console.log(`  Total output:   ${FMT(aggOut)} tokens`);
  console.log(`  Cache read:     ${FMT(aggCache)} tokens (${PCT(aggCache, aggIn)}%)`);
  console.log(`  Effective new:  ${FMT(aggIn - aggCache)} tokens`);
  console.log(`  Output/Input:   ${PCT(aggOut, aggIn)}%`);
  console.log(`  Avg LLM/trace:  ${(aggLLM / traces.length).toFixed(1)}`);
  console.log(`  Avg Tool/trace: ${(aggTool / traces.length).toFixed(1)}`);
}

function sectionTraceTable(traces: TraceAnalysis[]) {
  console.log("\n## 2. Per-Trace Breakdown\n");
  console.log("| # | Input (40 chars) | LLM | Tool | Input tok | Out tok | Cache% | Eff.new | Latency |");
  console.log("|--:|:-----------------|----:|-----:|----------:|--------:|-------:|--------:|--------:|");
  for (let i = 0; i < traces.length; i++) {
    const t = traces[i];
    const label = t.input.slice(0, 40).replace(/\|/g, "\\|");
    console.log(
      `| ${i + 1} | ${label} | ${t.llmCalls} | ${t.toolCalls} | ${FMT(t.totalInput)} | ${FMT(t.totalOutput)} | ${t.cachePct.toFixed(1)}% | ${FMT(t.effective)} | ${t.latency.toFixed(1)}s |`
    );
  }
}

function sectionToolAnalysis(traces: TraceAnalysis[]) {
  console.log("\n## 3. Tool Call Analysis\n");

  const toolMap = new Map<string, { count: number; totalLatency: number; errors: number }>();
  for (const t of traces) {
    for (const tool of t.toolDetails) {
      const existing = toolMap.get(tool.name) || { count: 0, totalLatency: 0, errors: 0 };
      existing.count++;
      existing.totalLatency += tool.latency;
      if (tool.status !== "success") existing.errors++;
      toolMap.set(tool.name, existing);
    }
  }

  const tools = [...toolMap.entries()]
    .map(([name, stats]) => ({ name, ...stats, avgLatency: stats.totalLatency / stats.count }))
    .sort((a, b) => b.count - a.count);
  const totalCalls = tools.reduce((s, t) => s + t.count, 0);

  console.log("### 3.1 Tool Frequency\n");
  console.log("| Tool | Calls | % of Total | Avg Latency | Errors |");
  console.log("|------|------:|-----------:|------------:|-------:|");
  for (const t of tools) {
    console.log(
      `| ${t.name} | ${t.count} | ${PCT(t.count, totalCalls)}% | ${t.avgLatency.toFixed(2)}s | ${t.errors} |`
    );
  }

  console.log("\n### 3.2 Tool \u2192 Context Growth\n");
  for (const t of traces) {
    if (t.genDetails.length < 2) continue;
    console.log(`**Trace**: "${t.input.slice(0, 50)}"\n`);
    console.log("| Step | Gen Input | Delta from prev | Tools between | Tool names |");
    console.log("|-----:|----------:|----------------:|--------------:|------------|");
    for (let i = 0; i < t.genDetails.length; i++) {
      const gen = t.genDetails[i];
      const delta = i > 0 ? gen.input - t.genDetails[i - 1].input : gen.input - gen.cacheRead;
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
      console.log(`  "${t.input.slice(0, 40)}...": ${dupes.map(([n, c]) => `${n}\u00d7${c}`).join(", ")}`);
    }
  }
}

function sectionContextGrowth(traces: TraceAnalysis[]) {
  console.log("\n## 4. Context Growth Trend\n");

  console.log("### 4.1 Per-Trace Input Token Growth\n");
  for (const t of traces) {
    if (t.genDetails.length < 2) continue;
    const g = t.genDetails;
    const maxInput = Math.max(...g.map((x) => x.input));
    const firstInput = g[0].input;
    const lastInput = g[g.length - 1].input;
    const growth = lastInput - firstInput;
    const growthPct = firstInput > 0 ? ((growth / firstInput) * 100).toFixed(1) : "0";

    console.log(`**"${t.input.slice(0, 50)}"** (${g.length} LLM calls)`);
    console.log(`  Start: ${FMT(firstInput)} \u2192 End: ${FMT(lastInput)} (growth: ${growthPct}%)\n`);

    for (let i = 0; i < g.length; i++) {
      const barWidth = 30;
      const pct = maxInput > 0 ? (g[i].input / maxInput) * barWidth : 0;
      const cacheWidth = maxInput > 0 ? (g[i].cacheRead / maxInput) * barWidth : 0;
      const newWidth = pct - cacheWidth;
      const bar = "\u2591".repeat(Math.round(cacheWidth)) + "\u2588".repeat(Math.round(newWidth)) + "\u2591".repeat(Math.max(0, barWidth - Math.round(pct)));
      console.log(`  ${String(i + 1).padStart(2)} |${bar}| ${FMT(g[i].input)} (new: ${FMT(g[i].input - g[i].cacheRead)})`);
    }
    console.log(`  Legend: \u2591=cached \u2588=new tokens\n`);
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

function sectionSystemPrompt(traces: TraceAnalysis[]) {
  console.log("\n## 5. System Prompt Occupancy\n");

  for (const t of traces) {
    const gen = t.observations.find((o) => o.type === "GENERATION");
    if (!gen) continue;
    const raw = gen.input;
    let messages: any[] = [];
    if (typeof raw === "string") {
      try { messages = JSON.parse(raw).messages || []; } catch { break; }
    } else if (raw && typeof raw === "object") {
      messages = (raw as any).messages || [];
    }
    if (!messages.length) break;

    const sysParts: { label: string; chars: number }[] = [];
    let userChars = 0, assistantChars = 0, toolChars = 0;

    for (const m of messages) {
      const role = m.role || "";
      const content = m.content || "";
      const chars = typeof content === "string"
        ? content.length
        : JSON.stringify(content).length;

      if (role === "system") {
        let label = "system";
        if (typeof content === "string") {
          const fl = content.split("\n")[0];
          if (fl.includes("CLAUDE.md")) label = "CLAUDE.md";
          else if (fl.includes("Deferred Tools") || fl.includes("ExtraTools")) label = "Deferred Tools";
          else if (fl.includes("SubAgent") || fl.includes("Subagents")) label = "SubAgent Defs";
          else if (fl.toLowerCase().includes("skill")) label = "Skills Summary";
          else if (fl.length > 40) label = fl.slice(0, 35) + "...";
          else label = fl;
        }
        sysParts.push({ label, chars });
      } else if (role === "user") userChars += chars;
      else if (role === "assistant") assistantChars += chars;
      else if (role === "tool") toolChars += chars;
    }

    const totalChars = sysParts.reduce((s, p) => s + p.chars, 0) + userChars + assistantChars + toolChars;
    const sysTotal = sysParts.reduce((s, p) => s + p.chars, 0);

    console.log("### 5.1 Section Breakdown\n");
    console.log("| Section | Chars | Est. Tokens | % of Context | Bar |");
    console.log("|---------|------:|------------:|-------------:|-----|");
    for (const p of sysParts) {
      const pct = totalChars > 0 ? (p.chars / totalChars) * 100 : 0;
      console.log(`| ${p.label} | ${FMT(p.chars)} | ~${FMT(Math.round(p.chars / 3.5))} | ${pct.toFixed(1)}% | ${BAR(pct, 15)} |`);
    }
    const sysPct = totalChars > 0 ? (sysTotal / totalChars) * 100 : 0;
    console.log(`| **System total** | **${FMT(sysTotal)}** | **~${FMT(Math.round(sysTotal / 3.5))}** | **${sysPct.toFixed(1)}%** | ${BAR(sysPct, 15)} |`);

    const userPct = totalChars > 0 ? (userChars / totalChars) * 100 : 0;
    const asstPct = totalChars > 0 ? (assistantChars / totalChars) * 100 : 0;
    console.log(`| User messages | ${FMT(userChars)} | ~${FMT(Math.round(userChars / 3.5))} | ${userPct.toFixed(1)}% | ${BAR(userPct, 15)} |`);
    console.log(`| Assistant messages | ${FMT(assistantChars)} | ~${FMT(Math.round(assistantChars / 3.5))} | ${asstPct.toFixed(1)}% | ${BAR(asstPct, 15)} |`);
    if (toolChars > 0) {
      const toolPct = totalChars > 0 ? (toolChars / totalChars) * 100 : 0;
      console.log(`| Tool results | ${FMT(toolChars)} | ~${FMT(Math.round(toolChars / 3.5))} | ${toolPct.toFixed(1)}% | ${BAR(toolPct, 15)} |`);
    }
    console.log(`| **Total** | **${FMT(totalChars)}** | **~${FMT(Math.round(totalChars / 3.5))}** | **100%** | |`);

    console.log("\n### 5.2 System vs Conversation Ratio\n");
    const convChars = userChars + assistantChars + toolChars;
    const convPct = totalChars > 0 ? (convChars / totalChars) * 100 : 0;
    console.log(`  System:       ${BAR(sysPct, 30)} ${sysPct.toFixed(1)}%`);
    console.log(`  Conversation: ${BAR(convPct, 30)} ${convPct.toFixed(1)}%`);
    break;
  }
}

function sectionExpensiveTrace(traces: TraceAnalysis[]) {
  const expensive = traces.reduce((a, b) => (a.totalInput > b.totalInput ? a : b), traces[0]);
  console.log(`\n## 6. Most Expensive Trace Detail\n`);
  console.log(`Input: "${expensive.input}"`);
  console.log(`Latency: ${expensive.latency}s\n`);
  console.log("| # | Model | Input | Output | Cache Read | Delta | Latency |");
  console.log("|--:|-------|------:|-------:|-----------:|------:|--------:|");
  for (let i = 0; i < expensive.genDetails.length; i++) {
    const g = expensive.genDetails[i];
    const delta = i > 0 ? g.input - expensive.genDetails[i - 1].input : g.input;
    const sign = delta >= 0 ? "+" : "";
    console.log(
      `| ${i + 1} | ${g.model} | ${FMT(g.input)} | ${FMT(g.output)} | ${FMT(g.cacheRead)} | ${sign}${FMT(delta)} | ${g.latency.toFixed(1)}s |`
    );
  }
}

function sectionSummary(traces: TraceAnalysis[]) {
  console.log("\n## 7. Summary & Flags\n");
  let aggIn = 0, aggOut = 0, aggCache = 0;
  const flags: string[] = [];

  for (const t of traces) {
    aggIn += t.totalInput; aggOut += t.totalOutput; aggCache += t.totalCache;

    if (t.cachePct < 90 && t.llmCalls > 1)
      flags.push(`\u26a0\ufe0f Low cache (${t.cachePct.toFixed(0)}%) on "${t.input.slice(0, 40)}"`);
    if (t.effective > 20000)
      flags.push(`\ud83d\udd34 High effective tokens (${FMT(t.effective)}) on "${t.input.slice(0, 40)}"`);
    if (t.llmCalls > 10)
      flags.push(`\ud83d\udfe1 Many LLM calls (${t.llmCalls}) on "${t.input.slice(0, 40)}"`);
    const slowGen = t.genDetails.find((g) => g.latency > 60);
    if (slowGen)
      flags.push(`\ud83d\udfe0 Slow LLM call (${slowGen.latency.toFixed(0)}s) in "${t.input.slice(0, 40)}"`);

    const toolCounts = new Map<string, number>();
    for (const td of t.toolDetails) toolCounts.set(td.name, (toolCounts.get(td.name) || 0) + 1);
    for (const [name, count] of [...toolCounts.entries()].filter(([, c]) => c > 2)) {
      flags.push(`\ud83d\udd01 Repeated tool: ${name} called ${count}\u00d7 in "${t.input.slice(0, 40)}"`);
    }
  }

  if (flags.length === 0) {
    console.log("  No issues detected. All metrics look healthy.");
  } else {
    for (const f of flags) console.log(`  ${f}`);
  }

  console.log(`\n  Cache hit rate: ${PCT(aggCache, aggIn)}%`);
  console.log(`  Output/Input:   ${PCT(aggOut, aggIn)}%`);
  console.log(`  Avg eff./trace: ${FMT(Math.round((aggIn - aggCache) / traces.length))} tokens`);
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
        if (!isNaN(n) && n > 0) limit = n;
        break;
      }
      case "--days": case "--from": case "--to":
      case "--tag": case "--user": case "--session":
      case "--name": case "--model":
        i++; // 跳过选项值，避免 --days 7 之类的数字被误当作 limit
        break;
      default: {
        const n = parseInt(args[i]);
        if (!isNaN(n) && n > 0) limit = n;
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
