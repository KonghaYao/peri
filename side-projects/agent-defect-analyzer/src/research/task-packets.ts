import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { DataLoader, DEFAULT_DB_PATH, NORMALIZER_VERSION } from "../data/loader.js";
import { validateThreadFilter, type ValidatedThreadFilter } from "../data/filters.js";
import { projectExecution, type NormalizedMessage, type ThreadSummary, type ToolCall, type ToolResult } from "../data/types.js";

export const TASK_PACKET_SCHEMA_VERSION = 2 as const;
export const TASK_PACKET_RUBRIC_VERSION = "task-effectiveness-v1" as const;
export const TASK_PACKET_MAX_BYTES = 128 * 1024;
export const TASK_PACKET_MAX_MESSAGES = 160;
const MAX_SINGLE_PACKET_BYTES = 1024 * 1024;
const MAX_SINGLE_PACKET_MESSAGES = 1000;

export type TaskStratum = "short" | "medium" | "long";
export interface TaskPacketMessage {
  messageId: string; threadId: string; sequence: number; role: string; origin: "own" | "inherited";
  calls: Array<{ id: string; name: string; source: string; arguments?: unknown; argumentsTruncated?: boolean }>;
  results: Array<{
    id: string; source: string; isError: boolean | null;
    execution: {
      status: ToolResult["execution"]["status"]; exitCode: number | null; hasOutputRef: boolean;
      outputTruncated: boolean; hasTaskId: boolean; source: ToolResult["execution"]["source"];
      taskId?: string;
      /** Local references are exported only after the caller explicitly opts into content. */
      outputRef?: string;
    };
    content?: string; contentTruncated?: boolean;
  }>;
  truncated: boolean; isSummary: boolean; parseIssues: string[];
  text?: string; exportTruncated?: boolean; omittedFields?: string[];
}
export interface TaskPacketCoverage {
  sourceMessageCount: number; exportedMessageCount: number; omittedMessages: number;
  omittedFields: string[]; sourceTruncatedMessages: number; exportTruncated: boolean;
  /** Compatibility summary consumed by reviewers; source/export details remain explicit above. */
  truncated: boolean; includeContent: boolean; byteCount: number;
}
export interface TaskPacket {
  caseId: string; threadId: string; packetHash: string; ownMessageCount: number;
  parentThreadId: string | null; childThreadIds: string[]; messages: TaskPacketMessage[];
  coverage: TaskPacketCoverage;
  thread?: { createdAt: string; updatedAt: string; title: string | null; messageCount: number; hidden: boolean };
}
export interface TaskPacketBundle {
  schemaVersion: 2; rubricVersion: "task-effectiveness-v1";
  sampling: {
    seed: string; perStratum: number; scope: "roots" | "children" | "all"; includeHidden: boolean;
    since: string | null; until: string | null;
    strata: Record<TaskStratum, { range: string; candidateCount: number; selectedCount: number; candidateIds: string[]; selectedIds: string[]; candidates: Array<{ threadId: string; createdAt: string; ownMessageCount: number; parentThreadId: string | null; hidden: boolean }>; selected: Array<{ threadId: string; createdAt: string; ownMessageCount: number; parentThreadId: string | null; hidden: boolean }>; rangeHash: string }>;
    noOwnMessages: number; source: { normalizer: string; schemaIdentity: string; snapshot: "readonly-transaction" };
  };
  packets: TaskPacket[];
}
export interface TaskPacketOptions { includeContent?: boolean; maxBytes?: number; maxMessages?: number; }
export interface TaskSampleOptions {
  seed: string | number; perStratum?: number; scope?: "roots" | "children" | "all"; includeHidden?: boolean; since?: string; until?: string; includeContent?: boolean;
}
export class TaskPacketInputError extends Error {}
export class TaskPacketNotFoundError extends Error {}

