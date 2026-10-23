/** Frequency and shape of shell `sleep` waiting in persisted Peri transcripts. */
import { createHash } from "crypto";
import { DataLoader, DEFAULT_DB_PATH, NORMALIZER_VERSION } from "../data/loader.js";
import { validateThreadFilter } from "../data/filters.js";
import { issueCategory, selectedThreads, type AnalysisOptions, type AnalysisScope } from "./metrics.js";
import type { ThreadSummary, ToolCall, ToolExecutionStatus, ToolResult } from "../data/types.js";
import { analyzeSleepCommand, extractCommandArgument, type SleepShape, type WaitKind } from "./shell_command.js";

export const WAITING_METRICS_VERSION = "shell-waiting-v1";

export type ThreadKind = "visible_root" | "hidden_root" | "child";
export type SleepExecutionStatus = ToolExecutionStatus | "missing";
export type SleepDurationBucket = "<1s" | "1-4s" | "5-9s" | "10-29s" | "30-59s" | "60-299s" | "300-899s" | ">=900s" | "unknown";

export interface SleepEvidenceEntry {
  threadId: string;
  messageId: string;
  callId: string;
  tool: string;
  shape: SleepShape;
  waitKind: WaitKind;
  seconds: number | null;
  variableSleep: boolean;
  background: boolean;
  timeoutMs: number | null;
  status: SleepExecutionStatus;
  isError: boolean | null;
}

export interface ToolWaitingMetric {
  name: string;
  calls: number;
  commandCalls: number;
  sleepCalls: number;
  sleepShare: number | null;
}

export interface ThreadWaitingMetric {
  threadId: string;
  threadKind: ThreadKind;
  createdMonth: string;
  commandCalls: number;
  sleepCalls: number;
  sleepShare: number | null;
  sleepSeconds: number;
}

export interface ShellWaitingReport {
  version: { normalizer: string; metrics: string };
  source: { fingerprint: string };
  filters: { scope: AnalysisScope; includeHidden: boolean; since: string | null; until: string | null };
  totals: {
    threads: number;
    messages: number;
    toolCalls: number;
    commandCalls: number;
    commandThreads: number;
    duplicateCallIds: number;
    duplicateResultIds: number;
    missingSleepResults: number;
    parseIssues: number;
  };
  sleep: {
    calls: number;
    shareOfCommandCalls: number | null;
    threadsWithSleep: number;
    threadShare: number | null;
    totalSleepSeconds: number;
    callSleepSeconds: { p50: number | null; p90: number | null; max: number | null; knownSamples: number };
    threadSleepSeconds: { p50: number | null; p90: number | null; max: number | null; knownSamples: number };
    backgroundSleepCalls: number;
    sleepExceedingTimeoutCalls: number;
    byShape: Record<SleepShape, number>;
    byWaitKind: Record<WaitKind, number>;
    byDurationBucket: Record<SleepDurationBucket, number>;
    byExecutionStatus: Record<SleepExecutionStatus, number>;
    byIsError: Record<"true" | "false" | "unknown", number>;
    byThreadType: Record<ThreadKind, { commandCalls: number; sleepCalls: number; threadsWithCommand: number; threadsWithSleep: number }>;
    byMonth: Record<string, { commandCalls: number; sleepCalls: number; sleepShare: number | null }>;
    byWeek: Record<string, { commandCalls: number; sleepCalls: number; sleepShare: number | null }>;
    threadSleepHistogram: Record<"1" | "2-4" | "5-9" | "10+", number>;
    topThreads: ThreadWaitingMetric[];
    examples: SleepEvidenceEntry[];
  };
  tools: ToolWaitingMetric[];
  sensitivity: {
    broadTextMatchCalls: number;
    wrapperEmbeddedSleepCalls: number;
    note: string;
  };
  parseIssues: Record<string, number>;
  capabilities: Record<string, { status: "available" | "partial" | "unavailable"; reason: string }>;
}

