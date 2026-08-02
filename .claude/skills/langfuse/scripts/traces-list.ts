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
import { fetchTracesFiltered, fetchObservations, parseFilterArgs, summarizeTokens, genTokens, fmt, pct, ms, isoToLocal } from "./lib.ts";

const args = process.argv.slice(2);
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
  input: string;
  llmCalls: number;
  toolCalls: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
  effective: number;
  cachePct: number;
  latency: number;
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

    let totalInput = 0, totalOutput = 0, totalCache = 0;
    for (const g of gens) {
      const tk = genTokens(g);
      totalInput += tk.input;
      totalOutput += tk.output;
      totalCache += tk.cacheRead;
    }

    summaries.push({
      id: t.id,
      input: (t.input as string)?.slice(0, 45) || "",
      llmCalls: gens.length,
      toolCalls: tools.length,
      totalInput,
      totalOutput,
      totalCache,
      effective: totalInput - totalCache,
      cachePct: totalInput > 0 ? (totalCache / totalInput) * 100 : 0,
      latency: t.latency || 0,
      timestamp: t.timestamp || "",
    });
  }
}

console.log("| # | Input | LLM | Tools | Input tok | Output tok | Cache% | Eff. new | Latency |");
console.log("|---|---------------------------------------------|-----|-------|-----------|------------|--------|----------|---------|");

for (let i = 0; i < summaries.length; i++) {
  const s = summaries[i];
  const label = s.input.replace(/\|/g, "\\|");
  console.log(
    `| ${i + 1} | ${label} | ${s.llmCalls} | ${s.toolCalls} | ${fmt(s.totalInput)} | ${fmt(s.totalOutput)} | ${s.cachePct.toFixed(1)}% | ${fmt(s.effective)} | ${ms(s.latency)} |`
  );
}

const agg = summaries.reduce(
  (a, s) => ({
    input: a.input + s.totalInput,
    output: a.output + s.totalOutput,
    cache: a.cache + s.totalCache,
    calls: a.calls + s.llmCalls,
    tools: a.tools + s.toolCalls,
  }),
  { input: 0, output: 0, cache: 0, calls: 0, tools: 0 }
);

console.log("\n## Aggregate");
console.log(`  Traces: ${summaries.length}  LLM calls: ${agg.calls}  Tool calls: ${agg.tools}`);
console.log(`  Input: ${fmt(agg.input)}  Output: ${fmt(agg.output)}  Cache: ${fmt(agg.cache)} (${pct(agg.cache, agg.input)})`);
console.log(`  Effective new: ${fmt(agg.input - agg.cache)}`);
console.log(`  Output/Input: ${agg.input > 0 ? ((agg.output / agg.input) * 100).toFixed(2) + "%" : "-"}`);
