import { createHash } from "crypto";
import { DataLoader, DEFAULT_DB_PATH, NORMALIZER_VERSION } from "../data/loader.js";
import { validateThreadFilter } from "../data/filters.js";
import type { NormalizedMessage, ThreadSummary, ToolCall, ToolExecutionStatus } from "../data/types.js";

export const METRICS_VERSION = "agent-metrics-v2";

export type AnalysisScope = "roots" | "children" | "all";
export interface AnalysisOptions { scope?: AnalysisScope; includeHidden?: boolean; since?: string; until?: string; }
export interface EvidenceEntry { threadId: string; messageId: string; callId: string; }
export interface Evidence { entries: EvidenceEntry[]; }
export interface Candidate {
  ruleId: string;
  evidenceKind: "observation" | "candidate";
  counts: number;
  denominator: number;
  evidence: Evidence;
  nextVerification: string;
}
export interface ToolMetric {
  label: string;
  outerName: string;
  effectiveName: string;
  calls: number;
  pairedResults: number;
  pairedKnownResults: number;
  errors: number;
  unknownErrors: number;
  calledThreads: number;
  errorAffectedThreads: number;
  errorRate: number | null;
}
export interface AnalysisReport {
  version: { normalizer: string; metrics: string };
  source: { fingerprint: string };
  filters: { scope: AnalysisScope; includeHidden: boolean; since: string | null; until: string | null };
  totals: {
    threads: number; messages: number; calls: number; pairedResults: number; pairedKnownResults: number;
    missingResults: number; orphanResults: number; duplicateCallIds: number;
    duplicateResultIds: number; unknownErrorResults: number; explicitErrors: number;
    parseIssues: number;
    execution?: {
      resultCount: number; typedCount: number; knownCount: number; coverage: number | null;
      statusCounts: Record<ToolExecutionStatus, number>; outputRefCount: number; outputTruncatedCount: number;
    };
  };
  tools: Record<string, ToolMetric>;
  resultBytes: { count: number; giantCount: number; p50: number | null; p95: number | null; errorBytes: number };
  parseIssues: Record<string, number>;
  candidates: Candidate[];
}

interface CallRecord { call: ToolCall; message: NormalizedMessage; label: string; effectiveName: string; }
interface MutableMetric extends ToolMetric { called: Set<string>; errored: Set<string>; }

function stable(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${stable(object[key])}`).join(",")}}`;
}
function effectiveName(call: ToolCall): { outer: string; effective: string } {
  if (call.name === "ExecuteExtraTool" && call.arguments && typeof call.arguments === "object" && !Array.isArray(call.arguments)) {
    const target = (call.arguments as Record<string, unknown>).tool_name;
    if (typeof target === "string" && target.length > 0) return { outer: call.name, effective: target };
  }
  return { outer: call.name, effective: call.name };
}
function labelOf(call: ToolCall): { label: string; outer: string; effective: string } {
  const names = effectiveName(call);
  return { ...names, label: names.outer === names.effective ? names.effective : `${names.outer}→${names.effective}` };
}
function bytes(text: string): number { return Buffer.byteLength(text, "utf8"); }
function quantile(values: number[], fraction: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * fraction))] ?? 0;
}
function issueCategory(issue: string): string {
  const separator = issue.indexOf(":");
  return separator < 0 ? issue : issue.slice(0, separator);
}
function selectedThreads(rows: ThreadSummary[], options: Required<Pick<AnalysisOptions, "scope" | "includeHidden">> & { since: number | null; until: number | null }): ThreadSummary[] {
  return rows.filter((thread) => {
    const created = Date.parse(thread.created_at);
    if (!Number.isFinite(created)) throw new RangeError(`thread ${thread.id} has invalid created_at`);
    if (!options.includeHidden && thread.hidden !== 0) return false;
    if (options.scope === "roots" && thread.parent_thread_id !== null) return false;
    if (options.scope === "children" && thread.parent_thread_id === null) return false;
    return (options.since === null || created >= options.since) && (options.until === null || created < options.until);
  });
}
function evidence(entries: EvidenceEntry[]): Evidence {
  return { entries: entries.slice(0, 8) };
}

function emptyExecutionStatusCounts(): Record<ToolExecutionStatus, number> {
  return { unknown: 0, completed: 0, failed: 0, cancelled: 0, timed_out: 0, running: 0, running_after_timeout: 0 };
}

