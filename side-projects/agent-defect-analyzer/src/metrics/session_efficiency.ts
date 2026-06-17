import {
  DataLoader,
  type ThreadRow,
  type AiContent,
  type ToolContent,
} from "../data/loader.js";
import {
  avg,
  p50,
  p95,
  pct,
  formatDuration,
  parseSinceArg,
  printHeader,
  printSection,
  printMetric,
  printWarning,
  printTable,
  printBar,
  printSeparator,
} from "../lib/utils.js";

// ── Constants ──

const METRIC_TITLE = "会话效率分析";
const DEAD_LOOP_MIN = 5;
const LINKAGE_WINDOW = 5;

// ── Local Types ──

interface ToolUseInfo {
  id: string;
  name: string;
  input: Record<string, any>;
}

interface AssistantMsg {
  index: number;
  toolUses: ToolUseInfo[];
}

interface ToolResultInfo {
  index: number;
  toolUseId: string;
  content: string;
  isError: boolean;
}

interface ThreadData {
  thread: ThreadRow;
  assistantMsgs: AssistantMsg[];
  toolResults: Map<string, ToolResultInfo>; // toolUseId → ToolResultInfo
}

// ── Main ──

const sinceHours = parseSinceArg();
const loader = new DataLoader();

printHeader(METRIC_TITLE);
if (sinceHours) printMetric("时间范围", `最近 ${sinceHours} 小时`);
const threads = sinceHours
  ? loader.loadVisibleThreadsSince(sinceHours)
  : loader.loadVisibleThreads();
printMetric("可见会话数", threads.length);

const allThreadData = collectThreadData(threads, loader);

analyzeMessagesPerSession(threads);
analyzeToolCallsPerTurn(allThreadData);
analyzeDeadLoops(allThreadData);
analyzeSessionDuration(threads);
analyzeRedundantReads(allThreadData);
analyzeSearchToReadLinkage(allThreadData);

loader.close();

// ── Data Collection ──

function collectThreadData(
  threads: ThreadRow[],
  loader: DataLoader,
): ThreadData[] {
  return threads.map((t) => {
    const messages = loader.loadMessages(t.id);
    const assistantMsgs: AssistantMsg[] = [];
    const toolResults = new Map<string, ToolResultInfo>();

    for (let i = 0; i < messages.length; i++) {
      const msg = messages[i];
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed) continue;

      if (parsed.role === "assistant") {
        const ai = parsed as AiContent;
        const toolUses: ToolUseInfo[] = [];
        if (Array.isArray(ai.content)) {
          for (const block of ai.content) {
            if (block.type === "tool_use") {
              const tu = block as {
                type: "tool_use";
                id: string;
                name: string;
                input: Record<string, any>;
              };
              toolUses.push({ id: tu.id, name: tu.name, input: tu.input ?? {} });
            }
          }
        }
        assistantMsgs.push({ index: i, toolUses });
      } else if (parsed.role === "tool") {
        const tc = parsed as ToolContent;
        const content =
          typeof tc.content === "string"
            ? tc.content
            : JSON.stringify(tc.content);
        toolResults.set(tc.tool_call_id, {
          index: i,
          toolUseId: tc.tool_call_id,
          content,
          isError: tc.is_error,
        });
      }
    }

    return { thread: t, assistantMsgs, toolResults };
  });
}

// ── Metric 1: 人均消息数 ──

