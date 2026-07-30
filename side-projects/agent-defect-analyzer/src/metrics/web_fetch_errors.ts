/**
 * WebFetch 专项错误分析——用 side-project 方式从真实使用记录
 * 中提取 WebFetch 的所有错误场景。
 *
 * 用法：bun run src/metrics/web_fetch_errors.ts
 */

import { DataLoader, type ThreadRow } from "../data/loader.js";
import {
  pct,
  printHeader,
  printSection,
  printMetric,
  printWarning,
} from "../lib/utils.js";

/** 简易表格输出，绕过 printTable 的类型问题 */
function simpleTable(
  rows: { category: string; count: number; pct: string }[],
  col1: string,
  col2: string,
  col3: string,
) {
  console.log(`  ${col1.padEnd(40)} ${col2.padEnd(10)} ${col3}`);
  console.log(`  ${"-".repeat(40)} ${"-".repeat(10)} ${"-".repeat(8)}`);
  for (const r of rows) {
    console.log(`  ${r.category.padEnd(40)} ${String(r.count).padEnd(10)} ${r.pct}`);
  }
}

function simpleUrlTable(rows: [string, number][]) {
  console.log(`  ${"URL/域名".padEnd(55)} 次数`);
  console.log(`  ${"-".repeat(55)} ${"-".repeat(6)}`);
  for (const [url, count] of rows) {
    console.log(`  ${url.padEnd(55)} ${count}`);
  }
}

interface WebFetchEvent {
  threadId: string;
  /** tool_use 中的 url */
  url: string;
  /** tool_use 中的 prompt (可选) */
  prompt?: string;
  /** 成功内容 / 错误消息 */
  resultContent: string;
  /** is_error 标记 */
  isError: boolean;
  /** 结果长度（字节） */
  resultSize: number;
  /** 是否包含截断提示 */
  isTruncated: boolean;
  /** 是否包含落盘路径 */
  hasPersistPath: boolean;
}

// ── Main ──

const loader = new DataLoader();

printHeader("WebFetch 专项错误分析");
const allThreads = loader.loadVisibleThreads();
printMetric("可见会话数", allThreads.length);

// 收集所有 WebFetch 事件（完整的 tool_use → tool_result 配对）
const events = collectWebFetchEvents(loader, allThreads);

printMetric("WebFetch 调用总数", events.length);

if (events.length === 0) {
  printWarning("未找到任何 WebFetch 调用记录");
  loader.close();
  process.exit(0);
}

// ── 分析维度 ──

// 1. 成功率 / 失败率
const successes = events.filter((e) => !e.isError);
const errors = events.filter((e) => e.isError);
const truncated = events.filter((e) => e.isTruncated);
const persisted = events.filter((e) => e.hasPersistPath);

printSection("一、整体统计");
printMetric("成功次数", successes.length);
printMetric("失败次数 (is_error=true)", errors.length);
printMetric("成功率", pct(successes.length, events.length));
printMetric("截断次数", truncated.length);
printMetric("截断比例", pct(truncated.length, events.length));
printMetric("落盘次数", persisted.length);
printMetric("落盘比例", pct(persisted.length, events.length));

// 2. 错误分类
printSection("二、错误内容分类");
const errorCategories = categorizeErrors(errors);
simpleTable(errorCategories, "错误类型", "次数", "占比");

// 3. 错误内容样本
printSection("三、错误内容样本 (前 30 条)");
let shown = 0;
for (const e of errors) {
  if (shown >= 30) break;
  const preview = e.resultContent.substring(0, 200).replace(/\n/g, "\\n");
  console.log(`  [${e.threadId.substring(0, 8)}] url=${e.url}`);
  console.log(`    ${preview}...`);
  console.log();
  shown++;
}

// 4. 截断统计
printSection("四、截断详情");
const truncSizes = truncated.map((e) => e.resultSize);
if (truncSizes.length > 0) {
  printMetric("截断结果平均大小", `${(truncSizes.reduce((a, b) => a + b, 0) / truncSizes.length / 1024).toFixed(1)} KB`);
  printMetric("截断结果最大大小", `${(Math.max(...truncSizes) / 1024).toFixed(1)} KB`);
}

// 5. 最多的目标 URL
printSection("五、高频 target URL (Top-10)");
const urlCounts = new Map<string, number>();
for (const e of events) {
  const host = extractHost(e.url);
  urlCounts.set(host, (urlCounts.get(host) || 0) + 1);
}
const topUrls = [...urlCounts.entries()]
  .sort((a, b) => b[1] - a[1])
  .slice(0, 10);
simpleUrlTable(topUrls);

// 6. 空内容检查
printSection("六、空内容 / 无内容提取");
const emptyContent = events.filter((e) => {
  const c = e.resultContent.trim();
  return c === "" || c.includes("No content extracted");
});
printMetric("空/无内容次数", emptyContent.length);
for (const e of emptyContent.slice(0, 10)) {
  console.log(`  url=${e.url} isError=${e.isError}`);
}

// 7. check failed_results 模式
printSection("七、包含 \"Extract failed\" 字样的错误");
const extractFailedErrors = errors.filter(
  (e) => e.resultContent.includes("Extract failed")
);
printMetric("Extract failed 错误数", extractFailedErrors.length);
for (const e of extractFailedErrors.slice(0, 20)) {
  console.log(`  url=${e.url}`);
  console.log(`  ${e.resultContent.substring(0, 200)}`);
  console.log();
}

