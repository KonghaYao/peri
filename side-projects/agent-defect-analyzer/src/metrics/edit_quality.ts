//! 场景五：编辑质量
//!
//! 3 项指标：纯验证重读率、编辑链重读率、Write 文件大小。
//! 注：仅剩 Write 作为编辑工具（Edit/LineEdit/HashlineEdit 已移除），LineEdit 连续编辑链指标已废弃。
//! 用法：bun run src/metrics/edit_quality.ts --since 24

import { DataLoader, type ThreadRow, type MessageRow, type ParsedMessage, type AiContent, type ContentBlock } from "../data/loader.js";
import { avg, median, p50, p95, pct, formatSize, parseSinceArg, printHeader, printSection, printMetric, printWarning, printTable, printBar, printSeparator } from "../lib/utils.js";

// ═══════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════

const EDIT_TOOLS = new Set(["Write"]);
const EDIT_READ_TOOLS = new Set(["Write", "Read"]);
const REREAD_WINDOW = 5;

// ═══════════════════════════════════════════════════
// 类型
// ═══════════════════════════════════════════════════

interface EditEvent {
  toolName: string;
  filePath: string;
  msgIndex: number;
  isError: boolean;
}

interface RereadStats {
  totalEdits: number;
  pureVerify: number;
  editChain: number;
  chainLengths: number[];
}

interface ByToolReread {
  [tool: string]: RereadStats;
}

// ═══════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════

function extractFilePath(callName: string, input: any): string {
  return input?.file_path || input?.path || "";
}

function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

function pathsMatch(a: string, b: string): boolean {
  if (a === b) return true;
  if (a.length > 10 && b.length > 10) {
    const minLen = Math.min(a.length, b.length);
    return a.endsWith(b.slice(-minLen)) || b.endsWith(a.slice(-minLen));
  }
  return false;
}

// ═══════════════════════════════════════════════════
// 事件序列构建
// ═══════════════════════════════════════════════════

/** 从单条消息提取 EditEvent（仅 edit/read 工具） */
function extractEventsFromMessages(messages: MessageRow[]): EditEvent[] {
  const events: EditEvent[] = [];
  const toolIdToEventIdx = new Map<string, number>();

  for (let mi = 0; mi < messages.length; mi++) {
    const parsed = DataLoader.parseContent(messages[mi].content);
    if (!parsed) continue;

    if (parsed.role === "assistant") {
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type !== "tool_use") continue;
        const name = block.name;
        if (!EDIT_READ_TOOLS.has(name)) continue;
        const input = block.input as Record<string, unknown> | undefined;
        const fp = extractFilePath(name, input);
        if (!fp) continue;
        events.push({
          toolName: name,
          filePath: normalizePath(fp),
          msgIndex: mi,
          isError: false,
        });
        toolIdToEventIdx.set(block.id, events.length - 1);
      }
    } else if (parsed.role === "tool") {
      const err = DataLoader.parseToolError(parsed);
      if (err && toolIdToEventIdx.has(err.toolCallId)) {
        events[toolIdToEventIdx.get(err.toolCallId)!].isError = err.isError;
      }
    }
  }

  return events;
}

/** 为所有可见 session 构建事件序列 */
function buildAllEventSequences(
  threads: ThreadRow[],
  loader: DataLoader,
): Map<string, EditEvent[]> {
  const map = new Map<string, EditEvent[]>();
  for (const t of threads) {
    const msgs = loader.loadMessages(t.id);
    const events = extractEventsFromMessages(msgs);
    if (events.length > 0) map.set(t.id, events);
  }
  return map;
}

// ═══════════════════════════════════════════════════
// 指标 1 & 2：重读率
// ═══════════════════════════════════════════════════

interface RereadResult {
  pureVerify: number;
  editChain: number;
  chainLengths: number[];
  byTool: Map<string, { totalEdits: number; pureVerify: number; editChain: number; chainLengths: number[] }>;
}

/** 按指定窗口计算重读率 */
function computeRereadRate(eventSeq: Map<string, EditEvent[]>, windowSize: number): RereadResult {
  let pureVerify = 0;
  let editChain = 0;
  const chainLengths: number[] = [];
  const byTool = new Map<string, { totalEdits: number; pureVerify: number; editChain: number; chainLengths: number[] }>();

  const ensureTool = (name: string) => {
    if (!byTool.has(name)) {
      byTool.set(name, { totalEdits: 0, pureVerify: 0, editChain: 0, chainLengths: [] });
    }
    return byTool.get(name)!;
  };

  for (const [, events] of eventSeq) {
    for (let i = 0; i < events.length; i++) {
      const ev = events[i];
      if (ev.toolName === "Read") continue;

      const toolStats = ensureTool(ev.toolName);
      toolStats.totalEdits++;

      // 在后续 windowSize 步内找第一个同文件 Read
      let readIdx = -1;
      const end = Math.min(i + windowSize + 1, events.length);
      for (let j = i + 1; j < end; j++) {
        if (events[j].toolName === "Read" && pathsMatch(ev.filePath, events[j].filePath)) {
          readIdx = j;
          break;
        }
      }

      if (readIdx === -1) continue; // 无重读

      // 检查 Read 之后是否有对同一文件的再次编辑
      let hasReEdit = false;
      let reEditCount = 0;
      for (let j = readIdx + 1; j < events.length; j++) {
        if (events[j].toolName !== "Read" && pathsMatch(ev.filePath, events[j].filePath)) {
          hasReEdit = true;
          reEditCount++;
        }
      }

      if (hasReEdit) {
        editChain++;
        toolStats.editChain++;
        chainLengths.push(reEditCount);
        toolStats.chainLengths.push(reEditCount);
      } else {
        pureVerify++;
        toolStats.pureVerify++;
      }
    }
  }

  return { pureVerify, editChain, chainLengths, byTool };
}

