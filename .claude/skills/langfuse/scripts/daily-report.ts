import {
  detectAnomalies, estimateGenerationsCost, fetchAllTracesFiltered, fetchObservations, fmt, fmtCost,
  fmtLatency, isoNow, isoSpan, isoToLocal, pct, summarizeErrors, summarizeTraceMetrics,
  totalInputTraffic, TraceFilter,
} from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log("用法: bun daily-report.ts [--days N] [--from ISO] [--to ISO] [--tag tag] [--model model] [--detail]");
  process.exit(0);
}

const value = (flag: string) => { const index = args.indexOf(flag); return index === -1 ? undefined : args[index + 1]; };
const days = Number(value("--days")) || 1;
const to = value("--to") || isoNow();
const from = value("--from") || isoSpan(to, days);
const tag = value("--tag");
const modelFilter = value("--model");
const detailMode = args.includes("--detail");
console.error(`Time range: ${from} → ${to}`);

const filter: TraceFilter = { fromTimestamp: from, toTimestamp: to, tags: tag ? [tag] : undefined, limit: 100 };
const traces = await fetchAllTracesFiltered({ ...filter, maxPages: 10 });
if (!traces.length) { console.log("\n## Daily Report\n\nNo traces found in this time range."); process.exit(0); }

const observations = new Map<string, any[]>();
for (let index = 0; index < traces.length; index += 5) {
  const batch = await Promise.all(traces.slice(index, index + 5).map(async (trace: any) => [trace.id, await fetchObservations(trace.id)] as const));
  for (const [id, items] of batch) observations.set(id, items);
}

interface TraceRow {
  id: string; timestamp: string; name: string; sessionId: string; model: string; cost: number;
  metrics: ReturnType<typeof summarizeTraceMetrics>; errors: ReturnType<typeof summarizeErrors>; flags: ReturnType<typeof detectAnomalies>;
}
const rows: TraceRow[] = [];
for (const trace of traces) {
  const items = observations.get(trace.id) || [];
  const metrics = summarizeTraceMetrics(items);
  const generations = items.filter((item) => item.type === "GENERATION");
  const modelCounts = new Map<string, number>();
  for (const generation of generations) { const model = generation.model || generation.modelName || "unknown"; modelCounts.set(model, (modelCounts.get(model) || 0) + 1); }
  const model = [...modelCounts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || "unknown";
  if (modelFilter && !model.toLowerCase().includes(modelFilter.toLowerCase())) continue;
  rows.push({ id: trace.id, timestamp: trace.timestamp || "", name: (trace.name || "").slice(0, 80), sessionId: trace.sessionId || "", model, cost: estimateGenerationsCost(generations), metrics, errors: summarizeErrors(items), flags: detectAnomalies(metrics) });
}

if (!rows.length) {
  console.log("\n## Daily Report\n\nNo traces matched the requested model filter.");
  process.exit(0);
}

const aggregate = rows.reduce((sum, row) => {
  const { metrics } = row;
  sum.input += metrics.inputTokens; sum.output += metrics.outputTokens; sum.effective += metrics.effectiveNewTokens; sum.cacheRead += metrics.cacheReadTokens; sum.cacheCreate += metrics.cacheCreateTokens; sum.llm += metrics.llmCalls; sum.tools += metrics.toolCalls; sum.cost += row.cost;
  if (metrics.latency.seconds !== null) { sum.latency += metrics.latency.seconds; sum.observed += 1; sum.sources[metrics.latency.source]++; }
  for (const category of row.errors.categories) sum.errors[category] = (sum.errors[category] || 0) + 1;
  return sum;
}, { input: 0, output: 0, effective: 0, cacheRead: 0, cacheCreate: 0, llm: 0, tools: 0, cost: 0, latency: 0, observed: 0, sources: { "agent-run": 0, observations: 0, unavailable: 0 } as Record<string, number>, errors: {} as Record<string, number> });

console.log("\n## Daily Report\n");
console.log(`Range: ${isoToLocal(from)} → ${isoToLocal(to)} (${days}d)`);
console.log("\n### Key Metrics\n\n| Metric | Value |\n|--------|-------|");
console.log(`| Traces | ${rows.length} |`);
console.log(`| LLM calls | ${aggregate.llm} (avg ${(aggregate.llm / rows.length).toFixed(1)}/trace) |`);
console.log(`| Tool calls | ${aggregate.tools} |`);
console.log(`| Effective new | ${fmt(aggregate.effective)} tokens |`);
console.log(`| Cache read | ${fmt(aggregate.cacheRead)} (${pct(aggregate.cacheRead, totalInputTraffic(aggregate))} of input traffic) |`);
console.log(`| Avg observed latency | ${aggregate.observed ? fmtLatency({ seconds: aggregate.latency / aggregate.observed, source: "observations" }) : "N/A"} |`);
console.log(`| Latency coverage | ${aggregate.observed}/${rows.length} (agent-run=${aggregate.sources["agent-run"]}, observations=${aggregate.sources.observations}) |`);
console.log(`| Errors by category | ${Object.entries(aggregate.errors).map(([kind, count]) => `${kind}=${count}`).join(", ") || "none"} |`);
console.log(`| Est. cost | ${fmtCost(aggregate.cost)} |`);

rows.sort((a, b) => b.metrics.inputTokens - a.metrics.inputTokens);
const top = rows.slice(0, 10);
console.log(`\n### Top ${top.length} Traces (by input tokens)\n\n| # | Time | Name | Input | Eff.New | Output | LLM | Tools | Lat | Flags |\n|---|------|------|-------|---------|--------|-----|-------|-----|-------|`);
for (const [index, row] of top.entries()) {
  const flags = row.flags.map((flag) => flag.type).join(",") || "-";
  console.log(`| ${index + 1} | ${isoToLocal(row.timestamp)} | ${row.name.replace(/\|/g, "\\|").slice(0, 45)} | ${fmt(row.metrics.inputTokens)} | ${fmt(row.metrics.effectiveNewTokens)} | ${fmt(row.metrics.outputTokens)} | ${row.metrics.llmCalls} | ${row.metrics.toolCalls} | ${fmtLatency(row.metrics.latency)} | ${flags} |`);
}

const anomalyRows = rows.filter((row) => row.flags.length || row.errors.hasError);
if (anomalyRows.length) {
  console.log(`\n### Diagnostics (${anomalyRows.length} traces)\n`);
  for (const row of anomalyRows) {
    const flags = row.flags.map((flag) => flag.description).join("; ");
    const errors = row.errors.categories.join(", ");
    console.log(`- [${row.id.slice(0, 8)}] ${flags || ""}${flags && errors ? "; " : ""}${errors ? `errors=${errors}` : ""}`);
  }
}

if (detailMode) {
  console.log("\n### All Traces\n\n| # | Time | Name | LLM | Tools | Effective new | Lat | Flags |\n|---|------|------|-----|-------|---------------|-----|-------|");
  for (const [index, row] of rows.entries()) console.log(`| ${index + 1} | ${isoToLocal(row.timestamp)} | ${row.name.replace(/\|/g, "\\|").slice(0, 50)} | ${row.metrics.llmCalls} | ${row.metrics.toolCalls} | ${fmt(row.metrics.effectiveNewTokens)} | ${fmtLatency(row.metrics.latency)} | ${row.flags.map((flag) => flag.type).join(",") || "-"} |`);
}
