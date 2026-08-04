/**
 * Langfuse API 客户端 —— 供其他脚本 import
 *
 * 自动从 .env 读取 LANGFUSE_HOST / LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY
 */
const BASE_URL = (process.env.LANGFUSE_HOST || process.env.LANGFUSE_BASE_URL || "").replace(/\/$/, "");
const PUBLIC_KEY = process.env.LANGFUSE_PUBLIC_KEY || "";
const SECRET_KEY = process.env.LANGFUSE_SECRET_KEY || "";

if (!BASE_URL || !PUBLIC_KEY || !SECRET_KEY) {
  console.error("Missing LANGFUSE_HOST/PUBLIC_KEY/SECRET_KEY env vars");
  process.exit(1);
}

const authHeader = `Basic ${btoa(`${PUBLIC_KEY}:${SECRET_KEY}`)}`;

export async function api(path: string) {
  const res = await fetch(`${BASE_URL}${path}`, {
    headers: { Authorization: authHeader, "Content-Type": "application/json" },
  });
  if (!res.ok) throw new Error(`API ${path}: ${res.status} ${await res.text()}`);
  return res.json();
}

export async function fetchTraces(limit: number) {
  const data = await api(`/api/public/traces?limit=${limit}`);
  return (data.data || []) as any[];
}

// ═══════════════════════════════════════════════════════════════
// 增强：带过滤的分页 trace 查询
// ═══════════════════════════════════════════════════════════════

export interface TraceFilter {
  limit?: number;
  page?: number;
  fromTimestamp?: string;
  toTimestamp?: string;
  tags?: string[];
  userId?: string;
  sessionId?: string;
  name?: string;
  orderBy?: string; // e.g. "timestamp.desc"
}

export async function fetchTracesFiltered(
  filter: TraceFilter
): Promise<{ traces: any[]; meta: any }> {
  const params = new URLSearchParams();
  if (filter.limit) params.set("limit", String(filter.limit));
  if (filter.page) params.set("page", String(filter.page));
  if (filter.fromTimestamp) params.set("fromTimestamp", filter.fromTimestamp);
  if (filter.toTimestamp) params.set("toTimestamp", filter.toTimestamp);
  if (filter.tags && filter.tags.length > 0) params.set("tags", filter.tags.join(","));
  if (filter.userId) params.set("userId", filter.userId);
  if (filter.sessionId) params.set("sessionId", filter.sessionId);
  if (filter.name) params.set("name", filter.name);
  if (filter.orderBy) params.set("orderBy", filter.orderBy);

  const data = await api(`/api/public/traces?${params.toString()}`);
  return { traces: (data.data || []) as any[], meta: data.meta || {} };
}

/** 带过滤的全量分页抓取 */
export async function fetchAllTracesFiltered(
  filter: Omit<TraceFilter, "page"> & { maxPages?: number }
): Promise<any[]> {
  const all: any[] = [];
  let page = 1;
  const maxPages = filter.maxPages ?? 20;
  while (page <= maxPages) {
    const { traces, meta } = await fetchTracesFiltered({ ...filter, page, limit: filter.limit || 50 });
    all.push(...traces);
    if (page >= (meta.totalPages || 1) || traces.length === 0) break;
    page++;
  }
  return all;
}

export async function fetchObservations(traceId: string) {
  const all: any[] = [];
  let page = 1;
  while (true) {
    const data = await api(`/api/public/observations?traceId=${traceId}&limit=100&page=${page}`);
    const items = (data.data || []) as any[];
    all.push(...items);
    const meta = data.meta || {};
    if (page >= (meta.totalPages || 1)) break;
    page++;
  }
  return all;
}

export async function fetchScores(traceId: string) {
  const all: any[] = [];
  let page = 1;
  while (true) {
    const data = await api(`/api/public/scores?traceId=${traceId}&limit=100&page=${page}`);
    const items = (data.data || []) as any[];
    all.push(...items);
    const meta = data.meta || {};
    if (page >= (meta.totalPages || 1)) break;
    page++;
  }
  return all;
}

// ═══════════════════════════════════════════════════════════════
// 时间工具
// ═══════════════════════════════════════════════════════════════

export function isoNow() {
  return new Date().toISOString();
}

export function isoSpan(fromISO: string, days: number) {
  const d = new Date(fromISO);
  d.setDate(d.getDate() - days);
  return d.toISOString();
}

/**
 * 单 trace 脚本的位置参数解析：第一个非选项、非选项值参数是 traceId，--index <N> 是索引。
 *
 * 修复：原实现 `args[args.indexOf("--index") + 1]` 在选项缺失时 indexOf 返回 -1，
 * 会把 args[0]（即 traceId 本身）当作选项值跳过，导致 `bun trace-tokens.ts <traceId>`
 * 永远报 Usage 错误；若同时存在其它数值选项（如 --days 7），"7" 反而会被误判为 traceId。
 */