/** Analyze one immutable SQLite snapshot. This function never writes the source DB. */
export function analyzeDatabase(dbPath: string = DEFAULT_DB_PATH, input: AnalysisOptions = {}): AnalysisReport {
  const validated = validateThreadFilter(input);
  const scope = validated.scope;
  const includeHidden = validated.includeHidden;
  const since = validated.since;
  const until = validated.until;
  const loader = new DataLoader(dbPath);
  try {
    const snapshot = loader.withSnapshot((db) => {
      const threads = selectedThreads(db.loadThreadSummaries(), { scope, includeHidden, since, until });
      const fingerprintHash = createHash("sha256");
      fingerprintHash.update(JSON.stringify({ schema: loader.capabilities, scope, includeHidden, since: validated.sinceText, until: validated.untilText }));
      for (const thread of threads) fingerprintHash.update(`${thread.id}\0${thread.created_at}\0${thread.hidden}\0${thread.parent_thread_id ?? ""}`);
      const metrics = new Map<string, MutableMetric>();
      const pending = new Map<string, CallRecord>();
      const seenCalls = new Set<string>();
      const seenResults = new Set<string>();
      const parsedIssues: Record<string, number> = Object.create(null) as Record<string, number>;
      const resultSizes: number[] = [];
      const errorResultSizes: number[] = [];
      const giantEntries: EvidenceEntry[] = [];
      const candidates = new Map<string, Candidate>();
      let messages = 0, calls = 0, pairedResults = 0, pairedKnownResults = 0, missingResults = 0;
      let orphanResults = 0, duplicateCallIds = 0, duplicateResultIds = 0;
      let unknownErrorResults = 0, explicitErrors = 0;
      let executionResultCount = 0, typedExecutionCount = 0, knownExecutionCount = 0;
      let executionOutputRefCount = 0, executionOutputTruncatedCount = 0;
      const executionStatusCounts = emptyExecutionStatusCounts();
      let repeatKey: string | null = null;
      let repeatCount = 0;
      let failureStreak: { label: string; count: number; threadId: string; messageId: string; callId: string } | null = null;
      const nextVerification = (ruleId: string): string => ruleId === "repeated-call"
        ? "Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition."
        : ruleId === "explicit-failure"
          ? "Inspect both failed results and the next user or assistant message before attributing a strategy defect."
          : "Inspect the cited result in its surrounding conversation before drawing a conclusion.";
      const addCandidate = (ruleId: string, threadId: string, messageId: string, callId: string, denominator: number) => {
        const current = candidates.get(ruleId);
        const entry = { threadId, messageId, callId };
        if (current) {
          current.counts++;
          if (current.evidence.entries.length < 8) current.evidence.entries.push(entry);
        } else {
          candidates.set(ruleId, { ruleId, evidenceKind: "candidate", counts: 1, denominator, evidence: evidence([entry]), nextVerification: nextVerification(ruleId) });
        }
      };
      for (const thread of threads) {
        repeatKey = null;
        repeatCount = 0;
        failureStreak = null;
        seenCalls.clear();
        seenResults.clear();
        for (const message of db.iterateNormalizedMessages(thread.id)) {
          messages++;
          fingerprintHash.update(`${message.messageId}\0${message.role}\0${message.sequence}\0${message.excludedFromContext}\0${message.truncated}`);
          fingerprintHash.update(stable({ text: message.text, calls: message.calls, results: message.results, parseIssues: message.parseIssues }));
          for (const issue of message.parseIssues) {
            const category = issueCategory(issue);
            parsedIssues[category] = (parsedIssues[category] ?? 0) + 1;
          }
          const invalidCallIds = new Set(message.parseIssues.flatMap((issue) => {
            const match = /^(?:conflictingToolCall|invalidArguments):(.+)$/.exec(issue);
            return match ? [match[1]] : [];
          }));
          const invalidResultIds = new Set(message.parseIssues.flatMap((issue) => {
            const match = /^conflictingToolResult:(.+)$/.exec(issue);
            return match ? [match[1]] : [];
          }));
          if (message.role === "user") { repeatKey = null; repeatCount = 0; failureStreak = null; }
          for (const call of message.calls) {
            const names = labelOf(call);
            const record: CallRecord = { call, message, label: names.label, effectiveName: names.effective };
            calls++;
            let metric = metrics.get(names.label);
            if (!metric) {
              metric = { label: names.label, outerName: names.outer, effectiveName: names.effective, calls: 0, pairedResults: 0, pairedKnownResults: 0, errors: 0, unknownErrors: 0, calledThreads: 0, errorAffectedThreads: 0, errorRate: null, called: new Set(), errored: new Set() };
              metrics.set(names.label, metric);
            }
            metric.calls++;
            metric.called.add(thread.id);
            if (invalidCallIds.has(call.id)) continue;
            const key = `${names.label}:${stable(call.arguments)}`;
            if (repeatKey === key) {
              repeatCount++;
              if (repeatCount === 3) addCandidate("repeated-call", thread.id, message.messageId, call.id, calls);
            } else {
              repeatKey = key;
              repeatCount = 1;
            }
            const pairingKey = `${thread.id}\0${call.id}`;
            if (seenCalls.has(call.id)) duplicateCallIds++;
            else { seenCalls.add(call.id); pending.set(pairingKey, record); }
          }
          for (const result of message.results) {
            executionResultCount++;
            if (result.execution.source === "typed") typedExecutionCount++;
            executionStatusCounts[result.execution.status]++;
            if (result.execution.status !== "unknown") knownExecutionCount++;
            if (result.execution.outputRef !== null) executionOutputRefCount++;
            if (result.execution.outputTruncated) executionOutputTruncatedCount++;
            const resultSize = bytes(result.content);
            resultSizes.push(resultSize);
            if (result.isError === true) errorResultSizes.push(resultSize);
            if (resultSize >= 100_000) {
              if (giantEntries.length < 8) giantEntries.push({ threadId: message.threadId, messageId: message.messageId, callId: result.id });
            }
            if (invalidResultIds.has(result.id)) continue;
            if (seenResults.has(result.id)) { duplicateResultIds++; continue; }
            seenResults.add(result.id);
            const call = pending.get(`${thread.id}\0${result.id}`);
            if (!call) { orphanResults++; continue; }
            pairedResults++;
            const metric = metrics.get(call.label)!;
            metric.pairedResults++;
            if (result.isError !== null) {
              pairedKnownResults++;
              metric.pairedKnownResults++;
            }
            if (result.isError === true) {
              explicitErrors++;
              metric.errors++;
              metric.errored.add(message.threadId);
              const next: number = failureStreak !== null && failureStreak.label === call.label ? failureStreak.count + 1 : 1;
              failureStreak = { label: call.label, count: next, threadId: message.threadId, messageId: message.messageId, callId: result.id };
              if (next === 2) addCandidate("explicit-failure", message.threadId, message.messageId, result.id, pairedKnownResults);
            } else if (result.isError === null) {
              unknownErrorResults++;
              metric.unknownErrors++;
              failureStreak = null;
            } else {
              failureStreak = null;
            }
            pending.delete(`${thread.id}\0${result.id}`);
          }
        }
        for (const record of pending.values()) if (record.message.threadId === thread.id) missingResults++;
        for (const key of [...pending.keys()]) if (pending.get(key)?.message.threadId === thread.id) pending.delete(key);
      }
      for (const metric of metrics.values()) {
        metric.calledThreads = metric.called.size;
        metric.errorAffectedThreads = metric.errored.size;
        metric.errorRate = metric.pairedKnownResults === 0 ? null : metric.errors / metric.pairedKnownResults;
      }
      const giantCount = resultSizes.filter((size) => size >= 100_000).length;
      const fingerprint = fingerprintHash.digest("hex");
      if (candidates.has("repeated-call")) candidates.get("repeated-call")!.denominator = calls;
      if (candidates.has("explicit-failure")) candidates.get("explicit-failure")!.denominator = pairedKnownResults;
      const candidatesList = [...candidates.values()];
      if (giantCount > 0) {
        candidatesList.push({ ruleId: "large-tool-output", evidenceKind: "observation", counts: giantCount, denominator: resultSizes.length, evidence: evidence(giantEntries), nextVerification: nextVerification("large-tool-output") });
      }
      return {
        version: { normalizer: NORMALIZER_VERSION, metrics: METRICS_VERSION },
        source: { fingerprint },
        filters: { scope, includeHidden, since: validated.sinceText, until: validated.untilText },
        totals: {
          threads: threads.length, messages, calls, pairedResults, pairedKnownResults, missingResults, orphanResults,
          duplicateCallIds, duplicateResultIds, unknownErrorResults, explicitErrors,
          parseIssues: Object.values(parsedIssues).reduce((sum, count) => sum + count, 0),
          execution: {
            resultCount: executionResultCount, typedCount: typedExecutionCount, knownCount: knownExecutionCount,
            coverage: executionResultCount === 0 ? null : typedExecutionCount / executionResultCount,
            statusCounts: executionStatusCounts, outputRefCount: executionOutputRefCount,
            outputTruncatedCount: executionOutputTruncatedCount,
          },
        },
        tools: Object.fromEntries([...metrics].map(([name, metric]) => [name, {
          label: metric.label,
          outerName: metric.outerName,
          effectiveName: metric.effectiveName,
          calls: metric.calls,
          pairedResults: metric.pairedResults,
          pairedKnownResults: metric.pairedKnownResults,
          errors: metric.errors,
          unknownErrors: metric.unknownErrors,
          calledThreads: metric.calledThreads,
          errorAffectedThreads: metric.errorAffectedThreads,
          errorRate: metric.errorRate,
        }])),
        resultBytes: { count: resultSizes.length, giantCount, p50: quantile(resultSizes, 0.5), p95: quantile(resultSizes, 0.95), errorBytes: errorResultSizes.reduce((sum, size) => sum + size, 0) },
        parseIssues: parsedIssues,
        candidates: candidatesList,
      } as AnalysisReport;
    });
    return snapshot.value;
  } finally { loader.close(); }
}
