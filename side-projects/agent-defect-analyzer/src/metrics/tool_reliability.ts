import {
  DataLoader,
  type ThreadRow,
  type AiContent,
  type ToolContent,
} from "../data/loader.js";
import {
  pct,
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

const METRIC_TITLE = "工具可靠性分析";
const TOP_N = 15;
const ERROR_PARAM = /missing field|invalid|parse error|out of range|timeout|参数/i;
const ERROR_MATCH = /not found|not unique|does not exist|ENOENT|no such/i;
const ERROR_SYSTEM = /interrupted|tool.*not found|subagent.*error|cancel|truncated/i;

// ── Local Types ──

interface ToolEvent {
  toolName: string;
  isError: boolean;
  errorContent: string;
}

interface ThreadToolData {
  threadId: string;
  toolEvents: ToolEvent[];
  grepPatterns: string[];
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

analyzeToolFailureRate(allThreadData);
analyzeErrorDistribution(allThreadData);
analyzeConsecutiveFailures(allThreadData);
analyzeGrepRepeatRate(allThreadData);

loader.close();

// ── Data Collection ──

function collectThreadData(
  threads: ThreadRow[],
  loader: DataLoader,
): ThreadToolData[] {
  return threads.map((t) => {
    const messages = loader.loadMessages(t.id);
    const toolUseMap = new Map<string, string>(); // tool_use_id → tool_name
    const toolEvents: ToolEvent[] = [];
    const grepPatterns: string[] = [];

    for (const msg of messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed) continue;

      if (parsed.role === "assistant") {
        const ai = parsed as AiContent;
        if (!Array.isArray(ai.content)) continue;
        for (const block of ai.content) {
          if (block.type === "tool_use") {
            const tu = block as {
              type: "tool_use";
              id: string;
              name: string;
              input: Record<string, any>;
            };
            toolUseMap.set(tu.id, tu.name);
            if (tu.name === "Grep" && tu.input?.pattern) {
              grepPatterns.push(String(tu.input.pattern));
            }
          }
        }
      } else if (parsed.role === "tool") {
        const tc = parsed as ToolContent;
        const toolName =
          toolUseMap.get(tc.tool_call_id) ?? tc.tool_call_id ?? "unknown";
        const errorContent =
          typeof tc.content === "string" ? tc.content : JSON.stringify(tc.content);
        const event: ToolEvent = {
          toolName,
          isError: tc.is_error,
          errorContent,
        };
        toolEvents.push(event);
      }
    }

    return {
      threadId: t.id,
      toolEvents,
      grepPatterns,
    };
  });
}

// ── Metric 1: 工具失败率 ──

function analyzeToolFailureRate(data: ThreadToolData[]): void {
  printSection("1. 工具失败率");

  const stats = new Map<string, { calls: number; errors: number }>();
  for (const td of data) {
    for (const ev of td.toolEvents) {
      let s = stats.get(ev.toolName);
      if (!s) {
        s = { calls: 0, errors: 0 };
        stats.set(ev.toolName, s);
      }
      s.calls++;
      if (ev.isError) s.errors++;
    }
  }

  const sorted = [...stats.entries()]
    .map(([name, s]) => ({
      name,
      calls: s.calls,
      errors: s.errors,
      rate: s.calls > 0 ? s.errors / s.calls : 0,
    }))
    .sort((a, b) => b.errors - a.errors);

  const top = sorted.slice(0, TOP_N);
  const rows = top.map((t) => [
    t.name,
    String(t.calls),
    String(t.errors),
    (t.rate * 100).toFixed(1) + "%",
  ]);
  printTable(["工具名", "调用数", "失败数", "失败率"], rows);

  const totalCalls = sorted.reduce((s, t) => s + t.calls, 0);
  const totalErrors = sorted.reduce((s, t) => s + t.errors, 0);
  printMetric("总计工具调用", totalCalls);
  printMetric("总计失败", totalErrors);
  printMetric("整体失败率", pct(totalErrors, totalCalls));
  printMetric("工具种类数", sorted.length);

  if (sorted.length > TOP_N) {
    printMetric("（仅显示 Top 15，其余省略）", "");
  }

  // Per-tool bar for top 10 by error rate
  printSeparator();
  const topByRate = [...sorted].sort((a, b) => b.rate - a.rate).slice(0, 10);
  for (const t of topByRate) {
    printBar(`  ${t.name.padEnd(18)}`, t.rate, 40);
  }
}

// ── Metric 2: 错误类型分布 ──

function analyzeErrorDistribution(data: ThreadToolData[]): void {
  printSection("2. 错误类型分布");

  let paramErr = 0;
  let matchErr = 0;
  let systemErr = 0;
  let otherErr = 0;

  for (const td of data) {
    for (const ev of td.toolEvents) {
      if (!ev.isError || !ev.errorContent) continue;
      if (ERROR_SYSTEM.test(ev.errorContent)) {
        systemErr++;
      } else if (ERROR_PARAM.test(ev.errorContent)) {
        paramErr++;
      } else if (ERROR_MATCH.test(ev.errorContent)) {
        matchErr++;
      } else {
        otherErr++;
      }
    }
  }

  const total = paramErr + matchErr + systemErr + otherErr;
  if (total === 0) {
    printWarning("无错误数据", "未找到任何工具错误");
    return;
  }

  const cats = [
    { name: "参数错误", count: paramErr },
    { name: "匹配错误", count: matchErr },
    { name: "系统错误", count: systemErr },
    { name: "其他", count: otherErr },
  ];

  for (const c of cats) {
    printBar(`  ${c.name.padEnd(12)}`, total > 0 ? c.count / total : 0, 40);
  }
  console.log("");

  const rows = cats.map((c) => [
    c.name,
    String(c.count),
    pct(c.count, total),
  ]);
  printTable(["错误类型", "数量", "占比"], rows);
  printMetric("错误总计", total);
}

// ── Metric 3: 连续失败序列 ──

function analyzeConsecutiveFailures(data: ThreadToolData[]): void {
  printSection("3. 连续失败序列");

  const runLengths: number[] = [];

  for (const td of data) {
    if (td.toolEvents.length === 0) continue;

    let currentTool = "";
    let runLen = 0;

    for (const ev of td.toolEvents) {
      if (ev.isError && ev.toolName === currentTool) {
        runLen++;
      } else {
        if (runLen > 0) runLengths.push(runLen);
        if (ev.isError) {
          currentTool = ev.toolName;
          runLen = 1;
        } else {
          currentTool = "";
          runLen = 0;
        }
      }
    }
    if (runLen > 0) runLengths.push(runLen);
  }

  if (runLengths.length === 0) {
    printWarning("无连续失败", "所有工具失败均为单次孤立事件");
    return;
  }

  const max = Math.max(...runLengths);
  const avg =
    runLengths.reduce((a, b) => a + b, 0) / runLengths.length;
  const sorted = [...runLengths].sort((a, b) => a - b);
  const p50val = sorted[Math.floor(sorted.length / 2)];
  const p95val = sorted[Math.ceil(sorted.length * 0.95) - 1] ?? sorted[sorted.length - 1];

  printMetric("最长连续失败", max, "次");
  printMetric("平均连续失败长度", avg.toFixed(1), "次");
  printMetric("P50 连续失败长度", p50val, "次");
  printMetric("P95 连续失败长度", p95val, "次");

  // 长度分布
  const dist = new Map<number, number>();
  for (const l of runLengths) {
    dist.set(l, (dist.get(l) ?? 0) + 1);
  }
  const distRows = [...dist.entries()]
    .sort(([a], [b]) => a - b)
    .map(([len, cnt]) => [String(len), String(cnt), pct(cnt, runLengths.length)]);
  printTable(["连续失败长度", "出现次数", "占比"], distRows);
  printMetric("连续失败段总数", runLengths.length);
}

// ── Metric 4: Grep 重复搜索率 ──

function analyzeGrepRepeatRate(data: ThreadToolData[]): void {
  printSection("4. Grep 重复搜索率");

  // 按 session 统计 pattern 重复
  const allGrepCalls = data.flatMap((td) => td.grepPatterns);
  const totalGrep = allGrepCalls.length;

  if (totalGrep === 0) {
    printWarning("无 Grep 调用", "未找到 Grep 工具的使用记录");
    return;
  }

  // 统计全局 pattern 频率
  const patternFreq = new Map<string, number>();
  for (const p of allGrepCalls) {
    patternFreq.set(p, (patternFreq.get(p) ?? 0) + 1);
  }

  const duplicatePatterns = [...patternFreq.entries()]
    .filter(([, cnt]) => cnt >= 2)
    .sort(([, a], [, b]) => b - a);

  const totalDuplicateCalls = duplicatePatterns.reduce(
    (s, [, cnt]) => s + cnt,
    0,
  );

  printMetric("Grep 总调用数", totalGrep);
  printMetric("重复 pattern 数", duplicatePatterns.length);
  printMetric("重复调用数", totalDuplicateCalls);
  printMetric("重复率", pct(totalDuplicateCalls, totalGrep));

  if (duplicatePatterns.length > 0) {
    printSeparator();
    const top10 = duplicatePatterns.slice(0, 10);
    const rows = top10.map(([pat, cnt], i) => [
      String(i + 1),
      pat.length > 60 ? pat.slice(0, 57) + "..." : pat,
      String(cnt),
      pct(cnt, totalGrep),
    ]);
    printTable(["#", "Pattern", "调用次数", "占比"], rows);

    if (duplicatePatterns.length > 10) {
      printMetric(`（仅显示 Top 10，共 ${duplicatePatterns.length} 个重复 pattern）`, "");
    }
  }
}