interface SleepRecord extends SleepEvidenceEntry {
  seconds: number | null;
}

function ratio(numerator: number, denominator: number): number | null {
  return denominator === 0 ? null : Number((numerator / denominator).toFixed(6));
}

function quantile(values: number[], fraction: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * fraction))] ?? null;
}

function threadKindOf(thread: ThreadSummary): ThreadKind {
  if (thread.parent_thread_id !== null) return "child";
  return thread.hidden === 0 ? "visible_root" : "hidden_root";
}

/** UTC Monday that starts the week containing the given instant. */
function weekStartUtc(date: Date): string {
  const day = date.getUTCDay();
  const delta = (day + 6) % 7;
  return new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate() - delta))
    .toISOString()
    .slice(0, 10);
}

function durationBucket(seconds: number, variableSleep: boolean): SleepDurationBucket {
  if (variableSleep && seconds === 0) return "unknown";
  if (seconds < 1) return "<1s";
  if (seconds < 5) return "1-4s";
  if (seconds < 10) return "5-9s";
  if (seconds < 30) return "10-29s";
  if (seconds < 60) return "30-59s";
  if (seconds < 300) return "60-299s";
  if (seconds < 900) return "300-899s";
  return ">=900s";
}

function emptyShapeCounts(): Record<SleepShape, number> {
  return { standalone: 0, compound: 0, loop: 0, variable: 0 };
}
function emptyWaitKindCounts(): Record<WaitKind, number> {
  return { pure: 0, noop_only: 0, other_command: 0 };
}
function emptyBucketCounts(): Record<SleepDurationBucket, number> {
  return { "<1s": 0, "1-4s": 0, "5-9s": 0, "10-29s": 0, "30-59s": 0, "60-299s": 0, "300-899s": 0, ">=900s": 0, unknown: 0 };
}
function emptyStatusCounts(): Record<SleepExecutionStatus, number> {
  return { unknown: 0, completed: 0, failed: 0, cancelled: 0, timed_out: 0, running: 0, running_after_timeout: 0, missing: 0 };
}
function emptyIsErrorCounts(): Record<"true" | "false" | "unknown", number> {
  return { true: 0, false: 0, unknown: 0 };
}
function emptyThreadTypeCounts(): Record<ThreadKind, { commandCalls: number; sleepCalls: number; threadsWithCommand: number; threadsWithSleep: number }> {
  return {
    visible_root: { commandCalls: 0, sleepCalls: 0, threadsWithCommand: 0, threadsWithSleep: 0 },
    hidden_root: { commandCalls: 0, sleepCalls: 0, threadsWithCommand: 0, threadsWithSleep: 0 },
    child: { commandCalls: 0, sleepCalls: 0, threadsWithCommand: 0, threadsWithSleep: 0 },
  };
}

