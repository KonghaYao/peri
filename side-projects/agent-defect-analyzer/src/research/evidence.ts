import { createHash } from "crypto";
import { DataLoader, DEFAULT_DB_PATH, NORMALIZER_VERSION } from "../data/loader.js";
import { validateThreadFilter, type ValidatedThreadFilter } from "../data/filters.js";
import { projectExecution, type ExecutionProjection, type NormalizedMessage, type ThreadSummary, type ToolCall, type ToolResult } from "../data/types.js";

const MAX_RADIUS = 20;
const MAX_CONTENT_BYTES = 64 * 1024;
const MAX_FIELD_BYTES = 16 * 1024;

export type EvidenceScope = "roots" | "children" | "all";
export interface SampleOptions {
  seed: string | number;
  size: number;
  minMessages?: number;
  scope?: EvidenceScope;
  includeHidden?: boolean;
  since?: string;
  until?: string;
}
export interface SampleThread { threadId: string; createdAt: string; messageCount: number; }
export interface SampleResult {
  seed: string;
  filter: { minMessages: number; scope: EvidenceScope; includeHidden: boolean; since: string | null; until: string | null };
  eligibleCount: number;
  samples: SampleThread[];
  source: SourceIdentity;
}
export interface EvidenceOptions { threadId: string; messageId: string; radius?: number; includeContent?: boolean; }
export interface Fingerprint { algorithm: "sha256"; scope: "sample-candidate-and-selected-metadata" | "evidence-window-metadata"; value: string; }
export interface SourceIdentity {
  snapshot: "readonly-transaction";
  normalizer: string;
  schema: { userVersion: number; identity: string };
  fingerprint: Fingerprint;
}
export interface ToolCallEvidence { id: string; name: string; source: string; arguments?: string; argumentsTruncated?: boolean; }
export interface ToolResultEvidence { id: string; source: string; isError: boolean | null; execution: ExecutionProjection; content?: string; contentTruncated?: boolean; }
export interface EvidenceRecord {
  messageId: string; threadId: string; sequence: number; role: string; origin: string;
  excludedFromContext: boolean; truncated: boolean; isSummary: boolean; parseIssues: string[];
  calls: ToolCallEvidence[]; results: ToolResultEvidence[];
  content?: { text: string; textTruncated: boolean; calls: ToolCallEvidence[]; results: ToolResultEvidence[] };
}
export interface EvidenceOmissions { records: number; calls: number; results: number; fields: number; }
export interface MessageEvidence {
  source: SourceIdentity;
  threadId: string;
  targetMessageId: string;
  radius: number;
  includeContent: boolean;
  truncated: boolean;
  omissions: EvidenceOmissions;
  records: EvidenceRecord[];
}

export class EvidenceInputError extends Error {}
export class EvidenceNotFoundError extends Error {}

function validateSampling(input: SampleOptions): { seed: string; size: number; minMessages: number; filter: ValidatedThreadFilter } {
  if (!Number.isInteger(input.size) || input.size < 1 || input.size > 50) throw new EvidenceInputError("size must be an integer between 1 and 50");
  const minMessages = input.minMessages ?? 0;
  if (!Number.isInteger(minMessages) || minMessages < 0) throw new EvidenceInputError("minMessages must be a non-negative integer");
  try {
    return { seed: String(input.seed), size: input.size, minMessages, filter: validateThreadFilter(input) };
  } catch (error) {
    if (error instanceof RangeError) throw new EvidenceInputError(error.message);
    throw error;
  }
}

function validateRadius(value: number | undefined): number {
  const radius = value ?? 2;
  if (!Number.isInteger(radius) || radius < 0 || radius > MAX_RADIUS) throw new EvidenceInputError("radius must be an integer between 0 and 20");
  return radius;
}

function matches(thread: ThreadSummary, filter: ValidatedThreadFilter, minMessages: number): boolean {
  const created = Date.parse(thread.created_at);
  if (!Number.isFinite(created)) throw new EvidenceInputError(`thread ${thread.id} has invalid created_at`);
  if (!filter.includeHidden && thread.hidden !== 0) return false;
  if (filter.scope === "roots" && thread.parent_thread_id !== null) return false;
  if (filter.scope === "children" && thread.parent_thread_id === null) return false;
  if (thread.message_count < minMessages) return false;
  return (filter.since === null || created >= filter.since) && (filter.until === null || created < filter.until);
}

function seedNumber(seed: string): number {
  let value = 2166136261;
  for (const byte of new TextEncoder().encode(seed)) value = Math.imul(value ^ byte, 16777619);
  return value >>> 0;
}

function shuffle<T>(items: T[], seed: string): T[] {
  let state = seedNumber(seed) || 1;
  const result = [...items];
  for (let i = result.length - 1; i > 0; i--) {
    state = Math.imul(state ^ (state >>> 16), 2246822519) >>> 0;
    state = Math.imul(state ^ (state >>> 13), 3266489917) >>> 0;
    const j = state % (i + 1);
    [result[i], result[j]] = [result[j], result[i]];
  }
  return result;
}

