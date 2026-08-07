#!/usr/bin/env bun
/**
 * 单 trace 的 system prompt 段落拆解（支持从过滤结果中选 trace）
 *
 * 用法:
 *   bun .claude/skills/langfuse/scripts/prompt-breakdown.ts <traceId>
 *   bun .claude/skills/langfuse/scripts/prompt-breakdown.ts --index <N> [过滤选项]
 */
import { fetchObservations, fetchTracesFiltered, parseFilterArgs, parseTraceArg, fmt, pct } from "./lib.ts";

const args = process.argv.slice(2);

if (args.includes("--help") || args.includes("-h")) {
  console.log(`用法: bun prompt-breakdown.ts <traceId>
       bun prompt-breakdown.ts --index <N> [过滤选项]

过滤选项: --from <ISO> --to <ISO> --days <N> --tag <t> --user <id> --session <id> --name <s> --limit <N>`);
  process.exit(0);
}

// Find trace to analyze: 第一个非选项参数是 traceId，--index <N> 从过滤结果中选
let traceId: string | undefined;
const { traceId: parsedTraceId, index } = parseTraceArg(args);
traceId = parsedTraceId;

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

if (!traceId) { console.error("Usage: bun prompt-breakdown.ts <traceId>  or  --index <N>"); process.exit(1); }

const observations = await fetchObservations(traceId);

const generations = observations.filter((o: any) => o.type === "GENERATION");
if (!generations.length) { console.log("No LLM generations found."); process.exit(0); }

console.log(`## Trace: ${traceId.slice(0, 12)}\n`);

const gen = generations[0];
const input = gen.input;
let messages: any[] = [];
if (typeof input === "string") {
  try { messages = JSON.parse(input).messages || []; } catch {}
} else if (input && typeof input === "object") {
  messages = (input as any).messages || [];
}

interface Section {
  role: string;
  chars: number;
}

const sections: Section[] = [];
let totalChars = 0;

for (const m of messages) {
  const role = m.role || "?";
  const content = m.content ?? "";
  const text = typeof content === "string" ? content : JSON.stringify(content);
  const chars = text.length;

  sections.push({ role, chars });
  totalChars += chars;
}

// --- By Role ---
console.log("### By Role\n");
const roleGroups: Record<string, { count: number; chars: number }> = {};
for (const s of sections) {
  if (!roleGroups[s.role]) roleGroups[s.role] = { count: 0, chars: 0 };
  roleGroups[s.role].count++;
  roleGroups[s.role].chars += s.chars;
}

console.log("| Role | Count | Chars | % of Total |");
console.log("|------|-------|-------|------------|");
for (const [role, g] of Object.entries(roleGroups).sort((a, b) => b[1].chars - a[1].chars)) {
  console.log(`| ${role} | ${g.count} | ${fmt(g.chars)} | ${pct(g.chars, totalChars)} |`);
}
console.log(`| **Total** | **${sections.length}** | **${fmt(totalChars)}** | |`);

// --- System sections detail ---
const sysSections = sections.filter((s) => s.role === "system");
if (sysSections.length > 0) {
  const sysTotal = sysSections.reduce((a, s) => a + s.chars, 0);
  console.log(`\n### System Prompt Summary (${fmt(sysTotal)} chars, ${pct(sysTotal, totalChars)} of total)\n`);
  console.log("| System messages | Chars | % of System | % of Total |");
  console.log("|-----------------|-------|-------------|------------|");
  console.log(`| ${sysSections.length} | ${fmt(sysTotal)} | 100.0% | ${pct(sysTotal, totalChars)} |`);
}

// --- Top 10 non-system ---
const nonSys = sections.filter((s) => s.role !== "system").sort((a, b) => b.chars - a.chars);
if (nonSys.length > 0) {
  console.log(`\n### Top 10 Largest Non-System Messages\n`);
  console.log("| # | Role | Chars | % of Total |");
  console.log("|---|------|-------|------------|");
  for (let i = 0; i < Math.min(10, nonSys.length); i++) {
    const s = nonSys[i];
    console.log(`| ${i + 1} | ${s.role} | ${fmt(s.chars)} | ${pct(s.chars, totalChars)} |`);
  }
}
