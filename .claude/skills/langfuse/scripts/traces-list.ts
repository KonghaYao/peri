#!/usr/bin/env bun
/**
 * 列出 Trace 的 token 汇总（支持过滤）
 *
 * 用法: bun .claude/skills/langfuse/scripts/traces-list.ts [N] [选项]
 *
 * 过滤:
 *   --from <ISO>   起始时间
 *   --to <ISO>     结束时间
 *   --days <N>     最近 N 天
 *   --tag <tag>    按 tag 过滤
 *   --user <id>    按用户过滤
 *   --session <id> 按 session 过滤
 *   --name <str>   按 name 过滤
 *   --limit <N>    条数限制
 */
import { fetchTracesFiltered, fetchObservations, parseFilterArgs, genTokens, fmt, pct, fmtLatency, summarizeLatency, totalInputTraffic } from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log("用法: bun traces-list.ts [N] [--from ISO] [--to ISO] [--days N] [--tag tag] [--user id] [--session id] [--name text] [--limit N]");
  process.exit(0);
}
const filter = parseFilterArgs(args);

if (filter.time.from) {
  console.error(`Time range: ${filter.time.from} → ${filter.time.to || "now"}`);
}

console.error(`Fetching latest ${filter.limit} traces...`);

const { traces } = await fetchTracesFiltered({
  limit: filter.limit,
  fromTimestamp: filter.time.from,
  toTimestamp: filter.time.to,
  tags: filter.tag ? [filter.tag] : undefined,
  userId: filter.userId,
  sessionId: filter.sessionId,
  name: filter.name,
});

if (traces.length === 0) {
  console.log("No traces found.");
  process.exit(0);
}

interface TraceSummary {
  id: string;
  llmCalls: number;
  toolCalls: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
  totalCacheCreate: number;
  effective: number;
  cachePct: number;
  latency: ReturnType<typeof summarizeLatency>;
  timestamp: string;
}

const summaries: TraceSummary[] = [];

for (let i = 0; i < traces.length; i += 5) {
  const batch = traces.slice(i, i + 5);
  const obsResults = await Promise.all(batch.map((t: any) => fetchObservations(t.id)));
  for (let j = 0; j < batch.length; j++) {
    const t = batch[j];
    const obs = obsResults[j];
    const gens = obs.filter((o: any) => o.type === "GENERATION");
    const tools = obs.filter((o: any) => o.type === "TOOL");

    let totalInput = 0, totalOutput = 0, totalCache = 0, totalCacheCreate = 0;
    for (const g of gens) {
      const tk = genTokens(g);
      totalInput += tk.input;
      totalOutput += tk.output;
      totalCache += tk.cacheRead;
      totalCacheCreate += tk.cacheCreate;
    }

    summaries.push({
      id: t.id,
      llmCalls: gens.length,
      toolCalls: tools.length,
      totalInput,
      totalOutput,
      totalCache,
      totalCacheCreate,
      effective: totalInput,
      cachePct: totalInputTraffic({ input: totalInput, cacheRead: totalCache, cacheCreate: totalCacheCreate }) > 0
        ? (totalCache / totalInputTraffic({ input: totalInput, cacheRead: totalCache, cacheCreate: totalCacheCreate })) * 100
        : 0,
      latency: summarizeLatency(obs),
      timestamp: t.timestamp || "",
    });
  }
}

console.log("| # | Trace | LLM | Tools | Input tok | Output tok | Cache% | Eff. new | Latency |");
console.log("|---|--------------|-----|-------|-----------|------------|--------|----------|---------|");

for (let i = 0; i < summaries.length; i++) {
  const s = summaries[i];
  console.log(
    `| ${i + 1} | ${s.id.slice(0, 12)} | ${s.llmCalls} | ${s.toolCalls} | ${fmt(s.totalInput)} | ${fmt(s.totalOutput)} | ${s.cachePct.toFixed(1)}% | ${fmt(s.effective)} | ${fmtLatency(s.latency)} |`
  );
}

const agg = summaries.reduce(
  (a, s) => ({
    input: a.input + s.totalInput,
    output: a.output + s.totalOutput,
    cache: a.cache + s.totalCache,
    cacheCreate: a.cacheCreate + s.totalCacheCreate,
    calls: a.calls + s.llmCalls,
    tools: a.tools + s.toolCalls,
  }),
  { input: 0, output: 0, cache: 0, cacheCreate: 0, calls: 0, tools: 0 }
);

console.log("\n## Aggregate");
console.log(`  Traces: ${summaries.length}  LLM calls: ${agg.calls}  Tool calls: ${agg.tools}`);
console.log(`  Raw input: ${fmt(agg.input)}  Output: ${fmt(agg.output)}  Cache read: ${fmt(agg.cache)} (${pct(agg.cache, totalInputTraffic({ input: agg.input, cacheRead: agg.cache, cacheCreate: agg.cacheCreate }))} of input traffic)  Cache create: ${fmt(agg.cacheCreate)}`);
console.log(`  Effective new: ${fmt(agg.input)}`);
console.log(`  Output/Input: ${agg.input > 0 ? ((agg.output / agg.input) * 100).toFixed(2) + "%" : "-"}`);