function analyzeMessagesPerSession(threads: ThreadRow[]): void {
  printSection("1. 人均消息数");

  const counts = threads.map((t) => t.message_count).filter((c) => c > 0);
  if (counts.length === 0) {
    printWarning("无消息数据", "没有包含消息的会话");
    return;
  }

  const sortedCounts = [...counts].sort((a, b) => a - b);
  const buckets: [string, number, number][] = [
    ["1-5", 1, 5],
    ["6-10", 6, 10],
    ["11-20", 11, 20],
    ["21-50", 21, 50],
    ["51-100", 51, 100],
    ["101-500", 101, 500],
    ["500+", 501, Infinity],
  ];

  printMetric("P50", p50(counts));
  printMetric("P95", p95(counts));
  printMetric("最大", Math.max(...counts));
  printMetric("平均", avg(counts).toFixed(1));
  printSeparator();

  const rows = buckets.map(([label, lo, hi]) => {
    const cnt = counts.filter((c) => c >= lo && c <= hi).length;
    return [label, String(cnt), pct(cnt, counts.length)];
  });
  printTable(["消息数", "会话数", "占比"], rows);
}

// ── Metric 2: 工具调用/轮次 ──

function analyzeToolCallsPerTurn(data: ThreadData[]): void {
  printSection("2. 工具调用/轮次");

  const toolCounts: number[] = [];
  for (const td of data) {
    for (const am of td.assistantMsgs) {
      toolCounts.push(am.toolUses.length);
    }
  }

  if (toolCounts.length === 0) {
    printWarning("无工具调用", "没有 assistant 消息包含工具调用");
    return;
  }

  const sortedCounts = [...toolCounts].sort((a, b) => a - b);
  const bucketDefs: [string, (n: number) => boolean][] = [
    ["0", (n) => n === 0],
    ["1", (n) => n === 1],
    ["2-3", (n) => n >= 2 && n <= 3],
    ["4-5", (n) => n >= 4 && n <= 5],
    ["6+", (n) => n >= 6],
  ];

  printMetric("平均", avg(toolCounts).toFixed(2));
  printMetric("P50", p50(toolCounts));
  printMetric("P95", p95(toolCounts));
  printMetric("最大", Math.max(...toolCounts));
  printSeparator();

  const rows = bucketDefs.map(([label, pred]) => {
    const cnt = toolCounts.filter(pred).length;
    return [label, String(cnt), pct(cnt, toolCounts.length)];
  });
  printTable(["工具数/轮", "轮次数", "占比"], rows);
  printMetric("总轮次", toolCounts.length);
}

// ── Metric 3: 死循环检测（N≥5） ──

function normalizeArgs(args: Record<string, any>): string {
  try {
    const sorted: Record<string, any> = {};
    for (const key of Object.keys(args).sort()) {
      sorted[key] = args[key];
    }
    return JSON.stringify(sorted);
  } catch {
    return JSON.stringify(args);
  }
}

function analyzeDeadLoops(data: ThreadData[]): void {
  printSection("3. 死循环检测（N≥5）");

  interface DeadLoop {
    threadId: string;
    toolName: string;
    count: number;
    argsPreview: string;
  }

  const deadLoops: DeadLoop[] = [];

  for (const td of data) {
    // 扁平化所有 tool_use 调用
    const calls: { name: string; args: string; raw: Record<string, any> }[] = [];
    for (const am of td.assistantMsgs) {
      for (const tu of am.toolUses) {
        calls.push({
          name: tu.name,
          args: normalizeArgs(tu.input),
          raw: tu.input,
        });
      }
    }

    if (calls.length < DEAD_LOOP_MIN) continue;

    let currentName = "";
    let currentArgs = "";
    let runLen = 0;
    let runStartRaw: Record<string, any> = {};

    for (const c of calls) {
      if (c.name === currentName && c.args === currentArgs) {
        runLen++;
      } else {
        if (runLen >= DEAD_LOOP_MIN) {
          deadLoops.push({
            threadId: td.thread.id.slice(0, 8),
            toolName: currentName,
            count: runLen,
            argsPreview: truncatePreview(runStartRaw),
          });
        }
        currentName = c.name;
        currentArgs = c.args;
        runLen = 1;
        runStartRaw = c.raw;
      }
    }
    // 末尾检查
    if (runLen >= DEAD_LOOP_MIN) {
      deadLoops.push({
        threadId: td.thread.id.slice(0, 8),
        toolName: currentName,
        count: runLen,
        argsPreview: truncatePreview(runStartRaw),
      });
    }
  }

  if (deadLoops.length === 0) {
    printWarning("未检测到死循环", `没有发现 N≥${DEAD_LOOP_MIN} 的连续重复调用`);
    return;
  }

  deadLoops.sort((a, b) => b.count - a.count);

  const maxLoop = deadLoops[0].count;
  printMetric("死循环数量", deadLoops.length);
  printMetric("最长循环长度", maxLoop);
  printMetric(
    "平均循环长度",
    avg(deadLoops.map((d) => d.count)).toFixed(1),
  );
  printSeparator();

  const rows = deadLoops.map((dl) => [
    dl.threadId,
    dl.toolName,
    String(dl.count),
    dl.argsPreview,
  ]);
  printTable(["会话ID", "工具名", "次数", "参数预览"], rows);
}

