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

// 伪工具名：tool_result 找不到配对 tool_use 时以 tool_call_id（call_00_*）兜底。
// 这些不是真实工具，计入统计会制造噪音（如"工具种类数 3,350"）。
const UNPAIRED_TOOL_RESULT = "<unpaired tool_result>";

// 场景级错误分类——比三分类（参数/匹配/系统）更接近根因，直接定位可行动项。
// 顺序敏感：先匹配更具体的场景，最后落到宽泛三分类。
const ERROR_SCENARIOS: { key: string; re: RegExp }[] = [
  // 注意：错误文本是 "old_string not found"（found 而非 find），
  // `not.*find` 会漏掉全部 "not found" 错误——必须用 (found|unique)。
  { key: "Edit: old_string 未找到/不唯一", re: /Edit.*old_string.*not.*(?:found|unique)/i },
  { key: "Edit: 参数缺失/非法", re: /Edit.*file_path.*required|Edit.*parameter/i },
  { key: "Bash: 命令超时", re: /Bash.*timed out|Command timed out/i },
  { key: "Bash: 参数缺失", re: /Bash.*command.*required|Bash.*Missing command parameter/i },
  // 用户手动 Ctrl-C 中断——非缺陷，单列避免污染"系统错误"口径
  { key: "Bash: 用户中断", re: /Bash.*interrupted by user/i },
  { key: "Read: 文件不存在", re: /Read.*(File not found|ENOENT|no such file)/i },
  { key: "Read: offset 越界", re: /Read.*offset/i },
  { key: "Read: 参数错误", re: /Read.*file_path.*required/i },
  { key: "Agent: sub-agent 执行失败", re: /Agent.*(Sub-agent|subagent).*(failed|error)|Agent.*Failed to/i },
  { key: "Agent: 中断/取消", re: /Agent.*(interrupted|cancel)/i },
  { key: "Agent: 参数缺失", re: /Agent.*(missing required parameter|subagent_type parameter)/i },
  { key: "Agent: 定义不存在", re: /Agent.*cannot find agent definition/i },
  { key: "WebFetch: API 额度限制", re: /WebFetch.*(USAGE_EXCEEDED|usage limit|432|deactivated)/i },
  { key: "WebFetch: 抓取失败", re: /WebFetch.*(Failed to fetch|Extract failed|404|400)/i },
  { key: "WebSearch: 失败", re: /WebSearch.*(interrupted|error)/i },
  { key: "ExecuteExtraTool: 失败", re: /ExecuteExtraTool/i },
  // ask_user 是 AskUserQuestion 的旧工具名（历史会话遗留）
  { key: "AskUserQuestion: 失败", re: /(AskUserQuestion|ask_user)/i },
  { key: "TodoWrite: 失败", re: /TodoWrite/i },
  { key: "Grep: 正则/参数错误", re: /Grep.*error/i },
  { key: "folder_operations: 失败", re: /folder_operations/i },
  { key: "Write: 失败", re: /Write.*(failed|error)/i },
  { key: "Write: 参数缺失", re: /Write.*file_path.*required/i },
  { key: "Workflow: 失败", re: /Workflow/i },
];

function classifyError(content: string): string {
  for (const s of ERROR_SCENARIOS) {
    if (s.re.test(content)) return s.key;
  }
  return "其他";
}

// ── Local Types ──

