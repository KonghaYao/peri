//! 场景三：资源消耗分析。
//!
//! 5 项指标：编辑工具入参大小、出参大小、超大入参检测、超大出参检测、手动 Compact 触发频率。
//! 注：当前仅剩 Write 作为编辑工具（Edit/LineEdit/HashlineEdit 已移除）。
//!
//! 用法：bun run src/metrics/resource_consumption.ts [--since 24]

import { DataLoader, type ThreadRow, type MessageRow, type ParsedMessage, type AiContent, type ContentBlock, type ToolCallRequest } from "../data/loader.js";
import { avg, median, p50, p95, pct, formatSize, formatDuration, parseSinceArg, printHeader, printSection, printMetric, printWarning, printTable, printBar, printSeparator } from "../lib/utils.js";

// ── 常量 ──

const EDIT_TOOLS = ["Write"] as const;

/** 超大入参阈值（字节） */
const OVERSIZED_INPUT_THRESHOLDS: Record<string, number> = {
  Write: 50 * 1024,      // 50KB
};

/** 超大出参统一阈值 */
const OVERSIZED_OUTPUT_THRESHOLD = 20 * 1024; // 20KB

/** Compact 命令正则 */
const COMPACT_CMD_RE = /^\/compact\b/;

// ── 内部数据结构 ──

interface EditRecord {
  threadId: string;
  toolName: string;
  inputSize: number;
  inputJson: string;
  outputSize: number;
  isError: boolean;
}

// ── 入口 ──

const sinceHours = parseSinceArg();
const loader = new DataLoader();
if (sinceHours) printMetric("时间范围", `最近 ${sinceHours} 小时`);
const threads = sinceHours ? loader.loadVisibleThreadsSince(sinceHours) : loader.loadVisibleThreads();

printHeader("场景三：资源消耗");

// 收集所有编辑工具记录
const records = collectEditRecords(loader, threads);
printMetric("可见会话数", threads.length);
printMetric("编辑工具调用总数", records.length);

// 1. 编辑工具入参大小
analyzeInputSize(records);

// 2. 编辑工具出参大小
analyzeOutputSize(records);

// 3. 超大入参检测
analyzeOversizedInput(records);

// 4. 超大出参检测
analyzeOversizedOutput(records);

// 5. 手动 Compact 触发频率
analyzeManualCompact(loader, threads);

loader.close();

// ═══════════════════════════════════════════════════
// 数据收集
// ═══════════════════════════════════════════════════

function collectEditRecords(loader: DataLoader, threads: ThreadRow[]): EditRecord[] {
  const records: EditRecord[] = [];

  for (const thread of threads) {
    const messages = loader.loadMessages(thread.id);

    // 建立 callId → output 映射
    const outputs = buildOutputMap(messages);

    // 提取 tool_use 记录
    for (const msg of messages) {
      if (msg.role !== "assistant") continue;
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "assistant") continue;

      const blocks = (parsed as AiContent).content;
      if (!Array.isArray(blocks)) continue;

      for (const block of blocks) {
        if (block.type !== "tool_use") continue;
        const name = block.name as string;
        if (!EDIT_TOOLS.includes(name as typeof EDIT_TOOLS[number])) continue;

        const inputJson = JSON.stringify(block.input || {});
        const inputSize = Buffer.byteLength(inputJson, "utf8");
        const output = outputs.get(block.id);

        records.push({
          threadId: thread.id,
          toolName: name,
          inputSize,
          inputJson,
          outputSize: output?.size ?? 0,
          isError: output?.isError ?? false,
        });
      }
    }
  }

  return records;
}

function buildOutputMap(messages: MessageRow[]): Map<string, { size: number; isError: boolean }> {
  const map = new Map<string, { size: number; isError: boolean }>();
  for (const msg of messages) {
    if (msg.role !== "tool") continue;
    const parsed = DataLoader.parseContent(msg.content);
    if (!parsed) continue;
    const toolResult = DataLoader.parseToolError(parsed);
    if (toolResult) {
      map.set(toolResult.toolCallId, {
        size: Buffer.byteLength(toolResult.content, "utf8"),
        isError: toolResult.isError,
      });
    }
  }
  return map;
}

// ═══════════════════════════════════════════════════
// 指标 1：编辑工具入参大小
// ═══════════════════════════════════════════════════

function analyzeInputSize(records: EditRecord[]): void {
  printSection("1. 编辑工具入参大小分布");

  const sizes = records.map((r) => r.inputSize);
  if (sizes.length === 0) {
    console.log("  无数据");
    return;
  }

  printMetric("总调用数", records.length);
  printMetric("入参平均", formatSize(Math.round(avg(sizes))));
  printMetric("入参 P50", formatSize(p50(sizes)));
  printMetric("入参 P95", formatSize(p95(sizes)));
  printMetric("入参最大", formatSize(Math.max(...sizes)));

  // 按工具分组
  const toolRows = EDIT_TOOLS.map((tool) => {
    const group = records.filter((r) => r.toolName === tool);
    if (group.length === 0) return [tool, "0", "-", "-", "-", "-"];
    const gs = group.map((r) => r.inputSize);
    return [
      tool,
      String(group.length),
      formatSize(Math.round(avg(gs))),
      formatSize(p50(gs)),
      formatSize(p95(gs)),
      formatSize(Math.max(...gs)),
    ];
  });

  printTable(["工具", "样本数", "平均", "P50", "P95", "最大"], toolRows);
}

// ═══════════════════════════════════════════════════
// 指标 2：编辑工具出参大小
// ═══════════════════════════════════════════════════