function truncatePreview(args: Record<string, any>): string {
  const s = JSON.stringify(args);
  return s.length > 60 ? s.slice(0, 57) + "..." : s;
}

// ── Metric 4: 会话时长 ──

function analyzeSessionDuration(threads: ThreadRow[]): void {
  printSection("4. 会话时长");

  const durations: number[] = [];
  for (const t of threads) {
    const created = new Date(t.created_at).getTime();
    const updated = new Date(t.updated_at).getTime();
    const ms = updated - created;
    if (ms <= 0) continue;
    durations.push(ms / 60000); // 转换为分钟
  }

  if (durations.length === 0) {
    printWarning("无时长数据", "会话创建/更新时间异常");
    return;
  }

  const sortedDurations = [...durations].sort((a, b) => a - b);
  const bucketMins: [string, number, number][] = [
    ["<1m", 0, 1],
    ["1-5m", 1, 5],
    ["5-15m", 5, 15],
    ["15-30m", 15, 30],
    ["30-60m", 30, 60],
    ["1h-3h", 60, 180],
    ["3h+", 180, Infinity],
  ];

  const maxMin = Math.max(...durations);
  printMetric("P50", formatDuration(p50(durations)));
  printMetric("P95", formatDuration(p95(durations)));
  printMetric("最大", formatDuration(maxMin));
  printMetric("平均", formatDuration(avg(durations)));
  printSeparator();

  const rows = bucketMins.map(([label, lo, hi]) => {
    const cnt = sortedDurations.filter((d) => d >= lo && d < hi).length;
    return [label, String(cnt), pct(cnt, sortedDurations.length)];
  });
  printTable(["时长", "会话数", "占比"], rows);
}

// ── Metric 5: 冗余 Read ──

