/**
 * Langfuse API 客户端和离线分析共用逻辑。
 * 报表只投影计数、时长和固定分类；不得返回或打印 input/output/error 正文。
 */
const BASE_URL = (process.env.LANGFUSE_HOST || process.env.LANGFUSE_BASE_URL || "").replace(/\/$/, "");
const PUBLIC_KEY = process.env.LANGFUSE_PUBLIC_KEY || "";
const SECRET_KEY = process.env.LANGFUSE_SECRET_KEY || "";

export async function api(path: string) {
  if (!BASE_URL || !PUBLIC_KEY || !SECRET_KEY) {
    throw new Error("Missing LANGFUSE_HOST/PUBLIC_KEY/SECRET_KEY env vars");
  }
  const authHeader = `Basic ${btoa(`${PUBLIC_KEY}:${SECRET_KEY}`)}`;
  const res = await fetch(`${BASE_URL}${path}`, {
    headers: { Authorization: authHeader, "Content-Type": "application/json" },
  });
  if (!res.ok) throw new Error(`API ${path}: HTTP ${res.status}`);
  const contentType = res.headers.get("content-type") || "";
  if (!contentType.toLowerCase().includes("application/json")) throw new Error(`API ${path}: expected JSON response`);
  return res.json();
}

const MAX_TRACE_LIMIT = 100;

export function clampTraceLimit(value: number, fallback = 50) {
  return Number.isInteger(value) && value > 0 ? Math.min(value, MAX_TRACE_LIMIT) : fallback;
}

export async function fetchTraces(limit: number) {
  const data = await api(`/api/public/traces?limit=${clampTraceLimit(limit)}`);
  return (data.data || []) as any[];
}

export interface TraceFilter {
  limit?: number;
  page?: number;
  fromTimestamp?: string;
  toTimestamp?: string;
  tags?: string[];
  userId?: string;
  sessionId?: string;
  name?: string;
  orderBy?: string;
}

export async function fetchTracesFiltered(filter: TraceFilter): Promise<{ traces: any[]; meta: any }> {
  const params = new URLSearchParams();
  if (filter.limit) params.set("limit", String(clampTraceLimit(filter.limit)));
  if (filter.page) params.set("page", String(filter.page));
  if (filter.fromTimestamp) params.set("fromTimestamp", filter.fromTimestamp);
  if (filter.toTimestamp) params.set("toTimestamp", filter.toTimestamp);
  if (filter.tags?.length) params.set("tags", filter.tags.join(","));
  if (filter.userId) params.set("userId", filter.userId);
  if (filter.sessionId) params.set("sessionId", filter.sessionId);
  if (filter.name) params.set("name", filter.name);
  if (filter.orderBy) params.set("orderBy", filter.orderBy);
  const data = await api(`/api/public/traces?${params.toString()}`);
  return { traces: (data.data || []) as any[], meta: data.meta || {} };
}

export async function fetchAllTracesFiltered(filter: Omit<TraceFilter, "page"> & { maxPages?: number }): Promise<any[]> {
  const all: any[] = [];
  const maxPages = filter.maxPages ?? 20;
  for (let page = 1; page <= maxPages; page++) {
    const { traces, meta } = await fetchTracesFiltered({ ...filter, page, limit: clampTraceLimit(filter.limit || 50) });
    all.push(...traces);
    if (page >= (meta.totalPages || 1) || traces.length === 0) break;
  }
  return all;
}

export async function fetchObservations(traceId: string) {
  const all: any[] = [];
  for (let page = 1; ; page++) {
    const data = await api(`/api/public/observations?traceId=${traceId}&limit=100&page=${page}`);
    const items = (data.data || []) as any[];
    all.push(...items);
    if (page >= (data.meta?.totalPages || 1)) break;
  }
  return all;
}

export interface ObservationTreeAudit {
  duplicateIds: string[];
  missingParents: { id: string; parentObservationId: string }[];
  cycles: string[][];
}

