import { api, fetchObservations, fetchTracesFiltered, fmt, parseFilterArgs, parseTraceArg, splitGenerationAgentSegments } from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log("用法: bun trace-messages.ts <traceId> | --index <N> [过滤选项]");
  process.exit(0);
}

let traceId = parseTraceArg(args).traceId;
const selectedIndex = parseTraceArg(args).index;
if (!traceId && selectedIndex !== undefined) {
  const filter = parseFilterArgs(args);
  const { traces } = await fetchTracesFiltered({ limit: filter.limit, fromTimestamp: filter.time.from, toTimestamp: filter.time.to, tags: filter.tag ? [filter.tag] : undefined, userId: filter.userId, sessionId: filter.sessionId, name: filter.name });
  if (!traces.length) { console.log("No traces found."); process.exit(0); }
  if (selectedIndex > traces.length) { console.log(`Index ${selectedIndex} out of range (max ${traces.length}).`); process.exit(1); }
  traceId = traces[selectedIndex - 1].id;
}
if (!traceId) { console.error("Usage: bun trace-messages.ts <traceId>  or  --index <N>"); process.exit(1); }

interface MessageSummary { role: string; chars: number; signature: string; }
function messageText(content: any): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((item) => typeof item === "string" ? item : item?.type === "text" ? String(item.text || "") : `[${String(item?.type || "unknown")}]`).join("\n");
  return content === undefined || content === null ? "" : JSON.stringify(content);
}
function parseMessages(raw: any): MessageSummary[] {
  try {
    const payload = typeof raw === "string" ? JSON.parse(raw) : raw;
    const messages = payload?.messages;
    if (!Array.isArray(messages)) return [];
    return messages.map((message: any) => {
      const text = messageText(message.content ?? message.tool_calls);
      return { role: String(message.role || "?"), chars: text.length, signature: `${message.role || "?"}\u0000${text}` };
    });
  } catch { return []; }
}
function diffMessages(previous: MessageSummary[], current: MessageSummary[]) {
  let changed = 0;
  for (let index = 0; index < current.length; index++) if (!previous[index] || previous[index].signature !== current[index].signature) changed++;
  return { added: Math.max(0, current.length - previous.length), changed };
}
function systemSignature(messages: MessageSummary[]) { return messages.filter((message) => message.role === "system").map((message) => message.signature).join("\u0001"); }

const [trace, observations] = await Promise.all([api(`/api/public/traces/${traceId}`), fetchObservations(traceId)]);
const generations = observations.filter((observation: any) => observation.type === "GENERATION");
if (!generations.length) { console.log("No LLM generations found."); process.exit(0); }
console.log(`## Trace: ${trace?.name ? String(trace.name).slice(0, 60) : String(trace?.id || traceId).slice(0, 12)}`);
console.log(`   Generations: ${generations.length}\n`);
console.log("### Message Composition\n\n| # | Sys | User | Asst | Tool | Total | New/Changed | Δ chars |\n|---|-----|------|------|------|-------|-------------|---------|");
let previous: MessageSummary[] = [];
const messagesByGeneration = new Map<any, MessageSummary[]>();
for (const [index, generation] of generations.entries()) {
  const messages = parseMessages(generation.input);
  messagesByGeneration.set(generation, messages);
  const diff = diffMessages(previous, messages);
  const roles: Record<string, number> = { system: 0, user: 0, assistant: 0, tool: 0 };
  let changedChars = 0;
  for (const [messageIndex, message] of messages.entries()) { roles[message.role === "tool" ? "tool" : message.role] = (roles[message.role === "tool" ? "tool" : message.role] || 0) + 1; if (!previous[messageIndex] || previous[messageIndex].signature !== message.signature) changedChars += message.chars; }
  console.log(`| ${index + 1} | ${roles.system} | ${roles.user} | ${roles.assistant} | ${roles.tool} | ${messages.length} | ${diff.added + diff.changed} | ${fmt(changedChars)} |`);
  previous = messages;
}

console.log("\n### System Prompt Stability\n\n| Segment | Agent | Rounds | System messages | Changed rounds | Status |\n|---------|-------|--------|-----------------|----------------|--------|");
const generationIndex = new Map(generations.map((generation: any, index: number) => [generation, index + 1]));
for (const [index, segment] of splitGenerationAgentSegments(observations, generations).entries()) {
  const roundNumbers = segment.generations.map((generation) => generationIndex.get(generation)!);
  if (segment.agentObservationId === null || segment.agentLabel === "unknown") {
    console.log(`| ${index + 1} | unknown | ${roundNumbers.join(",")} | - | - | Unknown ownership; not compared across agents |`);
    continue;
  }
  const signatures = segment.generations.map((generation) => systemSignature(messagesByGeneration.get(generation) || []));
  const baseline = signatures[0] || "";
  const changedRounds = signatures.flatMap((signature, round) => signature !== baseline ? [roundNumbers[round]] : []);
  const count = (messagesByGeneration.get(segment.generations[0]) || []).filter((message) => message.role === "system").length;
  const label = `${segment.agentLabel} [${segment.agentObservationId.slice(0, 8)}]`;
  const status = changedRounds.length ? `Changed in round ${changedRounds.join(",")}` : "Stable";
  console.log(`| ${index + 1} | ${label} | ${roundNumbers.join(",")} | ${count} | ${changedRounds.length} | ${status} |`);
}