/** 计算并输出重读率（纯验证 + 编辑链） */
function printRereadAnalysis(eventSeq: Map<string, EditEvent[]>, windowSize: number, label: string): void {
  printSection(`${label}（窗口 ${windowSize} 步）`);

  const result = computeRereadRate(eventSeq, windowSize);
  const totalToolEdits = [...result.byTool.values()].reduce((s, t) => s + t.totalEdits, 0);

  if (totalToolEdits === 0) {
    printWarning("无编辑事件", "未检测到任何可提取文件路径的编辑事件");
    return;
  }

  const rereadTotal = result.pureVerify + result.editChain;

  // 纯验证重读表格
  const toolNames = ["Write"];
  const pureRows: string[][] = [];
  for (const name of toolNames) {
    const ts = result.byTool.get(name);
    if (!ts || ts.totalEdits === 0) continue;
    pureRows.push([
      name,
      String(ts.totalEdits),
      String(ts.pureVerify),
      pct(ts.pureVerify, ts.totalEdits),
    ]);
  }
  // 总计行
  pureRows.push([
    "合计",
    String(totalToolEdits),
    String(result.pureVerify),
    pct(result.pureVerify, totalToolEdits),
  ]);

  printMetric("纯验证重读", `${result.pureVerify} / ${totalToolEdits} = ${pct(result.pureVerify, totalToolEdits)}`);
  printTable(["工具", "有效编辑", "纯验证重读", "纯验证重读率"], pureRows);

  // 编辑链重读表格
  const chainRows: string[][] = [];
  for (const name of toolNames) {
    const ts = result.byTool.get(name);
    if (!ts || ts.totalEdits === 0) continue;
    const avgChain = ts.chainLengths.length > 0 ? avg(ts.chainLengths).toFixed(1) : "-";
    chainRows.push([
      name,
      String(ts.totalEdits),
      String(ts.editChain),
      pct(ts.editChain, ts.totalEdits),
      avgChain,
    ]);
  }
  chainRows.push([
    "合计",
    String(totalToolEdits),
    String(result.editChain),
    pct(result.editChain, totalToolEdits),
    result.chainLengths.length > 0 ? avg(result.chainLengths).toFixed(1) : "-",
  ]);

  console.log("");
  printMetric("编辑链重读", `${result.editChain} / ${totalToolEdits} = ${pct(result.editChain, totalToolEdits)}`);
  printTable(["工具", "有效编辑", "编辑链重读", "编辑链重读率", "平均链长"], chainRows);
}

/** 按成功/失败拆分重读率 */
function printRereadByError(eventSeq: Map<string, EditEvent[]>, windowSize: number): void {
  printSection("按成功/失败拆分重读率（窗口 5 步）");

  // 收集所有编辑事件及其后续重读
  interface EditWithRead {
    toolName: string;
    isError: boolean;
    hasReread: boolean;
    isEditChain: boolean;
  }
  const edits: EditWithRead[] = [];

  for (const [, events] of eventSeq) {
    for (let i = 0; i < events.length; i++) {
      const ev = events[i];
      if (ev.toolName === "Read") continue;

      let readIdx = -1;
      const end = Math.min(i + windowSize + 1, events.length);
      for (let j = i + 1; j < end; j++) {
        if (events[j].toolName === "Read" && pathsMatch(ev.filePath, events[j].filePath)) {
          readIdx = j;
          break;
        }
      }

      let isEditChain = false;
      if (readIdx !== -1) {
        for (let j = readIdx + 1; j < events.length; j++) {
          if (events[j].toolName !== "Read" && pathsMatch(ev.filePath, events[j].filePath)) {
            isEditChain = true;
            break;
          }
        }
      }

      edits.push({
        toolName: ev.toolName,
        isError: ev.isError,
        hasReread: readIdx !== -1,
        isEditChain,
      });
    }
  }

  const successEdits = edits.filter((e) => !e.isError);
  const errorEdits = edits.filter((e) => e.isError);

  const rows: string[][] = [];
  for (const [label, group] of [
    ["成功编辑", successEdits],
    ["失败编辑", errorEdits],
  ] as const) {
    const total = group.length;
    const reread = group.filter((e) => e.hasReread).length;
    const pureVerify = group.filter((e) => e.hasReread && !e.isEditChain).length;
    const editChain = group.filter((e) => e.hasReread && e.isEditChain).length;
    if (total === 0) {
      rows.push([label, "0", "-", "-", "-"]);
    } else {
      rows.push([
        label,
        String(total),
        `${reread} (${pct(reread, total)})`,
        `${pureVerify} (${pct(pureVerify, total)})`,
        `${editChain} (${pct(editChain, total)})`,
      ]);
    }
  }

  printTable(["类别", "编辑数", "总重读", "纯验证重读", "编辑链重读"], rows);
}