/** 只检查 observation 身份与父链，不投影 input/output 正文。 */
export function auditObservationTree(observations: any[], traceId: string): ObservationTreeAudit {
  const counts = new Map<string, number>();
  const parentById = new Map<string, string | undefined>();
  for (const observation of observations) {
    const id = typeof observation?.id === "string" ? observation.id : "";
    if (!id) continue;
    counts.set(id, (counts.get(id) || 0) + 1);
    if (!parentById.has(id)) {
      parentById.set(id, typeof observation.parentObservationId === "string" && observation.parentObservationId ? observation.parentObservationId : undefined);
    }
  }

  const knownIds = new Set(parentById.keys());
  const duplicateIds = [...counts.entries()].filter(([, count]) => count > 1).map(([id]) => id).sort();
  const missingParents = [...parentById.entries()]
    .filter(([, parentId]) => parentId && parentId !== traceId && !knownIds.has(parentId))
    .map(([id, parentObservationId]) => ({ id, parentObservationId: parentObservationId! }))
    .sort((left, right) => left.id.localeCompare(right.id));

  const cycles: string[][] = [];
  const completed = new Set<string>();
  for (const startId of [...knownIds].sort()) {
    if (completed.has(startId)) continue;
    const path: string[] = [];
    const pathIndexes = new Map<string, number>();
    let currentId: string | undefined = startId;
    while (currentId && knownIds.has(currentId) && !completed.has(currentId)) {
      const cycleStart = pathIndexes.get(currentId);
      if (cycleStart !== undefined) {
        cycles.push([...path.slice(cycleStart), currentId]);
        break;
      }
      pathIndexes.set(currentId, path.length);
      path.push(currentId);
      const parentId = parentById.get(currentId);
      currentId = parentId === traceId ? undefined : parentId;
    }
    for (const id of path) completed.add(id);
  }

  return { duplicateIds, missingParents, cycles };
}

export async function fetchScores(traceId: string) {
  const all: any[] = [];
  for (let page = 1; ; page++) {
    const data = await api(`/api/public/scores?traceId=${traceId}&limit=100&page=${page}`);
    const items = (data.data || []) as any[];
    all.push(...items);
    if (page >= (data.meta?.totalPages || 1)) break;
  }
  return all;
}

export function isoNow() { return new Date().toISOString(); }
export function isoSpan(fromISO: string, days: number) {
  const date = new Date(fromISO);
  date.setDate(date.getDate() - days);
  return date.toISOString();
}

export function parseTraceArg(args: string[]): { traceId?: string; index?: number } {
  const valueFlags = ["--index", "--from", "--to", "--days", "--tag", "--user", "--session", "--name", "--limit"];
  const valuePositions = new Set<number>();
  for (const flag of valueFlags) {
    const index = args.indexOf(flag);
    if (index !== -1 && args[index + 1] !== undefined) valuePositions.add(index + 1);
  }
  const traceId = args.find((value, index) => !value.startsWith("--") && !valuePositions.has(index));
  const index = Number(args[args.indexOf("--index") + 1]);
  return { traceId, index: Number.isInteger(index) && index > 0 ? index : undefined };
}

export function parseTimeWindow(args: string[]): { from?: string; to?: string } {
  let from = args[args.indexOf("--from") + 1];
  let to = args[args.indexOf("--to") + 1];
  if (!args.includes("--from")) from = undefined;
  if (!args.includes("--to")) to = undefined;
  const daysIndex = args.indexOf("--days");
  if (daysIndex !== -1) {
    const days = Number(args[daysIndex + 1]);
    if (Number.isFinite(days)) {
      to ||= isoNow();
      from ||= isoSpan(to, days);
    }
  }
  return { from, to };
}

export function parseFilterArgs(args: string[]): {
  tag?: string; userId?: string; sessionId?: string; name?: string; model?: string; limit: number; time: { from?: string; to?: string };
} {
  const value = (flag: string) => {
    const index = args.indexOf(flag);
    return index === -1 ? undefined : args[index + 1];
  };
  let limit = clampTraceLimit(Number(value("--limit")) || 50);
  const valueFlags = ["--from", "--to", "--days", "--tag", "--user", "--session", "--name", "--model", "--limit"];
  const valuePositions = new Set(valueFlags.flatMap((flag) => {
    const index = args.indexOf(flag);
    return index !== -1 && args[index + 1] !== undefined ? [index + 1] : [];
  }));
  for (let index = 0; index < args.length; index++) {
    if (valuePositions.has(index)) continue;
    const positional = Number(args[index]);
    if (Number.isInteger(positional) && positional > 0) { limit = clampTraceLimit(positional); break; }
  }
  return { tag: value("--tag"), userId: value("--user"), sessionId: value("--session"), name: value("--name"), model: value("--model"), limit, time: parseTimeWindow(args) };
}