const stable = (value: unknown): string => JSON.stringify(value, (_key, item) => item && typeof item === "object" && !Array.isArray(item) ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b))) : item);
const hash = (value: unknown): string => createHash("sha256").update(stable(value)).digest("hex");
const bytes = (value: unknown): number => Buffer.byteLength(JSON.stringify(value), "utf8");
const truncate = (value: string, max: number): { value: string; truncated: boolean } => {
  if (Buffer.byteLength(value, "utf8") <= max) return { value, truncated: false };
  let result = "", used = 0;
  for (const ch of value) { const size = Buffer.byteLength(ch, "utf8"); if (used + size > max) break; result += ch; used += size; }
  return { value: result, truncated: true };
};
function stratum(count: number): TaskStratum | "none" { return count < 1 ? "none" : count <= 20 ? "short" : count <= 100 ? "medium" : "long"; }
function seedNumber(seed: string): number { let n = 2166136261; for (const b of new TextEncoder().encode(seed)) n = Math.imul(n ^ b, 16777619); return n >>> 0; }
function shuffle<T>(items: T[], seed: string): T[] { let state = seedNumber(seed) || 1; const out = [...items]; for (let i = out.length - 1; i > 0; i--) { state = Math.imul(state ^ (state >>> 16), 2246822519) >>> 0; state = Math.imul(state ^ (state >>> 13), 3266489917) >>> 0; const j = state % (i + 1); [out[i], out[j]] = [out[j], out[i]]; } return out; }
function callRecord(call: ToolCall, includeContent: boolean): TaskPacketMessage["calls"][number] { return { id: call.id, name: call.name, source: call.sources.join(","), ...(includeContent ? { arguments: call.arguments } : {}) }; }
function resultRecord(result: ToolResult, includeContent: boolean): TaskPacketMessage["results"][number] {
  const execution = projectExecution(result.execution, includeContent) as TaskPacketMessage["results"][number]["execution"];
  return { id: result.id, source: result.sources.join(","), isError: result.isError, execution, ...(includeContent ? { content: result.content } : {}) };
}
function messageRecord(message: NormalizedMessage, includeContent: boolean): TaskPacketMessage {
  const record: TaskPacketMessage = { messageId: message.messageId, threadId: message.threadId, sequence: message.sequence, role: message.role, origin: message.origin, calls: message.calls.map((c) => callRecord(c, includeContent)), results: message.results.map((r) => resultRecord(r, includeContent)), truncated: message.truncated, isSummary: message.isSummary, parseIssues: [...message.parseIssues] };
  if (includeContent) record.text = message.text;
  return record;
}
function packetHashInput(packet: TaskPacket): unknown { const { packetHash: _hash, ...withoutHash } = packet; return withoutHash; }
/** Canonical hash of the exported packet facts, excluding the self-referential hash. */
export function computeTaskPacketHash(packet: TaskPacket): string { return hash(packetHashInput(packet)); }

function boundPacket(packet: TaskPacket, maxBytes: number, maxMessages: number): TaskPacket {
  const omissions = new Set(packet.coverage.omittedFields);
  const mark = (field: string) => { omissions.add(field); packet.coverage.omittedFields = [...omissions].sort(); packet.coverage.exportTruncated = true; packet.coverage.truncated = true; };
  while (packet.messages.length > maxMessages) { packet.messages.splice(Math.floor(packet.messages.length / 2), 1); packet.coverage.omittedMessages++; mark("messages"); }
  // Shrink content fields before dropping transcript records. This preserves the
  // anchors and surrounding user/assistant evidence even for a giant tool result.
  while (bytes(packet) > maxBytes) {
    let candidate: { kind: "text" | "arguments" | "result"; message: TaskPacketMessage; call?: TaskPacketMessage["calls"][number]; result?: TaskPacketMessage["results"][number]; value: string } | undefined;
    for (const message of packet.messages) {
      if (message.text && message.text.length > (candidate?.value.length ?? 0)) candidate = { kind: "text", message, value: message.text };
      const call = message.calls.find((item) => typeof item.arguments === "string" && item.arguments.length > (candidate?.value.length ?? 0));
      if (call) candidate = { kind: "arguments", message, call, value: call.arguments as string };
      const result = message.results.find((item) => item.content && item.content.length > (candidate?.value.length ?? 0));
      if (result) candidate = { kind: "result", message, result, value: result.content as string };
    }
    if (!candidate || candidate.value.length < 32) break;
    const shortened = truncate(candidate.value, Math.max(16, Math.floor(Buffer.byteLength(candidate.value, "utf8") / 2)));
    if (candidate.kind === "text") { candidate.message.text = shortened.value; candidate.message.exportTruncated = true; mark("text"); }
    else if (candidate.kind === "arguments" && candidate.call) { candidate.call.arguments = shortened.value; candidate.call.argumentsTruncated = true; mark("call.arguments"); }
    else if (candidate.kind === "result" && candidate.result) { candidate.result.content = shortened.value; candidate.result.contentTruncated = true; mark("result.content"); }
  }
  while (bytes(packet) > maxBytes && packet.messages.length > 2) { packet.messages.splice(Math.floor(packet.messages.length / 2), 1); packet.coverage.omittedMessages++; mark("messages"); }
  while (bytes(packet) > maxBytes) {
    const message = packet.messages.find((m) => m.text !== undefined || m.calls.some((c) => c.arguments !== undefined) || m.results.some((r) => r.content !== undefined));
    if (!message) break;
    if (message.text !== undefined) { delete message.text; message.exportTruncated = true; mark("text"); continue; }
    const call = message.calls.find((c) => c.arguments !== undefined); if (call) { delete call.arguments; call.argumentsTruncated = true; mark("call.arguments"); continue; }
    const result = message.results.find((r) => r.content !== undefined); if (result) { delete result.content; result.contentTruncated = true; mark("result.content"); continue; }
  }
  if (bytes(packet) > maxBytes) {
    for (const message of packet.messages) { if (message.parseIssues.length) { message.parseIssues = []; mark("parseIssues"); } }
  }
  if (bytes(packet) > maxBytes) throw new TaskPacketInputError(`task packet exceeds ${maxBytes} bytes`);
  packet.coverage.exportedMessageCount = packet.messages.length;
  // byteCount is itself part of the encoded fact, so settle it before the final
  // size check and account for the hash field as well.
  packet.coverage.byteCount = 0;
  packet.packetHash = computeTaskPacketHash(packet);
  for (let i = 0; i < 4; i++) { packet.coverage.byteCount = bytes(packet); packet.packetHash = computeTaskPacketHash(packet); }
  while (bytes(packet) > maxBytes && packet.messages.length > 0) { packet.messages.splice(Math.floor(packet.messages.length / 2), 1); packet.coverage.omittedMessages++; mark("messages"); packet.coverage.exportedMessageCount = packet.messages.length; packet.coverage.byteCount = 0; packet.packetHash = computeTaskPacketHash(packet); for (let i = 0; i < 4; i++) { packet.coverage.byteCount = bytes(packet); packet.packetHash = computeTaskPacketHash(packet); } }
  if (bytes(packet) > maxBytes) throw new TaskPacketInputError(`task packet exceeds ${maxBytes} bytes`);
  return packet;
}