// 8. 网络层错误
printSection("八、网络层错误 (包含 request/connect/timeout/DNS 关键字)");
const networkErrors = errors.filter(
  (e) =>
    e.resultContent.includes("Extract request failed") ||
    e.resultContent.includes("request failed") ||
    e.resultContent.includes("timeout") ||
    e.resultContent.includes("connect") ||
    e.resultContent.includes("DNS") ||
    e.resultContent.includes("Failed to build HTTP client")
);
printMetric("网络层错误数", networkErrors.length);
for (const e of networkErrors.slice(0, 20)) {
  console.log(`  url=${e.url}`);
  console.log(`  ${e.resultContent.substring(0, 200)}`);
  console.log();
}

// 9. HTTP 状态码错误
printSection("九、HTTP 状态码错误 (包含 \"HTTP\" 关键字)");
const httpErrors = errors.filter(
  (e) => e.resultContent.includes("HTTP") && e.resultContent.includes("returned")
);
printMetric("HTTP 状态码错误数", httpErrors.length);
for (const e of httpErrors.slice(0, 20)) {
  console.log(`  url=${e.url}`);
  console.log(`  ${e.resultContent.substring(0, 200)}`);
  console.log();
}

// 10. JSON 解析错误
printSection("十、JSON 解析错误 (包含 \"parse\" 关键字)");
const parseErrors = errors.filter(
  (e) => e.resultContent.includes("parse") ||
       e.resultContent.includes("JSON") ||
       e.resultContent.includes("serde")
);
printMetric("JSON 解析错误数", parseErrors.length);
for (const e of parseErrors.slice(0, 20)) {
  console.log(`  url=${e.url}`);
  console.log(`  ${e.resultContent.substring(0, 200)}`);
  console.log();
}

printSection("分析完成");
loader.close();


// ═══════════════════════════════════════════════════
// 数据收集
// ═══════════════════════════════════════════════════

function collectWebFetchEvents(
  loader: DataLoader,
  threads: ThreadRow[]
): WebFetchEvent[] {
  const out: WebFetchEvent[] = [];

  for (const thread of threads) {
    const messages = loader.loadMessages(thread.id);

    // 第一遍：收集所有 tool_use (WebFetch 的 url + prompt)
    const toolUseMap = new Map<string, { url: string; prompt?: string }>();
    for (const m of messages) {
      if (m.role !== "assistant") continue;
      const parsed = DataLoader.parseContent(m.content);
      const toolCalls = DataLoader.extractToolCalls(parsed);
      for (const tc of toolCalls) {
        if (tc.name !== "WebFetch") continue;
        toolUseMap.set(tc.id, {
          url: tc.arguments?.url as string || "",
          prompt: tc.arguments?.prompt as string | undefined,
        });
      }
    }

    // 第二遍：收集所有 tool_result（匹配 WebFetch 调用）
    for (const m of messages) {
      if (m.role !== "tool") continue;
      const parsed = DataLoader.parseContent(m.content);
      if (!parsed || parsed.role !== "tool") continue;
      const tc = parsed as any;

      if (!toolUseMap.has(tc.tool_call_id)) continue;
      const meta = toolUseMap.get(tc.tool_call_id)!;

      const content = typeof tc.content === "string"
        ? tc.content
        : JSON.stringify(tc.content ?? "");

      const isTruncated =
        content.includes("Content truncated") ||
        content.includes("exceeds") ||
        content.includes("byte limit");

      const hasPersistPath = content.includes("saved to ");

      out.push({
        threadId: thread.id,
        url: meta.url,
        prompt: meta.prompt,
        resultContent: content,
        isError: !!tc.is_error,
        resultSize: Buffer.byteLength(content, "utf8"),
        isTruncated,
        hasPersistPath,
      });
    }
  }

  return out;
}

// ── 错误分类 ──

function categorizeErrors(
  errors: WebFetchEvent[]
): { category: string; count: number; pct: string }[] {
  const cats: Record<string, number> = {
    "缺少 url 参数": 0,
    "Extract request failed (网络层)": 0,
    "HTTP 状态码错误": 0,
    "Extract failed (Tavily语义)": 0,
    "JSON 解析失败": 0,
    "其他未分类错误": 0,
  };

  for (const e of errors) {
    const msg = e.resultContent;
    if (msg.includes("Missing url parameter")) {
      cats["缺少 url 参数"]++;
    } else if (
      msg.includes("Extract request failed") ||
      msg.includes("Failed to build HTTP client")
    ) {
      cats["Extract request failed (网络层)"]++;
    } else if (msg.includes("HTTP") && msg.includes("returned")) {
      cats["HTTP 状态码错误"]++;
    } else if (msg.includes("Extract failed")) {
      cats["Extract failed (Tavily语义)"]++;
    } else if (msg.includes("parse") || msg.includes("JSON") || msg.includes("serde")) {
      cats["JSON 解析失败"]++;
    } else {
      cats["其他未分类错误"]++;
    }
  }

  const total = errors.length;
  return Object.entries(cats)
    .filter(([, count]) => total === 0 || count > 0)
    .map(([category, count]) => ({
      category,
      count,
      pct: pct(count, total),
    }));
}

function extractHost(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    // 可能是相对路径或无效 URL，取前 60 字符
    return url.substring(0, 60);
  }
}