export function parseTraceArg(args: string[]): { traceId?: string; index?: number } {
  const valueFlags = ["--index", "--from", "--to", "--days", "--tag", "--user", "--session", "--name", "--limit"];
  const valuePositions = new Set<number>();
  for (const flag of valueFlags) {
    const idx = args.indexOf(flag);
    if (idx !== -1 && args[idx + 1] !== undefined) valuePositions.add(idx + 1);
  }

  let traceId: string | undefined;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.startsWith("--") || valuePositions.has(i)) continue;
    if (!traceId) traceId = a;
  }

  const indexIdx = args.indexOf("--index");
  let index: number | undefined;
  if (indexIdx !== -1 && args[indexIdx + 1] !== undefined) {
    const n = Number(args[indexIdx + 1]);
    if (Number.isInteger(n) && n > 0) index = n;
  }
  return { traceId, index };
}

/** 从命令行解析时间窗: --days N / --from ISO --to ISO */
export function parseTimeWindow(args: string[]): { from?: string; to?: string } {
  let from: string | undefined;
  let to: string | undefined;

  const fromIdx = args.indexOf("--from");
  const toIdx = args.indexOf("--to");
  const daysIdx = args.indexOf("--days");

  if (fromIdx !== -1 && args[fromIdx + 1]) from = args[fromIdx + 1];
  if (toIdx !== -1 && args[toIdx + 1]) to = args[toIdx + 1];

  if (daysIdx !== -1 && args[daysIdx + 1]) {
    const days = parseInt(args[daysIdx + 1]);
    if (!isNaN(days)) {
      if (!to) to = isoNow();
      if (!from) from = isoSpan(to, days);
    }
  }

  return { from, to };
}

/** 从命令行解析通用过滤参数 */
export function parseFilterArgs(args: string[]): {
  tag?: string;
  userId?: string;
  sessionId?: string;
  name?: string;
  model?: string;
  limit: number;
  time: { from?: string; to?: string };
} {
  const tagIdx = args.indexOf("--tag");
  const userIdx = args.indexOf("--user");
  const sessIdx = args.indexOf("--session");
  const nameIdx = args.indexOf("--name");
  const modelIdx = args.indexOf("--model");
  const limitIdx = args.indexOf("--limit");

  let limit = 50;
  if (limitIdx !== -1 && args[limitIdx + 1]) {
    const n = parseInt(args[limitIdx + 1]);
    if (!isNaN(n) && n > 0) limit = n;
  }

  // 收集所有带值选项的值位置，避免选项值被误判为位置数字
  // 修复：--from/--to 的 ISO 时间戳（如 "2026-08-03T13:40:00Z"）此前未被跳过，
  // parseInt 得到 2026 被当作 limit，超过 API 上限 100 导致 400 错误
  const valueFlags = ["--from", "--to", "--days", "--tag", "--user", "--session", "--name", "--model", "--limit"];
  const valuePositions = new Set<number>();
  for (const flag of valueFlags) {
    const idx = args.indexOf(flag);
    if (idx !== -1 && args[idx + 1] !== undefined) valuePositions.add(idx + 1);
  }

  // Find positional number arg (backward compat)
  for (let i = 0; i < args.length; i++) {
    if (valuePositions.has(i)) continue;
    const n = parseInt(args[i]);
    if (!isNaN(n) && n > 0) {
      limit = n;
      break;
    }
  }

  return {
    tag: tagIdx !== -1 ? args[tagIdx + 1] : undefined,
    userId: userIdx !== -1 ? args[userIdx + 1] : undefined,
    sessionId: sessIdx !== -1 ? args[sessIdx + 1] : undefined,
    name: nameIdx !== -1 ? args[nameIdx + 1] : undefined,
    model: modelIdx !== -1 ? args[modelIdx + 1] : undefined,
    limit,
    time: parseTimeWindow(args),
  };
}

// ═══════════════════════════════════════════════════════════════
// 格式化工具
// ═══════════════════════════════════════════════════════════════

export function fmt(n: number) {
  return n.toLocaleString();
}

export function pct(part: number, whole: number) {
  return whole > 0 ? `${((part / whole) * 100).toFixed(1)}%` : "-";
}

export function bar(pct: number, width = 20) {
  const filled = Math.round((pct / 100) * width);
  return "\u2588".repeat(filled) + "\u2591".repeat(width - filled);
}