export function fmt(n: number) { return n.toLocaleString(); }
export function pct(part: number, whole: number) { return whole > 0 ? `${((part / whole) * 100).toFixed(1)}%` : "-"; }
export function bar(percent: number, width = 20) { const filled = Math.round((percent / 100) * width); return "█".repeat(filled) + "░".repeat(width - filled); }
export function ms(seconds: number) {
  if (seconds < 1) return `${(seconds * 1000).toFixed(0)}ms`;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  return `${Math.floor(seconds / 60)}m${(seconds % 60).toFixed(0)}s`;
}
export function isoToLocal(iso: string) {
  const date = new Date(iso);
  return Number.isNaN(date.valueOf()) ? iso.slice(0, 16) : date.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export function genTokens(generation: any) {
  const usageDetails = generation?.usageDetails && typeof generation.usageDetails === "object" ? generation.usageDetails : {};
  const legacyUsage = generation?.usage && typeof generation.usage === "object" ? generation.usage : {};
  const nonNegative = (...values: unknown[]) => {
    for (const value of values) {
      if ((typeof value !== "number" && typeof value !== "string") || value === "") continue;
      const number = Number(value);
      if (Number.isFinite(number) && number >= 0) return number;
    }
    return 0;
  };
  return {
    // build_usage_details() writes raw input, already excluding cache read/create.
    input: nonNegative(
      usageDetails.input, usageDetails.input_tokens, usageDetails.inputTokens,
      usageDetails.prompt_tokens, usageDetails.promptTokens,
      legacyUsage.input, legacyUsage.input_tokens, legacyUsage.inputTokens,
      legacyUsage.prompt_tokens, legacyUsage.promptTokens,
    ),
    output: nonNegative(
      usageDetails.output, usageDetails.output_tokens, usageDetails.outputTokens,
      usageDetails.completion_tokens, usageDetails.completionTokens,
      legacyUsage.output, legacyUsage.output_tokens, legacyUsage.outputTokens,
      legacyUsage.completion_tokens, legacyUsage.completionTokens,
    ),
    cacheRead: nonNegative(
      usageDetails.cache_read_input_tokens, usageDetails.cacheReadInputTokens,
      usageDetails.cache_read, usageDetails.cacheRead,
      legacyUsage.cache_read_input_tokens, legacyUsage.cacheReadInputTokens,
      legacyUsage.cache_read, legacyUsage.cacheRead,
    ),
    cacheCreate: nonNegative(
      usageDetails.cache_creation_input_tokens, usageDetails.cacheCreationInputTokens,
      usageDetails.cache_creation, usageDetails.cacheCreation,
      legacyUsage.cache_creation_input_tokens, legacyUsage.cacheCreationInputTokens,
      legacyUsage.cache_creation, legacyUsage.cacheCreation,
    ),
  };
}

export interface TokenSummary { input: number; output: number; cacheRead: number; cacheCreate: number; effective: number; calls: number; }
export type InputTokenBuckets = Pick<TokenSummary, "input" | "cacheRead" | "cacheCreate">;
export function totalInputTraffic(tokens: InputTokenBuckets) {
  return tokens.input + tokens.cacheRead + tokens.cacheCreate;
}
export function summarizeTokens(generations: any[]): TokenSummary {
  const total = generations.reduce((sum, generation) => {
    const tokens = genTokens(generation);
    sum.input += tokens.input; sum.output += tokens.output; sum.cacheRead += tokens.cacheRead; sum.cacheCreate += tokens.cacheCreate;
    return sum;
  }, { input: 0, output: 0, cacheRead: 0, cacheCreate: 0 });
  // input is raw input; cache buckets are additional, independently reported usage.
  return { ...total, effective: total.input, calls: generations.length };
}

export type LatencySource = "agent-run" | "observations" | "unavailable";
export interface LatencySummary { seconds: number | null; source: LatencySource; }
function validInterval(observation: any): { start: number; end: number } | undefined {
  const start = Date.parse(observation?.startTime);
  const end = Date.parse(observation?.endTime);
  return Number.isFinite(start) && Number.isFinite(end) && end >= start ? { start, end } : undefined;
}
/** 真正的 trace 时长：优先主 agent-run，退化到所有 observation 的时间包络。 */
export function summarizeLatency(observations: any[]): LatencySummary {
  const agentRun = observations.find((observation) => observation?.type === "AGENT" && observation?.name === "agent-run");
  const interval = validInterval(agentRun);
  if (interval) return { seconds: (interval.end - interval.start) / 1000, source: "agent-run" };
  const intervals = observations.map(validInterval).filter((value): value is { start: number; end: number } => Boolean(value));
  if (!intervals.length) return { seconds: null, source: "unavailable" };
  return { seconds: (Math.max(...intervals.map((value) => value.end)) - Math.min(...intervals.map((value) => value.start))) / 1000, source: "observations" };
}
export function summarizeObservationLatency(observation: any): number | null {
  const interval = validInterval(observation);
  return interval ? (interval.end - interval.start) / 1000 : null;
}
export function fmtLatency(latency: LatencySummary) { return latency.seconds === null ? "N/A" : ms(latency.seconds); }

export interface TraceMetrics {
  llmCalls: number; toolCalls: number; inputTokens: number; outputTokens: number; cacheReadTokens: number; cacheCreateTokens: number; effectiveNewTokens: number; latency: LatencySummary;
}
export function summarizeTraceMetrics(observations: any[]): TraceMetrics {
  const tokens = summarizeTokens(observations.filter((observation) => observation?.type === "GENERATION"));
  return { llmCalls: tokens.calls, toolCalls: observations.filter((observation) => observation?.type === "TOOL").length, inputTokens: tokens.input, outputTokens: tokens.output, cacheReadTokens: tokens.cacheRead, cacheCreateTokens: tokens.cacheCreate, effectiveNewTokens: tokens.effective, latency: summarizeLatency(observations) };
}

/** 每百万 token 美元定价；cacheCreate 缺省时显式按 input 价计费，不混入 raw input。 */
const MODEL_PRICES: Record<string, { input: number; output: number; cacheRead: number; cacheCreate?: number }> = {
  // Anthropic
  "claude-3-7-sonnet": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-sonnet-4-20250514": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-sonnet-4-5": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-sonnet-4": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-opus-4-1": { input: 15, output: 75, cacheRead: 1.50 },
  "claude-opus-4": { input: 15, output: 75, cacheRead: 1.50 },
  "claude-3-5-sonnet": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-3-5-haiku": { input: 0.8, output: 4, cacheRead: 0.08 },
  "claude-3-opus": { input: 15, output: 75, cacheRead: 1.50 },
  "claude-3-haiku": { input: 0.25, output: 1.25, cacheRead: 0.025 },
  // OpenAI
  "gpt-5-nano": { input: 0.05, output: 0.40, cacheRead: 0.005 },
  "gpt-5-mini": { input: 0.25, output: 2, cacheRead: 0.025 },
  "gpt-5": { input: 1.25, output: 10, cacheRead: 0.125 },
  "gpt-4.1-nano": { input: 0.10, output: 0.40, cacheRead: 0.025 },
  "gpt-4.1-mini": { input: 0.40, output: 1.60, cacheRead: 0.10 },
  "gpt-4.1": { input: 2, output: 8, cacheRead: 0.50 },
  "gpt-4o-mini": { input: 0.15, output: 0.6, cacheRead: 0.075 },
  "gpt-4o": { input: 2.5, output: 10, cacheRead: 1.25 },
  "gpt-4-turbo": { input: 10, output: 30, cacheRead: 5 },
  // DeepSeek
  "deepseek-v3.2": { input: 0.28, output: 0.42, cacheRead: 0.028 },
  "deepseek-v3": { input: 0.27, output: 1.10, cacheRead: 0.07 },
  "deepseek-r1": { input: 0.55, output: 2.19, cacheRead: 0.14 },
  // Google
  "gemini-2.5-pro": { input: 1.25, output: 10, cacheRead: 0.125 },
  "gemini-2.5-flash": { input: 0.30, output: 2.50, cacheRead: 0.03 },
};
export function estimateCost(model: string, tokens: Pick<TokenSummary, "input" | "output" | "cacheRead" | "cacheCreate">): number {
  const key = Object.keys(MODEL_PRICES).find((candidate) => model.toLowerCase().includes(candidate));
  const price = key ? MODEL_PRICES[key] : { input: 2, output: 10, cacheRead: 0.5, cacheCreate: 2 };
  const billable = (value: number) => Number.isFinite(value) ? Math.max(0, value) : 0;
  return (billable(tokens.input) * price.input
    + billable(tokens.cacheRead) * price.cacheRead
    + billable(tokens.cacheCreate) * (price.cacheCreate ?? price.input)
    + billable(tokens.output) * price.output) / 1_000_000;
}

export function generationModel(generation: any): string {
  return generation?.model
    || generation?.modelName
    || generation?.providedModelName
    || generation?.internalModelId
    || generation?.metadata?.attributes?.["langfuse.observation.model.name"]
    || "unknown";
}

/** 按 generation 自身模型分别计价，避免 mixed-model trace/session 被单一模型误计。 */
export function estimateGenerationsCost(generations: any[]): number {
  return generations.reduce(
    (total, generation) => total + estimateCost(generationModel(generation), genTokens(generation)),
    0,
  );
}
export function fmtCost(usd: number) { return usd < 0.01 ? `$${usd.toFixed(4)}` : `$${usd.toFixed(2)}`; }

export type ErrorCategory = "provider_or_stream_failure" | "tool_failure" | "cancelled_or_user_aborted" | "timeout" | "rate_limit" | "max_iterations" | "unknown_failure";
export interface ErrorSummary { hasError: boolean; categories: ErrorCategory[]; failedLlmCalls: number; failedToolCalls: number; }
const ERROR_CLASS_CATEGORIES: Record<string, ErrorCategory> = {
  llm_failure: "provider_or_stream_failure",
  provider_or_stream_failure: "provider_or_stream_failure",
  tool_failure: "tool_failure",
  timeout: "timeout",
  rate_limit: "rate_limit",
  max_iterations: "max_iterations",
};
function structuredStatus(observation: any) {
  return String(observation?.status || observation?.metadata?.status || observation?.metadata?.error_class || "").toUpperCase();
}
export function summarizeErrors(observations: any[]): ErrorSummary {
  const categories = new Set<ErrorCategory>();
  let failedLlmCalls = 0;
  let failedToolCalls = 0;
  for (const observation of observations) {
    const status = structuredStatus(observation);
    const errorClass = String(observation?.metadata?.error_class || observation?.output?.error_class || "").toLowerCase();
    const category = ERROR_CLASS_CATEGORIES[errorClass];
    const isError = String(observation?.level || "").toUpperCase() === "ERROR" || Boolean(category) || ["ERROR", "FAILED", "CANCELLED", "ABORTED", "INTERRUPTED"].includes(status);
    if (!isError) continue;
    if (["CANCELLED", "ABORTED", "INTERRUPTED"].includes(status)) categories.add("cancelled_or_user_aborted");
    else if (category) {
      categories.add(category);
      if (category === "provider_or_stream_failure" && observation?.type === "GENERATION") failedLlmCalls++;
      if (category === "tool_failure" && observation?.type === "TOOL") failedToolCalls++;
    }
    else if (observation?.type === "TOOL") { categories.add("tool_failure"); failedToolCalls++; }
    else if (observation?.type === "GENERATION") { categories.add("provider_or_stream_failure"); failedLlmCalls++; }
    else categories.add("unknown_failure");
  }
  return { hasError: categories.size > 0, categories: [...categories].sort(), failedLlmCalls, failedToolCalls };
}

export interface Anomaly { type: "cache_drop" | "high_effective" | "high_latency" | "loop" | "empty_output" | "tiny_output"; severity: "low" | "medium" | "high"; description: string; traceId?: string; details?: string; }
export function detectAnomalies(metrics: TraceMetrics, _model?: string): Anomaly[] {
  const anomalies: Anomaly[] = [];
  const inputTraffic = totalInputTraffic({ input: metrics.inputTokens, cacheRead: metrics.cacheReadTokens, cacheCreate: metrics.cacheCreateTokens });
  if (metrics.effectiveNewTokens > 20_000) anomalies.push({ type: "high_effective", severity: "high", description: `有效新 token = ${fmt(metrics.effectiveNewTokens)} (>20K)，上下文可能持续膨胀` });
  if (inputTraffic > 5000 && metrics.cacheReadTokens / inputTraffic < 0.1) anomalies.push({ type: "cache_drop", severity: "medium", description: `缓存读取占输入流量仅 ${pct(metrics.cacheReadTokens, inputTraffic)}，可能是新 session 或 prompt 变更` });
  if (metrics.inputTokens > 100_000 && metrics.outputTokens / metrics.inputTokens < 0.001) anomalies.push({ type: "tiny_output", severity: "medium", description: `输出/输入比极低 ${pct(metrics.outputTokens, metrics.inputTokens)}，大量上下文可能无用` });
  if (metrics.latency.seconds !== null && metrics.latency.seconds > 120) anomalies.push({ type: "high_latency", severity: "low", description: `真实耗时 ${fmtLatency(metrics.latency)} (>2min)，单个 trace 耗时较长` });
  if (metrics.llmCalls > 10 || metrics.toolCalls > 10) {
    const high = metrics.effectiveNewTokens > 20_000 || (metrics.latency.seconds !== null && metrics.latency.seconds > 120);
    anomalies.push({ type: "loop", severity: high ? "high" : "medium", description: `长 loop：LLM=${metrics.llmCalls}，Tools=${metrics.toolCalls}，有效新增=${fmt(metrics.effectiveNewTokens)}，真实耗时=${fmtLatency(metrics.latency)}` });
  }
  return anomalies;
}

export interface AgentSegment { agentObservationId: string | null; agentLabel: string; generationIds: string[]; generations: any[]; }
function observationTime(observation: any, originalIndex: number) {
  const value = Date.parse(observation?.startTime || observation?.timestamp || "");
  return Number.isFinite(value) ? value : Number.MAX_SAFE_INTEGER + originalIndex;
}
/** 按 generation 最近的 AGENT 父节点拆分连续段；未知归属绝不与已知 agent 合并。 */
export function splitGenerationAgentSegments(observations: any[], generations?: any[]): AgentSegment[] {
  const byId = new Map(observations.filter((observation) => observation?.id).map((observation) => [observation.id, observation]));
  const ordered = (generations || observations.filter((observation) => observation?.type === "GENERATION")).map((generation, index) => ({ generation, index })).sort((a, b) => observationTime(a.generation, a.index) - observationTime(b.generation, b.index));
  const owner = (generation: any) => {
    let current = generation;
    const visited = new Set<string>();
    while (current?.parentObservationId && !visited.has(current.parentObservationId)) {
      const parentId = current.parentObservationId;
      visited.add(parentId);
      const parent = byId.get(parentId);
      if (!parent) break;
      if (parent.type === "AGENT") return { id: parent.id as string, label: parent.name === "agent-run" || String(parent.name || "").startsWith("subagent-") ? parent.name : "unknown" };
      current = parent;
    }
    return { id: null, label: "unknown" };
  };
  const segments: AgentSegment[] = [];
  for (const { generation } of ordered) {
    const identity = owner(generation);
    const previous = segments.at(-1);
    if (!previous || previous.agentObservationId !== identity.id) segments.push({ agentObservationId: identity.id, agentLabel: identity.label, generationIds: [], generations: [] });
    const segment = segments.at(-1)!;
    segment.generationIds.push(String(generation.id || "unknown"));
    segment.generations.push(generation);
  }
  return segments;
}