function packetFromLoader(loader: DataLoader, thread: ThreadSummary, options: TaskPacketOptions): TaskPacket {
  const includeContent = options.includeContent ?? false;
  const messages = [...loader.loadInheritedMessages(thread.id), ...loader.loadNormalizedMessages(thread.id)];
  const ids = new Set<string>(); for (const message of messages) { if (ids.has(message.messageId)) throw new TaskPacketInputError(`duplicate message id in thread ${thread.id}: ${message.messageId}`); ids.add(message.messageId); }
  const own = messages.filter((m) => m.origin === "own");
  const childThreadIds = loader.loadSubAgents(thread.id).map((child) => child.id).sort();
  const sourceTruncatedMessages = messages.filter((m) => m.truncated).length;
  const packet: TaskPacket = { caseId: thread.id, threadId: thread.id, packetHash: "", ownMessageCount: own.length, parentThreadId: thread.parent_thread_id, childThreadIds, messages: messages.map((m) => messageRecord(m, includeContent)), coverage: { sourceMessageCount: messages.length, exportedMessageCount: messages.length, omittedMessages: 0, omittedFields: [], sourceTruncatedMessages, exportTruncated: false, truncated: sourceTruncatedMessages > 0, includeContent, byteCount: 0 }, thread: { createdAt: thread.created_at, updatedAt: thread.updated_at, title: thread.title, messageCount: thread.message_count, hidden: thread.hidden !== 0 } };
  return boundPacket(packet, options.maxBytes ?? TASK_PACKET_MAX_BYTES, options.maxMessages ?? TASK_PACKET_MAX_MESSAGES);
}

function validateOptions(input: TaskSampleOptions): { seed: string; perStratum: number; filter: ValidatedThreadFilter } {
  const perStratum = input.perStratum ?? 4;
  if (!Number.isInteger(perStratum) || perStratum < 1 || perStratum > 10) throw new TaskPacketInputError("perStratum must be an integer between 1 and 10");
  try { return { seed: String(input.seed), perStratum, filter: validateThreadFilter(input) }; } catch (error) { throw new TaskPacketInputError(error instanceof Error ? error.message : String(error)); }
}