interface ToolEvent {
  toolName: string;
  isError: boolean;
  errorContent: string;
  inputKeys: string[];
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
analyzeParamErrorBreakdown(allThreadData);

loader.close();

// ── Data Collection ──

function collectThreadData(
  threads: ThreadRow[],
  loader: DataLoader,
): ThreadToolData[] {
  return threads.map((t) => {
    const messages = loader.loadMessages(t.id);
    const toolUseMap = new Map<string, { name: string; inputKeys: string[] }>(); // tool_use_id → meta
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
            toolUseMap.set(tu.id, { name: tu.name, inputKeys: Object.keys(tu.input ?? {}) });
            if (tu.name === "Grep" && tu.input?.pattern) {
              grepPatterns.push(String(tu.input.pattern));
            }
          }
        }
      } else if (parsed.role === "tool") {
        const tc = parsed as ToolContent;
        const tuMeta = toolUseMap.get(tc.tool_call_id);
        // 未配对 tool_result（compact/rewind 残留）用统一占位名，避免 call_00_* 伪工具名噪音
        const toolName = tuMeta?.name ?? (tc.tool_call_id?.startsWith("call_") ? UNPAIRED_TOOL_RESULT : tc.tool_call_id ?? "unknown");
        const errorContent =
          typeof tc.content === "string" ? tc.content : JSON.stringify(tc.content);
        const event: ToolEvent = {
          toolName,
          isError: tc.is_error,
          errorContent,
          inputKeys: tuMeta?.inputKeys ?? [],
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

  // 场景级明细：定位具体根因（Edit old_string 未找到 / Bash 超时等）
  const sceneCounts = new Map<string, number>();
  for (const td of data) {
    for (const ev of td.toolEvents) {
      if (!ev.isError || !ev.errorContent) continue;
      const key = classifyError(ev.errorContent);
      sceneCounts.set(key, (sceneCounts.get(key) ?? 0) + 1);
    }
  }
  const sceneRows = [...sceneCounts.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 12)
    .map(([key, n]) => [key, String(n), pct(n, total)]);
  if (sceneRows.length > 0) {
    printSeparator();
    printSection("2b. 错误场景 Top（根因定位）");
    printTable(["场景", "数量", "占比"], sceneRows);
  }
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

// ── Metric 5: 参数错误细分 ──

function analyzeParamErrorBreakdown(data: ThreadToolData[]): void {
  printSection("5. 参数错误细分");

  // 筛选参数类错误（排除同时命中系统错误的）
  const paramErrors: { tool: string; snippet: string; inputKeys: string[] }[] = [];
  for (const td of data) {
    for (const ev of td.toolEvents) {
      if (!ev.isError || !ev.errorContent) continue;
      if (ERROR_SYSTEM.test(ev.errorContent)) continue;
      if (!ERROR_PARAM.test(ev.errorContent)) continue;
      paramErrors.push({
        tool: ev.toolName,
        snippet: extractErrorSnippet(ev.errorContent),
        inputKeys: ev.inputKeys,
      });
    }
  }

  if (paramErrors.length === 0) {
    printWarning("无参数错误", "未检测到参数类错误");
    return;
  }

  printMetric("参数错误总数", paramErrors.length);

  // 5.1 按工具分组
  printSection("  5.1 按工具分组");
  const byTool = new Map<string, number>();
  for (const e of paramErrors) {
    byTool.set(e.tool, (byTool.get(e.tool) ?? 0) + 1);
  }
  const toolRows = [...byTool.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 10)
    .map(([tool, cnt], i) => [String(i + 1), tool, String(cnt), pct(cnt, paramErrors.length)]);
  printTable(["#", "工具", "参数错误数", "占比"], toolRows);

  // 5.2 Top 错误模式（工具 × 错误摘要）
  printSection("  5.2 Top 错误模式（工具 × 错误摘要）");
  const byPattern = new Map<string, { tool: string; snippet: string; count: number }>();
  for (const e of paramErrors) {
    const key = `${e.tool}::${e.snippet}`;
    const g = byPattern.get(key) ?? { tool: e.tool, snippet: e.snippet, count: 0 };
    g.count++;
    byPattern.set(key, g);
  }
  const patternRows = [...byPattern.values()]
    .sort((a, b) => b.count - a.count)
    .slice(0, 15)
    .map((g, i) => [String(i + 1), g.tool, truncateStr(g.snippet, 50), String(g.count)]);
  printTable(["#", "工具", "错误摘要", "次数"], patternRows);

  // 5.3 出错时参数 key 频率
  printSection("  5.3 出错时参数 key 频率");
  const keyFreq = new Map<string, number>();
  for (const e of paramErrors) {
    for (const k of e.inputKeys) {
      keyFreq.set(k, (keyFreq.get(k) ?? 0) + 1);
    }
  }
  if (keyFreq.size > 0) {
    const keyRows = [...keyFreq.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 10)
      .map(([k, cnt]) => [k, String(cnt), pct(cnt, paramErrors.length)]);
    printTable(["参数 key", "出现次数", "占参数错误比"], keyRows);
  } else {
    console.log("  无参数 key 信息");
  }
}

/** 从错误消息提取关键摘要 */
function extractErrorSnippet(content: string): string {
  // 尝试提取引号内的字段名
  const fieldMatch = content.match(/['"`]([^'"`]{2,40})['"`]/);
  if (fieldMatch) {
    return `${content.slice(0, 30).trim()}…"${fieldMatch[1]}"`;
  }
  // 截取首行前 80 字符
  const firstLine = content.split("\n")[0];
  return firstLine.length > 80 ? firstLine.slice(0, 77) + "…" : firstLine;
}

function truncateStr(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max - 1) + "…";
}