function schemaIdentity(loader: DataLoader): string {
  return createHash("sha256").update(JSON.stringify(loader.capabilities)).digest("hex");
}

function source(loader: DataLoader, scope: Fingerprint["scope"], metadata: unknown): SourceIdentity {
  const value = createHash("sha256").update(JSON.stringify({ normalizer: NORMALIZER_VERSION, scope, metadata })).digest("hex");
  return {
    snapshot: "readonly-transaction",
    normalizer: NORMALIZER_VERSION,
    schema: { userVersion: loader.capabilities.userVersion, identity: schemaIdentity(loader) },
    fingerprint: { algorithm: "sha256", scope, value },
  };
}

function threadMetadata(thread: ThreadSummary): unknown {
  return { id: thread.id, createdAt: thread.created_at, messageCount: thread.message_count, parentThreadId: thread.parent_thread_id, hidden: thread.hidden };
}

function sourceFields(call: ToolCall): ToolCallEvidence { return { id: call.id, name: call.name, source: call.sources.join(",") }; }
function resultFields(result: ToolResult, includeContent = false): ToolResultEvidence { return { id: result.id, source: result.sources.join(","), isError: result.isError, execution: projectExecution(result.execution, includeContent) }; }

/** Truncate on Unicode code-point boundaries, so output never exceeds maxBytes. */
function truncateUtf8(value: string, maxBytes: number): { value: string; truncated: boolean } {
  if (Buffer.byteLength(value, "utf8") <= maxBytes) return { value, truncated: false };
  let output = "";
  let used = 0;
  for (const character of value) {
    const bytes = Buffer.byteLength(character, "utf8");
    if (used + bytes > maxBytes) break;
    output += character;
    used += bytes;
  }
  return { value: output, truncated: true };
}

function recordOf(message: NormalizedMessage, includeContent: boolean): EvidenceRecord {
  const calls = message.calls.map(sourceFields);
  const results = message.results.map((result) => resultFields(result));
  const record: EvidenceRecord = { messageId: message.messageId, threadId: message.threadId, sequence: message.sequence, role: message.role, origin: message.origin, excludedFromContext: message.excludedFromContext, truncated: message.truncated, isSummary: message.isSummary, parseIssues: [...message.parseIssues], calls, results };
  if (includeContent) {
    const text = truncateUtf8(message.text, MAX_FIELD_BYTES);
    record.content = {
      text: text.value,
      textTruncated: text.truncated,
      calls: message.calls.map((call) => {
        const encoded = JSON.stringify(call.arguments) ?? "null";
        const argumentsValue = truncateUtf8(encoded, MAX_FIELD_BYTES);
        return { ...sourceFields(call), arguments: argumentsValue.value, argumentsTruncated: argumentsValue.truncated };
      }),
      results: message.results.map((result) => {
        const content = truncateUtf8(result.content, MAX_FIELD_BYTES);
        return { ...resultFields(result, true), content: content.value, contentTruncated: content.truncated };
      }),
    };
  }
  return record;
}

function messageMetadata(message: NormalizedMessage): unknown {
  return {
    id: message.messageId,
    threadId: message.threadId,
    sequence: message.sequence,
    role: message.role,
    origin: message.origin,
    excludedFromContext: message.excludedFromContext,
    truncated: message.truncated,
    isSummary: message.isSummary,
    parseIssues: message.parseIssues,
    calls: message.calls.map((call) => sourceFields(call)),
    results: message.results.map((result) => resultFields(result)),
  };
}

function contentEntryCount(record: EvidenceRecord): number {
  return record.content ? 1 + record.content.calls.length + record.content.results.length : 0;
}

function dropContent(record: EvidenceRecord, omissions: EvidenceOmissions): void {
  if (!record.content) return;
  omissions.fields += 1;
  omissions.calls += record.content.calls.length;
  omissions.results += record.content.results.length;
  delete record.content;
}

function encodedSize(result: MessageEvidence): number { return Buffer.byteLength(JSON.stringify(result, null, 2), "utf8"); }

