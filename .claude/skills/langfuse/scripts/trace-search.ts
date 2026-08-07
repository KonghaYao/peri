import {
  detectAnomalies, fetchObservations, fetchTracesFiltered, fmt, fmtLatency, isoToLocal,
  parseFilterArgs, summarizeErrors, summarizeTraceMetrics,
} from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log("用法: bun trace-search.ts [--from ISO] [--to ISO] [--days N] [--tag tag] [--user id] [--session id] [--name text] [--model model] [--status error] [--error-kind provider|tool|cancelled|unknown] [--csv|--json|--summary]");
  process.exit(0);
}
const filter = parseFilterArgs(args);
const value = (flag: string) => { const index = args.indexOf(flag); return index === -1 ? undefined : args[index + 1]; };
const page = Number(value("--page")) || 1;
const orderBy = value("--order") || "timestamp.desc";
const statusError = value("--status") === "error";
const errorKind = value("--error-kind");
const wantedErrorKind: Record<string, string> = { provider: "provider_or_stream_failure", tool: "tool_failure", cancelled: "cancelled_or_user_aborted", unknown: "unknown_failure" };
const outputCsv = args.includes("--csv");
const outputJson = args.includes("--json");
const summaryOnly = args.includes("--summary");

const { traces, meta } = await fetchTracesFiltered({ limit: filter.limit, page, fromTimestamp: filter.time.from, toTimestamp: filter.time.to, tags: filter.tag ? [filter.tag] : undefined, userId: filter.userId, sessionId: filter.sessionId, name: filter.name, orderBy });
if (!traces.length) { console.log("No traces found."); process.exit(0); }

const observations = new Map<string, any[]>();
for (let index = 0; index < traces.length; index += 5) {
  const batch = await Promise.all(traces.slice(index, index + 5).map(async (trace: any) => [trace.id, await fetchObservations(trace.id)] as const));
  for (const [id, items] of batch) observations.set(id, items);
}

const rows = traces.map((trace: any) => {
  const items = observations.get(trace.id) || [];
  const metrics = summarizeTraceMetrics(items);
  const errors = summarizeErrors(items);
  const models = items.filter((item) => item.type === "GENERATION").map((item) => item.model || item.modelName || "unknown");
  return { id: trace.id, timestamp: trace.timestamp || "", name: (trace.name || "").slice(0, 80), userId: trace.userId || "", sessionId: trace.sessionId || "", tags: trace.tags || [], metrics, errors, anomalies: detectAnomalies(metrics), models };
}).filter((row) => {
  if (filter.model && !row.models.some((model: string) => model.toLowerCase().includes(filter.model!.toLowerCase()))) return false;
  if (statusError && !row.errors.hasError) return false;
  return !errorKind || row.errors.categories.includes(wantedErrorKind[errorKind] || errorKind);
});

if (outputJson) { console.log(JSON.stringify({ traces: rows, meta }, null, 2)); process.exit(0); }
if (outputCsv) {
  console.log("id,timestamp,name,userId,sessionId,tags,inputTokens,effectiveNew,outputTokens,cachePct,latency,latencySource,llmCalls,toolCalls,errorCategories,failedLlmCalls,failedToolCalls,flags");
  for (const row of rows) {
    const cachePct = row.metrics.inputTokens ? row.metrics.cacheReadTokens / row.metrics.inputTokens * 100 : 0;
    console.log([row.id, row.timestamp, `"${row.name.replace(/"/g, '""')}"`, row.userId, row.sessionId, `"${row.tags.join(";")}"`, row.metrics.inputTokens, row.metrics.effectiveNewTokens, row.metrics.outputTokens, cachePct.toFixed(1), row.metrics.latency.seconds ?? "", row.metrics.latency.source, row.metrics.llmCalls, row.metrics.toolCalls, row.errors.categories.join(";"), row.errors.failedLlmCalls, row.errors.failedToolCalls, row.anomalies.map((item) => item.type).join(";")].join(","));
  }
  process.exit(0);
}

if (summaryOnly) {
  const total = rows.reduce((sum, row) => ({ input: sum.input + row.metrics.inputTokens, output: sum.output + row.metrics.outputTokens, effective: sum.effective + row.metrics.effectiveNewTokens, llm: sum.llm + row.metrics.llmCalls, tools: sum.tools + row.metrics.toolCalls }), { input: 0, output: 0, effective: 0, llm: 0, tools: 0 });
  console.log(`## Search Summary\n  Traces: ${rows.length} (page ${page}/${meta.totalPages || "?"})\n  LLM calls: ${total.llm}  Tool calls: ${total.tools}\n  Effective new: ${fmt(total.effective)}  Output: ${fmt(total.output)}`);
  process.exit(0);
}

console.log(`\n## Trace Search Results (${rows.length} traces, page ${page}/${meta.totalPages || "?"})\n\n| # | Time | Name | Eff.New | LLM | Tools | Lat | Errors | Flags |\n|---|------|------|---------|-----|-------|-----|--------|-------|`);
for (const [index, row] of rows.entries()) console.log(`| ${index + 1 + (page - 1) * filter.limit} | ${isoToLocal(row.timestamp)} | ${row.name.replace(/\|/g, "\\|").slice(0, 50)} | ${fmt(row.metrics.effectiveNewTokens)} | ${row.metrics.llmCalls} | ${row.metrics.toolCalls} | ${fmtLatency(row.metrics.latency)} | ${row.errors.categories.join(",") || "-"} | ${row.anomalies.map((item) => item.type).join(",") || "-"} |`);

const anomalies = rows.flatMap((row) => row.anomalies.map((anomaly) => ({ ...anomaly, traceId: row.id })));
if (anomalies.length) {
  console.log(`\n### Diagnostics (${anomalies.length})\n`);
  for (const anomaly of anomalies.slice(0, 10)) console.log(`- [${anomaly.traceId?.slice(0, 8)}] ${anomaly.description}`);
}