// ═══════════════════════════════════════════════════
// 指标 2：Write 文件大小
// ═══════════════════════════════════════════════════

function analyzeWriteSizes(threads: ThreadRow[], loader: DataLoader): void {
  printSection("Write 文件大小分布");

  const sizes: number[] = [];

  for (const t of threads) {
    const msgs = loader.loadMessages(t.id);
    for (const msg of msgs) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type !== "tool_use" || block.name !== "Write") continue;
        const input = block.input as Record<string, unknown> | undefined;
        const content = input?.content ?? input?.file_text ?? "";
        if (typeof content !== "string") continue;
        const bytes = new TextEncoder().encode(content).length;
        sizes.push(bytes);
      }
    }
  }

  if (sizes.length === 0) {
    printWarning("无 Write 调用", "未检测到任何 Write 工具调用");
    return;
  }

  printMetric("Write 调用总数", sizes.length);
  printMetric("P50", formatSize(p50(sizes)));
  printMetric("P95", formatSize(p95(sizes)));
  printMetric("P99", formatSize(quantile99(sizes)));
  printMetric("最大值", formatSize(Math.max(...sizes)));
  printMetric("总计", formatSize(sizes.reduce((a, b) => a + b, 0)));

  // 分布桶
  const buckets: Record<string, number> = {
    "<1KB": 0,
    "1-5KB": 0,
    "5-20KB": 0,
    "20-100KB": 0,
    "100KB+": 0,
  };
  for (const s of sizes) {
    if (s < 1024) buckets["<1KB"]++;
    else if (s < 5120) buckets["1-5KB"]++;
    else if (s < 20480) buckets["5-20KB"]++;
    else if (s < 102400) buckets["20-100KB"]++;
    else buckets["100KB+"]++;
  }

  console.log("");
  printTable(
    ["大小范围", "数量", "占比", "分布"],
    Object.entries(buckets).map(([label, count]) => [
      label,
      String(count),
      pct(count, sizes.length),
      "█".repeat(Math.round((count / Math.max(1, sizes.length)) * 40)),
    ]),
  );
}

function quantile99(arr: number[]): number {
  if (arr.length === 0) return 0;
  const sorted = [...arr].sort((a, b) => a - b);
  const idx = Math.ceil(sorted.length * 0.99) - 1;
  return sorted[Math.max(0, idx)];
}

// ═══════════════════════════════════════════════════
// 主入口
// ═══════════════════════════════════════════════════

function main(): void {
  const sinceHours = parseSinceArg();
  const loader = new DataLoader();

  if (sinceHours) printMetric("时间范围", `最近 ${sinceHours} 小时`);
  const threads = sinceHours
    ? loader.loadVisibleThreadsSince(sinceHours)
    : loader.loadVisibleThreads();

  printMetric("分析会话数", threads.length);
  printSeparator();

  // 构建事件序列
  const eventSeq = buildAllEventSequences(threads, loader);

  // 统计概览
  let totalEdits = 0;
  let totalReads = 0;
  for (const [, events] of eventSeq) {
    for (const ev of events) {
      if (ev.toolName === "Read") totalReads++;
      else totalEdits++;
    }
  }

  printHeader("场景五：编辑质量");
  printMetric("事件总数（编辑+读）", totalEdits + totalReads);
  printMetric("编辑事件数", totalEdits);
  printMetric("读取事件数", totalReads);

  // ── 指标 1 & 2：重读率 ──
  printSection("指标 1：纯验证重读率");
  printRereadAnalysis(eventSeq, REREAD_WINDOW, "默认");

  // 不同窗口对比
  printSection("重读率窗口对比");
  printRereadAnalysis(eventSeq, 1, "紧邻重读（1 步内）");
  printRereadAnalysis(eventSeq, 3, "3 步内重读");
  printRereadAnalysis(eventSeq, 5, "5 步内重读");

  // 按成功/失败
  printRereadByError(eventSeq, REREAD_WINDOW);

  // ── 指标 2：Write 文件大小 ──
  analyzeWriteSizes(threads, loader);

  loader.close();
}

main();