/** Keep the target identity, and report every budget-driven omission. */
function boundResult(result: MessageEvidence): MessageEvidence {
  const omissions = result.omissions;
  const targetIndex = (): number => result.records.findIndex((record) => record.messageId === result.targetMessageId);
  const dropRecord = (index: number): void => {
    const [record] = result.records.splice(index, 1);
    if (!record) return;
    omissions.records++;
    omissions.calls += record.calls.length;
    omissions.results += record.results.length;
    omissions.fields += contentEntryCount(record);
  };

  while (encodedSize(result) > MAX_CONTENT_BYTES) {
    const target = targetIndex();
    const nonTargetContent = result.records.find((record, index) => index !== target && record.content);
    if (nonTargetContent) {
      dropContent(nonTargetContent, omissions);
    } else if (result.records.length > 1) {
      const last = result.records.length - 1;
      dropRecord(last === target ? 0 : last);
    } else {
      break;
    }
  }

  const trimMetadata = (): boolean => {
    const candidate = result.records
      .map((record, index) => ({ record, index, count: record.calls.length + record.results.length }))
      .filter((entry) => entry.count > 0)
      .sort((left, right) => right.count - left.count)[0];
    if (!candidate) return false;
    if (candidate.record.calls.length >= candidate.record.results.length && candidate.record.calls.length > 0) {
      const removed = Math.max(1, Math.ceil(candidate.record.calls.length / 4));
      candidate.record.calls.splice(-removed, removed);
      omissions.calls += removed;
    } else if (candidate.record.results.length > 0) {
      const removed = Math.max(1, Math.ceil(candidate.record.results.length / 4));
      candidate.record.results.splice(-removed, removed);
      omissions.results += removed;
    }
    return true;
  };
  while (encodedSize(result) > MAX_CONTENT_BYTES && trimMetadata()) {}

  const target = result.records.find((record) => record.messageId === result.targetMessageId);
  if (encodedSize(result) > MAX_CONTENT_BYTES && target?.content) {
    omissions.fields += 1;
    target.content.text = "";
    target.content.textTruncated = true;
    omissions.calls += target.content.calls.length;
    omissions.results += target.content.results.length;
    target.content.calls = [];
    target.content.results = [];
  }
  while (encodedSize(result) > MAX_CONTENT_BYTES && target && target.parseIssues.length > 0) {
    target.parseIssues.pop();
    omissions.fields++;
  }
  if (encodedSize(result) > MAX_CONTENT_BYTES && target?.content) {
    const available = Math.max(0, MAX_CONTENT_BYTES - (encodedSize(result) - Buffer.byteLength(JSON.stringify(target.content.text), "utf8")) - 32);
    target.content.text = truncateUtf8(target.content.text, available).value;
    target.content.textTruncated = true;
  }
  if (encodedSize(result) > MAX_CONTENT_BYTES) throw new EvidenceInputError("evidence exceeds the bounded byte budget");
  result.truncated = result.truncated || omissions.records > 0 || omissions.calls > 0 || omissions.results > 0 || omissions.fields > 0;
  return result;
}

export function sampleThreads(dbPath: string = DEFAULT_DB_PATH, input: SampleOptions): SampleResult {
  const validated = validateSampling(input);
  const loader = new DataLoader(dbPath);
  try {
    return loader.withSnapshot((db) => {
      const eligible = db.loadThreadSummaries()
        .filter((thread) => matches(thread, validated.filter, validated.minMessages))
        .sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id));
      const selected = shuffle(eligible, validated.seed).slice(0, validated.size);
      const filter = { minMessages: validated.minMessages, scope: validated.filter.scope, includeHidden: validated.filter.includeHidden, since: validated.filter.sinceText, until: validated.filter.untilText };
      const metadata = { filter, eligible: eligible.map(threadMetadata), selected: selected.map(threadMetadata) };
      return {
        seed: validated.seed,
        filter,
        eligibleCount: eligible.length,
        samples: selected.map((thread) => ({ threadId: thread.id, createdAt: thread.created_at, messageCount: thread.message_count })),
        source: source(loader, "sample-candidate-and-selected-metadata", metadata),
      };
    }).value;
  } finally { loader.close(); }
}

export function evidenceForMessage(dbPath: string = DEFAULT_DB_PATH, input: EvidenceOptions): MessageEvidence {
  if (!input.threadId || !input.messageId) throw new EvidenceInputError("threadId and messageId are required");
  const radius = validateRadius(input.radius);
  const includeContent = input.includeContent ?? false;
  if (typeof includeContent !== "boolean") throw new EvidenceInputError("includeContent must be boolean");
  const loader = new DataLoader(dbPath);
  try {
    return loader.withSnapshot((db) => {
      if (!db.hasThread(input.threadId)) throw new EvidenceNotFoundError(`thread not found: ${input.threadId}`);
      const messages = db.loadNormalizedMessageWindow(input.threadId, input.messageId, radius);
      if (!messages) throw new EvidenceNotFoundError(`message not found: ${input.messageId}`);
      const records = messages.map((message) => recordOf(message, includeContent));
      const fieldTruncated = includeContent && messages.some((message) => {
        const argumentsTruncated = message.calls.some((call) => Buffer.byteLength(JSON.stringify(call.arguments) ?? "null", "utf8") > MAX_FIELD_BYTES);
        return Buffer.byteLength(message.text, "utf8") > MAX_FIELD_BYTES || argumentsTruncated || message.results.some((result) => Buffer.byteLength(result.content, "utf8") > MAX_FIELD_BYTES);
      });
      return boundResult({
        source: source(loader, "evidence-window-metadata", messages.map(messageMetadata)),
        threadId: input.threadId,
        targetMessageId: input.messageId,
        radius,
        includeContent,
        truncated: fieldTruncated,
        omissions: { records: 0, calls: 0, results: 0, fields: 0 },
        records,
      });
    }).value;
  } finally { loader.close(); }
}