const BROAD_SLEEP_TEXT = /(?:^|[\s;&|(])sleep\s+(?:\d|\.|\$|[A-Za-z_])/;
const SLEEP_TEXT_ANYWHERE = /\bsleep\s+(?:\d|\.|\$|[A-Za-z_])/;
const WRAPPER_TEXT = /\b(?:bash|sh)\s+-c\b|\bxargs\b|\beval\b|\bsudo\b/;

/** Analyze one immutable SQLite snapshot. Never writes the source database. */
export function analyzeShellWaiting(dbPath: string = DEFAULT_DB_PATH, input: AnalysisOptions = {}): ShellWaitingReport {
  const validated = validateThreadFilter(input);
  const loader = new DataLoader(dbPath);
  try {
    return loader.withSnapshot((db) => {
      const threads = selectedThreads(db.loadThreadSummaries(), {
        scope: validated.scope,
        includeHidden: validated.includeHidden,
        since: validated.since,
        until: validated.until,
      });
      const fingerprint = createHash("sha256");
      fingerprint.update(JSON.stringify({
        schema: loader.capabilities,
        scope: validated.scope,
        includeHidden: validated.includeHidden,
        since: validated.sinceText,
        until: validated.untilText,
      }));
      for (const thread of threads) {
        fingerprint.update(`${thread.id}\0${thread.created_at}\0${thread.hidden}\0${thread.parent_thread_id ?? ""}`);
      }

      const tools = new Map<string, { calls: number; commandCalls: number; sleepCalls: number }>();
      const threadMetrics: ThreadWaitingMetric[] = [];
      const sleepSecondSamples: number[] = [];
      const threadSleepSecondSamples: number[] = [];
      const examples: SleepEvidenceEntry[] = [];
      const exampleThreads = new Set<string>();
      const parseIssues: Record<string, number> = Object.create(null) as Record<string, number>;
      const byShape = emptyShapeCounts();
      const byWaitKind = emptyWaitKindCounts();
      const byBucket = emptyBucketCounts();
      const byStatus = emptyStatusCounts();
      const byIsError = emptyIsErrorCounts();
      const byThreadType = emptyThreadTypeCounts();
      const byMonth = new Map<string, { commandCalls: number; sleepCalls: number }>();
      const byWeek = new Map<string, { commandCalls: number; sleepCalls: number }>();
      const threadSleepHistogram = { "1": 0, "2-4": 0, "5-9": 0, "10+": 0 };

      let messages = 0;
      let toolCalls = 0;
      let commandCalls = 0;
      let sleepCalls = 0;
      let commandThreads = 0;
      let threadsWithSleep = 0;
      let duplicateCallIds = 0;
      let duplicateResultIds = 0;
      let missingSleepResults = 0;
      let totalSleepSeconds = 0;
      let backgroundSleepCalls = 0;
      let sleepExceedingTimeoutCalls = 0;
      let broadTextMatchCalls = 0;
      let wrapperEmbeddedSleepCalls = 0;

      for (const thread of threads) {
        const kind = threadKindOf(thread);
        const month = thread.created_at.slice(0, 7);
        const monthCounts = byMonth.get(month) ?? { commandCalls: 0, sleepCalls: 0 };
        byMonth.set(month, monthCounts);
        const week = weekStartUtc(new Date(thread.created_at));
        const weekCounts = byWeek.get(week) ?? { commandCalls: 0, sleepCalls: 0 };
        byWeek.set(week, weekCounts);
        const results = new Map<string, ToolResult>();
        const seenCalls = new Set<string>();
        const threadSleep: SleepRecord[] = [];
        let threadCommandCalls = 0;
        let threadSleepSeconds = 0;

        for (const message of db.iterateNormalizedMessages(thread.id)) {
          messages++;
          fingerprint.update(`${message.messageId}\0${message.role}\0${message.sequence}\0${message.truncated ? 1 : 0}`);
          for (const issue of message.parseIssues) {
            const category = issueCategory(issue);
            parseIssues[category] = (parseIssues[category] ?? 0) + 1;
          }
          for (const result of message.results) {
            fingerprint.update(`${result.id}\0${result.execution.status}\0${result.isError === null ? "null" : result.isError}`);
            if (results.has(result.id)) duplicateResultIds++;
            else results.set(result.id, result);
          }
          for (const call of message.calls) {
            toolCalls++;
            fingerprint.update(`${call.id}\0${call.name}\0${JSON.stringify(call.arguments)}`);
            const tool = tools.get(call.name) ?? { calls: 0, commandCalls: 0, sleepCalls: 0 };
            tools.set(call.name, tool);
            tool.calls++;
            if (seenCalls.has(call.id)) { duplicateCallIds++; continue; }
            seenCalls.add(call.id);
            const command = extractCommandArgument(call.arguments);
            if (command === null) continue;
            commandCalls++;
            threadCommandCalls++;
            tool.commandCalls++;
            if (BROAD_SLEEP_TEXT.test(command)) broadTextMatchCalls++;
            const sleep = analyzeSleepCommand(command);
            if (sleep.sleepSegments === 0) {
              if (SLEEP_TEXT_ANYWHERE.test(command) && WRAPPER_TEXT.test(command)) wrapperEmbeddedSleepCalls++;
              continue;
            }
            sleepCalls++;
            tool.sleepCalls++;
            byShape[sleep.shape!]++;
            byWaitKind[sleep.waitKind as WaitKind]++;
            byBucket[durationBucket(sleep.seconds, sleep.variableSleep)]++;
            if (!(sleep.variableSleep && sleep.seconds === 0)) sleepSecondSamples.push(sleep.seconds);
            totalSleepSeconds += sleep.seconds;
            threadSleepSeconds += sleep.seconds;
            const background = isTrue(call.arguments, "run_in_background") || isTrue(call.arguments, "background");
            if (background) backgroundSleepCalls++;
            const timeoutMs = numberField(call.arguments, "timeout");
            if (timeoutMs !== null && timeoutMs > 0 && sleep.seconds * 1000 > timeoutMs) sleepExceedingTimeoutCalls++;
            threadSleep.push({
              threadId: thread.id,
              messageId: message.messageId,
              callId: call.id,
              tool: call.name,
              shape: sleep.shape!,
              waitKind: sleep.waitKind as WaitKind,
              seconds: sleep.variableSleep && sleep.seconds === 0 ? null : sleep.seconds,
              variableSleep: sleep.variableSleep,
              background,
              timeoutMs,
              status: "missing",
              isError: null,
            });
          }
        }

        for (const record of threadSleep) {
          const result = results.get(record.callId);
          record.status = result ? result.execution.status : "missing";
          record.isError = result ? result.isError : null;
          byStatus[record.status]++;
          byIsError[record.isError === null ? "unknown" : String(record.isError) as "true" | "false"]++;
          if (!result) missingSleepResults++;
          if (examples.length < 15 && !exampleThreads.has(record.threadId)) {
            exampleThreads.add(record.threadId);
            examples.push({ ...record });
          }
        }

        byThreadType[kind].commandCalls += threadCommandCalls;
        byThreadType[kind].sleepCalls += threadSleep.length;
        monthCounts.commandCalls += threadCommandCalls;
        monthCounts.sleepCalls += threadSleep.length;
        weekCounts.commandCalls += threadCommandCalls;
        weekCounts.sleepCalls += threadSleep.length;
        if (threadCommandCalls > 0) {
          commandThreads++;
          byThreadType[kind].threadsWithCommand++;
        }
        if (threadSleep.length > 0) {
          threadsWithSleep++;
          byThreadType[kind].threadsWithSleep++;
          threadSleepHistogram[threadSleep.length === 1 ? "1" : threadSleep.length <= 4 ? "2-4" : threadSleep.length <= 9 ? "5-9" : "10+"]++;
          threadSleepSecondSamples.push(threadSleepSeconds);
        }
        threadMetrics.push({
          threadId: thread.id,
          threadKind: kind,
          createdMonth: month,
          commandCalls: threadCommandCalls,
          sleepCalls: threadSleep.length,
          sleepShare: ratio(threadSleep.length, threadCommandCalls),
          sleepSeconds: Number(threadSleepSeconds.toFixed(1)),
        });
      }

      const toolMetrics: ToolWaitingMetric[] = [...tools.entries()]
        .map(([name, value]) => ({ name, ...value, sleepShare: ratio(value.sleepCalls, value.commandCalls) }))
        .sort((left, right) => right.calls - left.calls || left.name.localeCompare(right.name));
      const byMonthOut: Record<string, { commandCalls: number; sleepCalls: number; sleepShare: number | null }> = {};
      for (const month of [...byMonth.keys()].sort()) {
        const counts = byMonth.get(month)!;
        byMonthOut[month] = { ...counts, sleepShare: ratio(counts.sleepCalls, counts.commandCalls) };
      }
      const byWeekOut: Record<string, { commandCalls: number; sleepCalls: number; sleepShare: number | null }> = {};
      for (const week of [...byWeek.keys()].sort()) {
        const counts = byWeek.get(week)!;
        byWeekOut[week] = { ...counts, sleepShare: ratio(counts.sleepCalls, counts.commandCalls) };
      }

      return {
        version: { normalizer: NORMALIZER_VERSION, metrics: WAITING_METRICS_VERSION },
        source: { fingerprint: fingerprint.digest("hex") },
        filters: {
          scope: validated.scope,
          includeHidden: validated.includeHidden,
          since: validated.sinceText,
          until: validated.untilText,
        },
        totals: {
          threads: threads.length,
          messages,
          toolCalls,
          commandCalls,
          commandThreads,
          duplicateCallIds,
          duplicateResultIds,
          missingSleepResults,
          parseIssues: Object.values(parseIssues).reduce((sum, value) => sum + value, 0),
        },
        sleep: {
          calls: sleepCalls,
          shareOfCommandCalls: ratio(sleepCalls, commandCalls),
          threadsWithSleep,
          threadShare: ratio(threadsWithSleep, commandThreads),
          totalSleepSeconds: Number(totalSleepSeconds.toFixed(1)),
          callSleepSeconds: {
            p50: quantile(sleepSecondSamples, 0.5),
            p90: quantile(sleepSecondSamples, 0.9),
            max: sleepSecondSamples.length === 0 ? null : Math.max(...sleepSecondSamples),
            knownSamples: sleepSecondSamples.length,
          },
          threadSleepSeconds: {
            p50: quantile(threadSleepSecondSamples, 0.5),
            p90: quantile(threadSleepSecondSamples, 0.9),
            max: threadSleepSecondSamples.length === 0 ? null : Math.max(...threadSleepSecondSamples),
            knownSamples: threadSleepSecondSamples.length,
          },
          backgroundSleepCalls,
          sleepExceedingTimeoutCalls,
          byShape,
          byWaitKind,
          byDurationBucket: byBucket,
          byExecutionStatus: byStatus,
          byIsError,
          byThreadType,
          byMonth: byMonthOut,
          byWeek: byWeekOut,
          threadSleepHistogram,
          topThreads: threadMetrics
            .filter((thread) => thread.sleepCalls > 0)
            .sort((left, right) => right.sleepCalls - left.sleepCalls || right.commandCalls - left.commandCalls)
            .slice(0, 15),
          examples,
        },
        tools: toolMetrics,
        sensitivity: {
          broadTextMatchCalls,
          wrapperEmbeddedSleepCalls,
          note: "Primary counts require a simple command whose effective first token is sleep. Broad text matches and wrapper-embedded sleep calls are upper-bound aids and are not manually reviewed.",
        },
        parseIssues: Object.fromEntries(Object.entries(parseIssues).sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))),
        capabilities: {
          command_attribution: { status: "available", reason: "top-level command/cmd/script/shell_command argument on persisted tool calls; ExecuteExtraTool wrappers are not unwrapped" },
          actual_wait_duration: { status: "unavailable", reason: "persisted messages have no stable wall-clock duration or loop iteration count" },
          completion: { status: "unavailable", reason: "a successful sleep does not prove the waited-for condition was reached" },
          execution_status: { status: "partial", reason: "typed execution metadata is present for a minority of legacy tool results; missing metadata remains missing" },
        } as ShellWaitingReport["capabilities"],
      };
    }).value;
  } finally {
    loader.close();
  }
}

function isTrue(args: unknown, key: string): boolean {
  return !!args && typeof args === "object" && !Array.isArray(args) && (args as Record<string, unknown>)[key] === true;
}

function numberField(args: unknown, key: string): number | null {
  if (!args || typeof args !== "object" || Array.isArray(args)) return null;
  const value = (args as Record<string, unknown>)[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