function analyzeRedundantReads(data: ThreadData[]): void {
  printSection("5. 冗余 Read");

  interface FileRead {
    filePath: string;
    offset: number;
    msgIndex: number;
  }

  let totalReads = 0;
  let redundantReads = 0;
  const redundantFiles = new Map<string, number>();

  for (const td of data) {
    // 收集该线程的所有 Read 和 Write
    const reads: FileRead[] = [];
    const edits = new Map<string, number[]>(); // filePath → msgIndex[]

    for (const am of td.assistantMsgs) {
      for (const tu of am.toolUses) {
        const isEdit = tu.name === "Write";
        if (isEdit && tu.input.file_path) {
          const fp = String(tu.input.file_path);
          if (!edits.has(fp)) edits.set(fp, []);
          edits.get(fp)!.push(am.index);
        }
        if (tu.name === "Read" && tu.input.file_path) {
          reads.push({
            filePath: String(tu.input.file_path),
            offset: Number(tu.input.offset ?? 0),
            msgIndex: am.index,
          });
        }
      }
    }

    totalReads += reads.length;

    // 按文件分组，并按 msgIndex 排序
    const fileReads = new Map<string, FileRead[]>();
    for (const r of reads) {
      if (!fileReads.has(r.filePath)) fileReads.set(r.filePath, []);
      fileReads.get(r.filePath)!.push(r);
    }

    for (const [filePath, frs] of fileReads) {
      if (frs.length < 2) continue; // 只读一次不视为冗余

      frs.sort((a, b) => a.msgIndex - b.msgIndex);
      const fileEdits = (edits.get(filePath) ?? []).sort((a, b) => a - b);

      // 将 reads 切分为"访问窗口"：连续读取之间无编辑的为同一窗口
      const windows: FileRead[][] = [];
      let winStart = 0;

      for (let j = 0; j < frs.length; j++) {
        const nextIsSeparated =
          j + 1 < frs.length &&
          fileEdits.some(
            (ei) => ei > frs[j].msgIndex && ei < frs[j + 1].msgIndex,
          );

        if (nextIsSeparated || j === frs.length - 1) {
          windows.push(frs.slice(winStart, j + 1));
          winStart = j + 1;
        }
      }

      for (const window of windows) {
        if (window.length < 2) continue; // 单次读取且在多次读取中：被编辑分隔

        const hasEditAfterWindow = fileEdits.some(
          (ei) => ei > window[window.length - 1].msgIndex,
        );
        if (hasEditAfterWindow) continue; // 窗口后有编辑，非冗余

        // 检查 offset 递增（分页阅读排除）
        let isPagination = true;
        for (let j = 1; j < window.length; j++) {
          if (window[j].offset <= window[j - 1].offset) {
            isPagination = false;
            break;
          }
        }
        if (isPagination) continue; // 分页阅读，非冗余

        redundantReads += window.length;
        redundantFiles.set(
          filePath,
          (redundantFiles.get(filePath) ?? 0) + window.length,
        );
      }
    }
  }

  if (totalReads === 0) {
    printWarning("无 Read 调用", "未找到 Read 工具的使用记录");
    return;
  }

  printMetric("Read 总调用数", totalReads);
  printMetric("冗余 Read 数", redundantReads);
  printMetric("冗余率", pct(redundantReads, totalReads));
  printBar("  冗余率", totalReads > 0 ? redundantReads / totalReads : 0, 40);

  if (redundantFiles.size > 0) {
    printSeparator();
    const top10 = [...redundantFiles.entries()]
      .sort(([, a], [, b]) => b - a)
      .slice(0, 10);
    const rows = top10.map(([fp, cnt], i) => [
      String(i + 1),
      truncatePath(fp),
      String(cnt),
    ]);
    printTable(["#", "文件路径", "冗余次数"], rows);

    if (redundantFiles.size > 10) {
      printMetric(`（仅显示 Top 10，共 ${redundantFiles.size} 个冗余文件）`, "");
    }
  }
}

function truncatePath(path: string): string {
  return path.length > 70 ? "..." + path.slice(-67) : path;
}

// ── Metric 6: 搜索→Read 联动率 ──

function simpleGlobMatch(pattern: string, filePath: string): boolean {
  let regexStr = pattern
    .replace(/[.+^${}()|[\]\\]/g, "\\$&")
    .replace(/\*\*/g, "\x00GLOBSTAR\x00")
    .replace(/\*/g, "[^/]*")
    .replace(/\x00GLOBSTAR\x00/g, ".*")
    .replace(/\?/g, ".");
  try {
    const re = new RegExp(regexStr);
    return re.test(filePath) || re.test(filePath.split("/").pop() ?? filePath);
  } catch {
    return filePath.includes(pattern.replace(/[*?]+/g, ""));
  }
}

function extractGrepResultFiles(content: string): string[] {
  const files: string[] = [];
  for (const line of content.split("\n")) {
    const m = line.match(/^([/\w\-\.]+\.\w{1,10})[:|:]\d+/);
    if (m) files.push(m[1]);
  }
  return [...new Set(files)];
}