export function ms(seconds: number) {
  if (seconds < 1) return `${(seconds * 1000).toFixed(0)}ms`;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}m${s.toFixed(0)}s`;
}

export function isoToLocal(iso: string) {
  try {
    return new Date(iso).toLocaleString("zh-CN", {
      month: "2-digit", day: "2-digit",
      hour: "2-digit", minute: "2-digit",
    });
  } catch {
    return iso.slice(0, 16);
  }
}

// ═══════════════════════════════════════════════════════════════
// Token 分析
// ═══════════════════════════════════════════════════════════════

export function genTokens(g: any) {
  const u = g.usageDetails || (g.usage as any) || {};
  return {
    input: (u.input || u.prompt_tokens || 0) as number,
    output: (u.output || u.completion_tokens || 0) as number,
    cacheRead: (u.cache_read_input_tokens || 0) as number,
    cacheCreate: (u.cache_creation_input_tokens || 0) as number,
  };
}

export interface TokenSummary {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreate: number;
  effective: number; // input - cacheRead
  calls: number;
}

export function summarizeTokens(generations: any[]): TokenSummary {
  let input = 0, output = 0, cacheRead = 0, cacheCreate = 0;
  for (const g of generations) {
    const tk = genTokens(g);
    input += tk.input;
    output += tk.output;
    cacheRead += tk.cacheRead;
    cacheCreate += tk.cacheCreate;
  }
  return { input, output, cacheRead, cacheCreate, effective: input - cacheRead, calls: generations.length };
}

// ═══════════════════════════════════════════════════════════════
// 成本估算（近似定价 USD per 1M tokens）
// ═══════════════════════════════════════════════════════════════

const MODEL_PRICES: Record<string, { input: number; output: number; cacheRead: number }> = {
  "claude-sonnet-4-20250514": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-3-5-sonnet": { input: 3, output: 15, cacheRead: 0.30 },
  "claude-3-5-haiku": { input: 0.8, output: 4, cacheRead: 0.08 },
  "claude-3-opus": { input: 15, output: 75, cacheRead: 1.50 },
  "claude-3-haiku": { input: 0.25, output: 1.25, cacheRead: 0.025 },
  "gpt-4o": { input: 2.5, output: 10, cacheRead: 1.25 },
  "gpt-4o-mini": { input: 0.15, output: 0.6, cacheRead: 0.075 },
  "gpt-4-turbo": { input: 10, output: 30, cacheRead: 5 },
  "deepseek-v3": { input: 0.27, output: 1.10, cacheRead: 0.07 },
  "deepseek-r1": { input: 0.55, output: 2.19, cacheRead: 0.14 },
  "gemini-2.5-pro": { input: 1.25, output: 10, cacheRead: 0.3125 },
  "gemini-2.5-flash": { input: 0.15, output: 0.6, cacheRead: 0.0375 },
};

/** 根据模型名和 token 用量估算成本（USD） */
export function estimateCost(model: string, t: TokenSummary): number {
  const key = Object.keys(MODEL_PRICES).find((k) => model.toLowerCase().includes(k));
  const price = key ? MODEL_PRICES[key] : { input: 2, output: 10, cacheRead: 0.5 };
  // Anthropic 类 provider 将 cache_read 单独计费（input 不含 cache），差值可能为负，钳制到 0
  const inputCost = (Math.max(0, t.input - t.cacheRead) / 1_000_000) * price.input;
  const cacheCost = (t.cacheRead / 1_000_000) * price.cacheRead;
  const outputCost = (t.output / 1_000_000) * price.output;
  return inputCost + cacheCost + outputCost;
}

/** 格式化成本为美元 */
export function fmtCost(usd: number) {
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}

// ═══════════════════════════════════════════════════════════════
// 异常检测
// ═══════════════════════════════════════════════════════════════

export interface Anomaly {
  type: "cache_drop" | "high_effective" | "high_latency" | "loop" | "empty_output" | "tiny_output";
  severity: "low" | "medium" | "high";
  description: string;
  traceId?: string;
  details?: string;
}

export function detectAnomalies(t: TokenSummary, latency: number, model: string): Anomaly[] {
  const a: Anomaly[] = [];

  // 缓存骤降（有效新 token 超过 20K）
  if (t.effective > 20_000) {
    a.push({ type: "high_effective", severity: "high",
      description: `有效新 token = ${fmt(t.effective)} (>20K)，上下文可能持续膨胀`,
      details: `input=${fmt(t.input)} cacheRead=${fmt(t.cacheRead)}` });
  }

  // 缓存命中率低
  if (t.input > 5000 && t.cacheRead / t.input < 0.1) {
    a.push({ type: "cache_drop", severity: "medium",
      description: `缓存命中率仅 ${pct(t.cacheRead, t.input)}，可能是新 session 或 prompt 变更` });
  }

  // 输出占比极低
  if (t.input > 100_000 && t.output / t.input < 0.001) {
    a.push({ type: "tiny_output", severity: "medium",
      description: `输出/输入比极低 ${pct(t.output, t.input)}，大量上下文可能无用` });
  }

  // LLM 调用过多
  if (t.calls > 10) {
    a.push({ type: "loop", severity: "medium",
      description: `${t.calls} 次 LLM 调用，可能存在 agent loop` });
  }

  // 延迟过高
  if (latency > 120) {
    a.push({ type: "high_latency", severity: "low",
      description: `延迟 ${ms(latency)} (>2min)，单个 trace 耗时过长` });
  }

  return a;
}