export function exportTaskPacket(dbPath: string = DEFAULT_DB_PATH, threadId: string, options: TaskPacketOptions = {}): TaskPacket {
  if (!threadId) throw new TaskPacketInputError("threadId is required");
  if (options.maxBytes !== undefined && (!Number.isInteger(options.maxBytes) || options.maxBytes < 1 || options.maxBytes > MAX_SINGLE_PACKET_BYTES)) throw new TaskPacketInputError(`maxBytes must be an integer between 1 and ${MAX_SINGLE_PACKET_BYTES}`);
  if (options.maxMessages !== undefined && (!Number.isInteger(options.maxMessages) || options.maxMessages < 1 || options.maxMessages > MAX_SINGLE_PACKET_MESSAGES)) throw new TaskPacketInputError(`maxMessages must be an integer between 1 and ${MAX_SINGLE_PACKET_MESSAGES}`);
  const loader = new DataLoader(dbPath);
  try { return loader.withSnapshot((db) => { const thread = db.loadThreadsByIds([threadId])[0]; if (!thread) throw new TaskPacketNotFoundError(`thread not found: ${threadId}`); const maxBytes = Math.min(options.maxBytes ?? TASK_PACKET_MAX_BYTES, MAX_SINGLE_PACKET_BYTES); const maxMessages = Math.min(options.maxMessages ?? TASK_PACKET_MAX_MESSAGES, MAX_SINGLE_PACKET_MESSAGES); return packetFromLoader(db, thread, { ...options, maxBytes, maxMessages }); }).value; } finally { loader.close(); }
}

export function sampleTaskPackets(dbPath: string = DEFAULT_DB_PATH, input: TaskSampleOptions): TaskPacketBundle {
  const validated = validateOptions(input); const loader = new DataLoader(dbPath);
  try { return loader.withSnapshot((db) => {
    const threads = (validated.filter.scope === "roots" ? (validated.filter.includeHidden ? db.loadAllMainThreads() : db.loadVisibleMainThreads()) : validated.filter.scope === "children" ? db.loadAllSubAgents() : (validated.filter.includeHidden ? db.loadAllThreads() : db.loadVisibleThreads())).filter((t) => validated.filter.includeHidden || t.hidden === 0).filter((t) => { const time = Date.parse(t.created_at); if (!Number.isFinite(time)) throw new TaskPacketInputError(`thread ${t.id} has invalid created_at`); return (validated.filter.since === null || time >= validated.filter.since) && (validated.filter.until === null || time < validated.filter.until); });
    const byStratum: Record<TaskStratum, ThreadSummary[]> = { short: [], medium: [], long: [] }; const ownCounts = new Map<string, number>(); let noOwnMessages = 0;
    for (const thread of threads) { const ownCount = db.loadNormalizedMessages(thread.id).length; ownCounts.set(thread.id, ownCount); const bucket = stratum(ownCount); if (bucket === "none") noOwnMessages++; else byStratum[bucket].push(thread); }
    const strata = {} as TaskPacketBundle["sampling"]["strata"]; const selected: ThreadSummary[] = [];
    for (const name of ["short", "medium", "long"] as TaskStratum[]) { const candidates = byStratum[name].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id)); const picked = shuffle(candidates, `${validated.seed}:${name}`).slice(0, validated.perStratum); selected.push(...picked); const metadata = (items: ThreadSummary[]) => items.map((t) => ({ threadId: t.id, createdAt: t.created_at, ownMessageCount: ownCounts.get(t.id) ?? 0, parentThreadId: t.parent_thread_id, hidden: t.hidden !== 0 })); strata[name] = { range: name === "short" ? "1..20" : name === "medium" ? "21..100" : ">100", candidateCount: candidates.length, selectedCount: picked.length, candidateIds: candidates.map((t) => t.id), selectedIds: picked.map((t) => t.id), candidates: metadata(candidates), selected: metadata(picked), rangeHash: hash(metadata(candidates)) }; }
    const packets = selected.map((thread) => packetFromLoader(db, thread, { includeContent: input.includeContent }));
    return { schemaVersion: TASK_PACKET_SCHEMA_VERSION, rubricVersion: TASK_PACKET_RUBRIC_VERSION, sampling: { seed: validated.seed, perStratum: validated.perStratum, scope: validated.filter.scope, includeHidden: validated.filter.includeHidden, since: validated.filter.sinceText, until: validated.filter.untilText, strata, noOwnMessages, source: { normalizer: NORMALIZER_VERSION, schemaIdentity: hash(loader.capabilities), snapshot: "readonly-transaction" } }, packets } as TaskPacketBundle;
  }).value; } finally { loader.close(); }
}

export function writeTaskPacketBundle(bundle: TaskPacketBundle, outDir: string): void { mkdirSync(outDir, { recursive: true }); writeFileSync(`${outDir}/task-packets.json`, JSON.stringify(bundle, null, 2)); }