function analyzeSearchToReadLinkage(data: ThreadData[]): void {
  printSection("6. 搜索→Read 联动率");

  interface LinkageStat {
    total: number;
    step1: number; // 1步内
    step2to5: number; // 2-5步
    zeroLink: number; // 零联动
  }

  const grepStat: LinkageStat = { total: 0, step1: 0, step2to5: 0, zeroLink: 0 };
  const globStat: LinkageStat = { total: 0, step1: 0, step2to5: 0, zeroLink: 0 };
  const webSearchStat: LinkageStat = {
    total: 0,
    step1: 0,
    step2to5: 0,
    zeroLink: 0,
  };

  for (const td of data) {
    // 构建 read 调用索引：msgIndex → filePath[]
    const readByMsg = new Map<number, string[]>();
    for (const am of td.assistantMsgs) {
      for (const tu of am.toolUses) {
        if (tu.name === "Read" && tu.input.file_path) {
          if (!readByMsg.has(am.index)) readByMsg.set(am.index, []);
          readByMsg.get(am.index)!.push(String(tu.input.file_path));
        }
      }
    }

    for (const am of td.assistantMsgs) {
      for (const tu of am.toolUses) {
        const isSearch =
          tu.name === "Grep" ||
          tu.name === "Glob" ||
          tu.name === "WebSearch";
        if (!isSearch) continue;

        const stat =
          tu.name === "Grep"
            ? grepStat
            : tu.name === "Glob"
              ? globStat
              : webSearchStat;
        stat.total++;

        // 获取搜索结果
        const result = td.toolResults.get(tu.id);
        const resultFiles: string[] =
          tu.name === "Grep" && result
            ? extractGrepResultFiles(result.content)
            : [];

        // 在后续 LINKAGE_WINDOW 条消息中查找 Read
        let linkedStep = -1;
        for (
          let offset = 1;
          offset <= LINKAGE_WINDOW && linkedStep < 0;
          offset++
        ) {
          const checkIdx = am.index + offset;
          const readFiles = readByMsg.get(checkIdx);
          if (!readFiles || readFiles.length === 0) continue;

          for (const rf of readFiles) {
            let matches = false;
            if (tu.name === "Grep") {
              matches = resultFiles.some(
                (f) => rf.endsWith(f) || f.endsWith(rf) || rf === f,
              );
            } else if (tu.name === "Glob") {
              matches = simpleGlobMatch(
                String(tu.input.pattern ?? ""),
                rf,
              );
            } else {
              // WebSearch: any Read counts
              matches = true;
            }
            if (matches) {
              linkedStep = offset;
              break;
            }
          }
        }

        if (linkedStep === 1) {
          stat.step1++;
        } else if (linkedStep >= 2) {
          stat.step2to5++;
        } else {
          stat.zeroLink++;
        }
      }
    }
  }

  const allStats = [
    { name: "Grep", stat: grepStat },
    { name: "Glob", stat: globStat },
    { name: "WebSearch", stat: webSearchStat },
  ];

  const rows = allStats.map(({ name, stat }) => {
    const linked = stat.step1 + stat.step2to5;
    const rate = stat.total > 0 ? linked / stat.total : 0;
    return [
      name,
      String(stat.total),
      String(stat.step1),
      String(stat.step2to5),
      String(stat.zeroLink),
      (rate * 100).toFixed(1) + "%",
    ];
  });

  const grandTotal = allStats.reduce((s, a) => s + a.stat.total, 0);
  if (grandTotal === 0) {
    printWarning("无搜索调用", "未找到 Grep/Glob/WebSearch 的使用记录");
    return;
  }

  printTable(
    ["工具", "总搜索", "1步内Read", "2-5步Read", "零联动", "联动率"],
    rows,
  );

  const totalLinked = allStats.reduce(
    (s, a) => s + a.stat.step1 + a.stat.step2to5,
    0,
  );
  printMetric("搜索总数", grandTotal);
  printMetric("联动总数", totalLinked);
  printMetric("整体联动率", pct(totalLinked, grandTotal));
  printBar(
    "  整体联动率",
    grandTotal > 0 ? totalLinked / grandTotal : 0,
    40,
  );
}