function analyzeOutputSize(records: EditRecord[]): void {
  printSection("2. 编辑工具出参大小分布");

  const recsWithOutput = records.filter((r) => r.outputSize > 0);
  if (recsWithOutput.length === 0) {
    console.log("  无出参数据");
    return;
  }

  const sizes = recsWithOutput.map((r) => r.outputSize);
  printMetric("有出参的调用数", recsWithOutput.length);
  printMetric("出参平均", formatSize(Math.round(avg(sizes))));
  printMetric("出参 P50", formatSize(p50(sizes)));
  printMetric("出参 P95", formatSize(p95(sizes)));
  printMetric("出参最大", formatSize(Math.max(...sizes)));

  const toolRows = EDIT_TOOLS.map((tool) => {
    const group = recsWithOutput.filter((r) => r.toolName === tool);
    if (group.length === 0) return [tool, "0", "-", "-", "-", "-"];
    const gs = group.map((r) => r.outputSize);
    return [
      tool,
      String(group.length),
      formatSize(Math.round(avg(gs))),
      formatSize(p50(gs)),
      formatSize(p95(gs)),
      formatSize(Math.max(...gs)),
    ];
  });

  printTable(["工具", "样本数", "平均", "P50", "P95", "最大"], toolRows);
}

// ═══════════════════════════════════════════════════
// 指标 3：超大入参检测
// ═══════════════════════════════════════════════════

function analyzeOversizedInput(records: EditRecord[]): void {
  printSection("3. 超大入参检测");

  const oversized: EditRecord[] = [];

  for (const r of records) {
    const threshold = OVERSIZED_INPUT_THRESHOLDS[r.toolName];
    if (threshold && r.inputSize > threshold) {
      oversized.push(r);
    }
  }

  if (oversized.length === 0) {
    console.log("  未发现超大入参");
    return;
  }

  printMetric("超大入参总数", oversized.length);
  const tt = { Write: "50KB" };
  printMetric("阈值说明", `Write>${tt.Write}`);

  oversized.sort((a, b) => b.inputSize - a.inputSize);
  const rows = oversized.map((r) => [
    r.threadId.slice(0, 12) + "...",
    r.toolName,
    formatSize(r.inputSize),
    extractFilePath(r.toolName, r.inputJson),
  ]);

  printTable(["会话ID", "工具", "入参大小", "文件路径"], rows);
}

function extractFilePath(toolName: string, inputJson: string): string {
  try {
    const input = JSON.parse(inputJson);
    // 通用 file_path
    if (input.file_path) return String(input.file_path);
    // diff header 正则
    if (input.diff && typeof input.diff === "string") {
      const m = input.diff.match(/\+\+\+ b\/(.+)$/m);
      if (m) return m[1];
    }
  } catch { /* ignore */ }
  return "(未知)";
}

// ═══════════════════════════════════════════════════
// 指标 4：超大出参检测
// ═══════════════════════════════════════════════════

function analyzeOversizedOutput(records: EditRecord[]): void {
  printSection("4. 超大出参检测");

  const oversized = records
    .filter((r) => r.outputSize > OVERSIZED_OUTPUT_THRESHOLD)
    .sort((a, b) => b.outputSize - a.outputSize);

  if (oversized.length === 0) {
    console.log(`  未发现超过 ${formatSize(OVERSIZED_OUTPUT_THRESHOLD)} 的工具出参`);
    return;
  }

  printMetric("超大出参总数", oversized.length);
  printMetric("阈值", formatSize(OVERSIZED_OUTPUT_THRESHOLD));

  const rows = oversized.map((r) => [
    r.threadId.slice(0, 12) + "...",
    r.toolName,
    formatSize(r.outputSize),
    r.inputJson.slice(0, 100),
  ]);

  printTable(["会话ID", "工具", "出参大小", "入参预览"], rows);
}

// ═══════════════════════════════════════════════════
// 指标 5：手动 Compact 触发频率
// ═══════════════════════════════════════════════════

function analyzeManualCompact(loader: DataLoader, threads: ThreadRow[]): void {
  printSection("5. 手动 Compact 触发频率");

  let totalCompact = 0;
  const sessionCompacts: { threadId: string; count: number }[] = [];

  for (const thread of threads) {
    const messages = loader.loadMessages(thread.id);
    let count = 0;

    for (const msg of messages) {
      if (msg.role !== "user") continue;
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed) continue;
      const text = extractText(parsed);
      if (COMPACT_CMD_RE.test(text.trim())) {
        count++;
      }
    }

    if (count > 0) {
      totalCompact += count;
      sessionCompacts.push({ threadId: thread.id, count });
    }
  }

  printMetric("手动 Compact 总次数", totalCompact);
  printMetric("有 Compact 的会话数", `${sessionCompacts.length} / ${threads.length}`);
  if (threads.length > 0) {
    printMetric("会话覆盖率", pct(sessionCompacts.length, threads.length));
  }

  if (sessionCompacts.length > 0) {
    sessionCompacts.sort((a, b) => b.count - a.count);
    printMetric("单会话最多 Compact 次数", Math.max(...sessionCompacts.map((s) => s.count)));
    const rows = sessionCompacts.map((s) => [
      s.threadId.slice(0, 20) + "...",
      String(s.count),
    ]);
    printTable(["会话ID", "Compact 次数"], rows);
  } else {
    console.log("  未发现手动 /compact 命令");
  }
}

function extractText(parsed: ParsedMessage): string {
  const content = (parsed as any).content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .filter((b: any) => b.type === "text")
      .map((b: any) => b.text || "")
      .join("");
  }
  return "";
}
