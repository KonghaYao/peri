#!/usr/bin/env bun
/**
 * 单 trace 逐轮 token 流 + 缓存异常检测（支持从过滤结果中选 trace）
 *
 * 用法:
 *   bun .claude/skills/langfuse/scripts/trace-tokens.ts <traceId>
 *   bun .claude/skills/langfuse/scripts/trace-tokens.ts --index <N> [--from/--to/--days/--tag/--user/--session/--name/--limit]
 */
import { fetchObservations, fetchTracesFiltered, parseFilterArgs, parseTraceArg, genTokens, fmt, pct, fmtLatency, summarizeLatency, summarizeObservationLatency, totalInputTraffic } from "./lib.ts";

const args = process.argv.slice(2);

// Help
if (args.includes("--help") || args.includes("-h")) {
  console.log(`用法: bun trace-tokens.ts <traceId>
       bun trace-tokens.ts --index <N> [过滤选项]

过滤选项: --from <ISO> --to <ISO> --days <N> --tag <t> --user <id> --session <id> --name <s> --limit <N>`);
  process.exit(0);
}

// Find trace to analyze: 第一个非选项参数是 traceId，--index <N> 从过滤结果中选
let traceId: string | undefined;
const { traceId: parsedTraceId, index } = parseTraceArg(args);
traceId = parsedTraceId;

// Or use --index to pick from filtered results
if (!traceId && index !== undefined) {
  const filter = parseFilterArgs(args);
  console.error(`Fetching filtered traces (limit ${filter.limit})...`);
  const { traces } = await fetchTracesFiltered({
    limit: filter.limit,
    fromTimestamp: filter.time.from,
    toTimestamp: filter.time.to,
    tags: filter.tag ? [filter.tag] : undefined,
    userId: filter.userId,
    sessionId: filter.sessionId,
    name: filter.name,
  });
  if (traces.length === 0) { console.log("No traces found."); process.exit(0); }
  if (index > traces.length) { console.log(`Index ${index} out of range (max ${traces.length}).`); process.exit(1); }
  traceId = traces[index - 1].id;
  console.error(`Using trace #${index}: ${traceId}`);
}

if (!traceId) { console.error("Usage: bun trace-tokens.ts <traceId>  or  --index <N>"); process.exit(1); }

const observations = await fetchObservations(traceId);

const generations = observations.filter((o: any) => o.type === "GENERATION");
if (!generations.length) {
  console.log("No LLM generations found.");
  process.exit(0);
}

console.log(`## Trace: ${traceId.slice(0, 12)}`);
console.log(`   Latency: ${fmtLatency(summarizeLatency(observations))} | Generations: ${generations.length}\n`);

// --- Token flow ---
console.log("### Token Flow\n");
console.log("| # | Input | Cache Read | Cache Create | Eff. New | Output | Cache% | Cumul. New |");
console.log("|---|-------|------------|--------------|----------|--------|--------|------------|");

let cumulativeNew = 0;
let totalInput = 0, totalCacheRead = 0, totalCacheCreate = 0, totalOutput = 0;
const roundTokens: { idx: number; input: number; cacheRead: number; cacheCreate: number; output: number; effective: number }[] = [];

for (let i = 0; i < generations.length; i++) {
  const tk = genTokens(generations[i]);
  const effective = tk.input;
  cumulativeNew += effective;
  totalInput += tk.input;
  totalCacheRead += tk.cacheRead;
  totalCacheCreate += tk.cacheCreate;
  totalOutput += tk.output;

  const cachePct = pct(tk.cacheRead, totalInputTraffic(tk));
  console.log(
    `| ${i + 1} | ${fmt(tk.input)} | ${fmt(tk.cacheRead)} | ${fmt(tk.cacheCreate)} | ${fmt(effective)} | ${fmt(tk.output)} | ${cachePct} | ${fmt(cumulativeNew)} |`
  );
  roundTokens.push({ idx: i, ...tk, effective });
}

console.log(
  `\n**Totals**: RawInput=${fmt(totalInput)} CacheRead=${fmt(totalCacheRead)} (${pct(totalCacheRead, totalInputTraffic({ input: totalInput, cacheRead: totalCacheRead, cacheCreate: totalCacheCreate }))} of input traffic) CacheCreate=${fmt(totalCacheCreate)} Output=${fmt(totalOutput)}`
);

// --- Cache anomalies ---
console.log("\n### Cache Anomalies\n");
let anomalies = 0;

for (let i = 0; i < roundTokens.length; i++) {
  const r = roundTokens[i];

  // cache hit drop
  if (i > 0) {
    const prev = roundTokens[i - 1];
    const prevTraffic = totalInputTraffic(prev);
    const curTraffic = totalInputTraffic(r);
    const prevPct = prevTraffic > 0 ? (prev.cacheRead / prevTraffic) * 100 : 100;
    const curPct = curTraffic > 0 ? (r.cacheRead / curTraffic) * 100 : 100;
    if (curPct < prevPct - 10) {
      console.log(`  ⚠️ Round ${i + 1}: Cache hit dropped ${prevPct.toFixed(1)}% → ${curPct.toFixed(1)}% (${(prevPct - curPct).toFixed(0)}pp)`);
      anomalies++;
    }
  }

  // cache creation
  if (r.cacheCreate > 0) {
    console.log(`  📦 Round ${i + 1}: Cache creation = ${fmt(r.cacheCreate)} tokens (new prefix cached)`);
    anomalies++;
  }

  // high effective new
  if (r.effective > 5000) {
    console.log(`  🔴 Round ${i + 1}: Effective new = ${fmt(r.effective)} (>5K — check tool results or context injection)`);
    anomalies++;
  }

  // high latency
  const latency = summarizeObservationLatency(generations[i]);
  if (latency !== null && latency > 60) {
    console.log(`  🐌 Round ${i + 1}: Latency = ${latency.toFixed(1)}s (>60s)`);
    anomalies++;
  }
}

// input tokens decreasing = context truncation / compact
for (let i = 1; i < roundTokens.length; i++) {
  const prev = roundTokens[i - 1];
  const curr = roundTokens[i];
  const prevTraffic = totalInputTraffic(prev);
  const currTraffic = totalInputTraffic(curr);
  if (currTraffic < prevTraffic * 0.85) {
    console.log(`  ✂️ Round ${i + 1}: Input traffic dropped ${fmt(prevTraffic)} → ${fmt(currTraffic)} (possible compact/truncation)`);
    anomalies++;
  }
}

if (anomalies === 0) {
  console.log("  ✅ No anomalies detected.");
}
