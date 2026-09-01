import {
  clampTraceLimit, detectAnomalies, estimateGenerationsCost, fetchAllTracesFiltered, fetchObservations, fmt, fmtCost,
  fmtLatency, genTokens, isoToLocal, pct, summarizeErrors, summarizeObservationLatency,
  summarizeTraceMetrics, totalInputTraffic,
} from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h") || !args.includes("--session")) {
  console.log("用法: bun session-analyze.ts --session <sessionId> [--limit N] [--detail] [--csv]");
  process.exit(args.includes("--help") || args.includes("-h") ? 0 : 1);
}
const value = (flag: string) => { const index = args.indexOf(flag); return index === -1 ? undefined : args[index + 1]; };
const sessionId = value("--session")!;
const limit = clampTraceLimit(Number(value("--limit")) || 100);
const detailMode = args.includes("--detail");
const csvMode = args.includes("--csv");
const traces = await fetchAllTracesFiltered({ sessionId, limit, maxPages: 10 });
if (!traces.length) { console.log("No traces found for this session."); process.exit(0); }
traces.sort((a: any, b: any) => String(a.timestamp || "").localeCompare(String(b.timestamp || "")));

const observations = new Map<string, any[]>();
for (let index = 0; index < traces.length; index += 5) {
  const batch = await Promise.all(traces.slice(index, index + 5).map(async (trace: any) => [trace.id, await fetchObservations(trace.id)] as const));
  for (const [id, items] of batch) observations.set(id, items);
}

const analyses = traces.map((trace: any, index: number) => {
  const items = observations.get(trace.id) || [];
  const metrics = summarizeTraceMetrics(items);
  return { idx: index + 1, id: trace.id, timestamp: trace.timestamp || "", name: (trace.name || "").slice(0, 80), metrics, errors: summarizeErrors(items), flags: detectAnomalies(metrics), generations: items.filter((item) => item.type === "GENERATION"), tools: items.filter((item) => item.type === "TOOL") };
});

if (csvMode) {
  console.log("traceIdx,traceId,timestamp,genIdx,model,input,cacheRead,cacheCreate,effective,output,latency,traceLatencySource,traceFlags,errorCategories");
  for (const analysis of analyses) for (const [index, generation] of analysis.generations.entries()) {
    const tokens = genTokens(generation);
    const latency = summarizeObservationLatency(generation);
    console.log([analysis.idx, analysis.id, analysis.timestamp, index + 1, generation.model || generation.modelName || "?", tokens.input, tokens.cacheRead, tokens.cacheCreate, tokens.input, tokens.output, latency ?? "", analysis.metrics.latency.source, analysis.flags.map((flag) => flag.type).join(";"), analysis.errors.categories.join(";")].join(","));
  }
  process.exit(0);
}

const aggregate = analyses.reduce((sum, analysis) => {
  const metrics = analysis.metrics;
  sum.input += metrics.inputTokens; sum.output += metrics.outputTokens; sum.cacheRead += metrics.cacheReadTokens; sum.cacheCreate += metrics.cacheCreateTokens; sum.effective += metrics.effectiveNewTokens; sum.llm += metrics.llmCalls; sum.tools += metrics.toolCalls;
  if (metrics.latency.seconds !== null) { sum.latency += metrics.latency.seconds; sum.observed += 1; }
  for (const category of analysis.errors.categories) sum.errors[category] = (sum.errors[category] || 0) + 1;
  return sum;
}, { input: 0, output: 0, cacheRead: 0, cacheCreate: 0, effective: 0, llm: 0, tools: 0, latency: 0, observed: 0, errors: {} as Record<string, number> });
const sessionCost = estimateGenerationsCost(analyses.flatMap((analysis) => analysis.generations));
console.log(`\n## Session: ${sessionId}\n\n| Metric | Value |\n|--------|-------|`);
console.log(`| Traces | ${analyses.length} |\n| LLM calls | ${aggregate.llm} |\n| Tool calls | ${aggregate.tools} |\n| Effective new | ${fmt(aggregate.effective)} tokens |\n| Cache read | ${fmt(aggregate.cacheRead)} (${pct(aggregate.cacheRead, totalInputTraffic(aggregate))} of input traffic) |\n| Avg observed latency | ${aggregate.observed ? fmtLatency({ seconds: aggregate.latency / aggregate.observed, source: "observations" }) : "N/A"} (${aggregate.observed}/${analyses.length} traces) |\n| Errors by category | ${Object.entries(aggregate.errors).map(([kind, count]) => `${kind}=${count}`).join(", ") || "none"} |\n| Est. cost | ${fmtCost(sessionCost)} |`);
console.log("\n### Trace Timeline\n\n| # | Time | Name | Eff.New | LLM | Tools | Lat | Errors | Flags |\n|---|------|------|---------|-----|-------|-----|--------|-------|");
for (const analysis of analyses) console.log(`| ${analysis.idx} | ${isoToLocal(analysis.timestamp)} | ${analysis.name.replace(/\|/g, "\\|").slice(0, 50)} | ${fmt(analysis.metrics.effectiveNewTokens)} | ${analysis.metrics.llmCalls} | ${analysis.metrics.toolCalls} | ${fmtLatency(analysis.metrics.latency)} | ${analysis.errors.categories.join(",") || "-"} | ${analysis.flags.map((flag) => flag.type).join(",") || "-"} |`);

if (detailMode) for (const analysis of analyses) {
  if (!analysis.generations.length) continue;
  console.log(`\n--- Trace #${analysis.idx}: ${analysis.name.slice(0, 50)}\n\n| Round | Input | Cache | Eff.New | Output | Lat |\n|-------|-------|-------|---------|--------|-----|`);
  for (const [index, generation] of analysis.generations.entries()) {
    const tokens = genTokens(generation);
    const latency = summarizeObservationLatency(generation);
    console.log(`| ${index + 1} | ${fmt(tokens.input)} | ${fmt(tokens.cacheRead)} | ${fmt(tokens.input)} | ${fmt(tokens.output)} | ${latency === null ? "N/A" : fmtLatency({ seconds: latency, source: "observations" })} |`);
  }
}

const diagnostics = analyses.flatMap((analysis) => analysis.flags.map((flag) => ({ ...flag, trace: analysis.id })));
if (diagnostics.length) {
  console.log(`\n### Diagnostics (${diagnostics.length})\n`);
  for (const diagnostic of diagnostics) console.log(`- [${diagnostic.trace.slice(0, 8)}] ${diagnostic.description}`);
}
