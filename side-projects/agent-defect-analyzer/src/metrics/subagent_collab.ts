//! 场景六：SubAgent 协作
//!
//! 4 项指标：空转 SubAgent、消息量分布、工具错误率、编辑产出比。
//! 用法：bun run src/metrics/subagent_collab.ts --since 24

import { DataLoader, type ThreadRow, type MessageRow, type AiContent, type ContentBlock } from "../data/loader.js";
import { avg, median, p50, p95, pct, quantile, formatSize, parseSinceArg, printHeader, printSection, printMetric, printWarning, printTable, printBar, printSeparator } from "../lib/utils.js";
import chalk from "chalk";

// ═══════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════

const EDIT_OUTPUT_TOOLS = new Set(["Write"]);

/** 非编辑型 SubAgent 类型：这些 agent 的本职工作就不是编辑文件 */
const NON_EDITING_TYPES = new Set([
  "explore",          // 代码探索，只读
  "web-researcher",   // 网页调研，只读
  "hello-agent",      // 打招呼，无操作
  "verification",     // 验证测试，不编辑
  "plan",             // 方案设计，不编辑
]);

// ═══════════════════════════════════════════════════
// 类型
// ═══════════════════════════════════════════════════

interface SubAgentAnalysis {
  thread: ThreadRow;
  messages: MessageRow[];
  subagentType: string;
  toolUseCount: number;
  editToolUseCount: number;
  toolErrorCount: number;
  toolErrorRate: number;
}

// ═══════════════════════════════════════════════════
// 指标 1：内置 Agent 分类分析
// ═══════════════════════════════════════════════════

function analyzeAgentClassification(subAgents: SubAgentAnalysis[]): void {
  printSection("指标 1：内置 Agent 分类分析");

  const editingCount = subAgents.filter(
    (sa) => !NON_EDITING_TYPES.has(sa.subagentType),
  ).length;
  const nonEditingCount = subAgents.filter(
    (sa) => NON_EDITING_TYPES.has(sa.subagentType),
  ).length;

  printMetric("SubAgent 总数", subAgents.length);
  printMetric("编辑型", editingCount);
  printMetric("非编辑型", nonEditingCount, ` (${[...NON_EDITING_TYPES].join(", ")})`);

  // ── 构建类型数据 ──
  const typeProfiles = new Map<
    string,
    {
      agents: SubAgentAnalysis[];
      count: number;
      totalMsg: number;
      totalCall: number;
      toolCounts: Map<string, number>;
      searchPct: number;
      editPct: number;
      execPct: number;
    }
  >();

  for (const sa of subAgents) {
    if (!typeProfiles.has(sa.subagentType)) {
      typeProfiles.set(sa.subagentType, {
        agents: [],
        count: 0,
        totalMsg: 0,
        totalCall: 0,
        toolCounts: new Map(),
        searchPct: 0,
        editPct: 0,
        execPct: 0,
      });
    }
    const p = typeProfiles.get(sa.subagentType)!;
    p.agents.push(sa);
    p.count++;
    p.totalMsg += sa.thread.message_count;
    for (const msg of sa.messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use") {
          p.toolCounts.set(block.name, (p.toolCounts.get(block.name) || 0) + 1);
          p.totalCall++;
        }
      }
    }
  }

  // 计算各类占比
  for (const [, p] of typeProfiles) {
    const t = p.totalCall;
    p.searchPct = [...p.toolCounts.entries()]
      .filter(([t]) => SEARCH_TOOLS.has(t))
      .reduce((s, [, c]) => s + c, 0) / (t || 1);
    p.editPct = [...p.toolCounts.entries()]
      .filter(([t]) => EDIT_OUTPUT_TOOLS.has(t))
      .reduce((s, [, c]) => s + c, 0) / (t || 1);
    p.execPct = [...p.toolCounts.entries()]
      .filter(([t]) => EXEC_TOOLS.has(t))
      .reduce((s, [, c]) => s + c, 0) / (t || 1);
  }

  // ── 逐一分析各类型 ──
  const sorted = [...typeProfiles.entries()].sort((a, b) => b[1].count - a[1].count);

  for (const [type, p] of sorted) {
    const isNonEditing = NON_EDITING_TYPES.has(type);
    const cat = isNonEditing ? "非编辑型" : "编辑型";
    const direction = isNonEditing
      ? "只读"
      : p.editPct > p.searchPct * 0.3
        ? "编辑"
        : "研究";

    printSection(type);
    console.log(
      chalk.dim(
        `  分类: ${cat}  方向: ${direction}型  数量: ${p.count}  均消息: ${Math.round(p.totalMsg / p.count)}  总调用: ${p.totalCall}`,
      ),
    );

    // 工具占比 Top 8
    console.log("");
    const topTools = [...p.toolCounts.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 8);
    for (const [tool, count] of topTools) {
      const t = p.totalCall || 1;
      const bar = "█".repeat(Math.round((count / t) * 30));
      console.log(
        `    ${tool.padEnd(18)} ${String(count).padStart(5)}  ${pct(count, t).padStart(5)}  ${bar}`,
      );
    }

    // 模式分布条
    console.log("");
    printBar("搜索", p.searchPct);
    printBar("编辑", p.editPct);
    printBar("执行", p.execPct);
    console.log("");
  }
}

// ═══════════════════════════════════════════════════
// 辅助：SubAgent 类型推断 + 编辑产出检测
// ═══════════════════════════════════════════════════

/** 从父线程的 Agent tool_use 中提取 SubAgent 类型映射 */
function buildSubAgentTypeMap(
  loader: DataLoader,
  subAgents: ThreadRow[],
): Map<string, string> {
  const typeMap = new Map<string, string>();

  // 按父线程分组
  const byParent = new Map<string, ThreadRow[]>();
  for (const sa of subAgents) {
    if (!sa.parent_thread_id) continue;
    if (!byParent.has(sa.parent_thread_id)) byParent.set(sa.parent_thread_id, []);
    byParent.get(sa.parent_thread_id)!.push(sa);
  }

  for (const [parentId, children] of byParent) {
    const messages = loader.loadMessages(parentId);

    // 收集 Agent tool_use 调用
    const agentCalls = new Map<string, { subagentType?: string }>();
    for (const msg of messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use" && block.name === "Agent") {
          agentCalls.set(block.id, {
            subagentType: (block.input as any)?.subagent_type || (block.input as any)?.type,
          });
        }
      }
    }

    if (agentCalls.size === 0) continue;

    // 匹配 tool_result → child thread
    for (const msg of messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "tool") continue;
      const tc = parsed as any;
      if (!tc.tool_call_id) continue;

      const agentCall = agentCalls.get(tc.tool_call_id);
      if (!agentCall) continue;

      const resultContent =
        typeof tc.content === "string" ? tc.content : JSON.stringify(tc.content);
      for (const child of children) {
        if (resultContent.includes(child.id)) {
          typeMap.set(child.id, agentCall.subagentType || "unknown");
        }
      }
    }

    // 回退：按顺序匹配未分配的
    const unmatched = children.filter((c) => !typeMap.has(c.id));
    if (unmatched.length > 0) {
      const agentEntries = [...agentCalls.entries()];
      for (let i = 0; i < Math.min(unmatched.length, agentEntries.length); i++) {
        typeMap.set(
          unmatched[i].id,
          agentEntries[i][1].subagentType || "unknown",
        );
      }
    }
  }

  // 最终回退
  for (const sa of subAgents) {
    if (!typeMap.has(sa.id)) {
      typeMap.set(sa.id, "unknown");
    }
  }

  return typeMap;
}

// ═══════════════════════════════════════════════════
// 指标 2：SubAgent 消息量
// ═══════════════════════════════════════════════════

function analyzeMessageVolume(subAgents: ThreadRow[]): void {
  printSection("指标 2：SubAgent 消息量分布");

  const counts = subAgents.map((sa) => sa.message_count).filter((c) => c > 0);

  if (counts.length === 0) {
    printWarning("无数据", "没有可用的 SubAgent 消息量数据");
    return;
  }

  printMetric("P50", p50(counts));
  printMetric("P95", p95(counts));
  printMetric("P99", quantile99(counts));
  printMetric("最大值", Math.max(...counts));
  printMetric("平均值", avg(counts).toFixed(1));
  printMetric("总计消息", counts.reduce((a, b) => a + b, 0));

  // 分布桶
  const buckets: Record<string, number> = {
    "1-5": 0,
    "6-10": 0,
    "11-20": 0,
    "21-50": 0,
    "51-100": 0,
    "101+": 0,
  };
  for (const c of counts) {
    if (c <= 5) buckets["1-5"]++;
    else if (c <= 10) buckets["6-10"]++;
    else if (c <= 20) buckets["11-20"]++;
    else if (c <= 50) buckets["21-50"]++;
    else if (c <= 100) buckets["51-100"]++;
    else buckets["101+"]++;
  }

  console.log("");
  printTable(
    ["消息数范围", "数量", "占比", "分布"],
    Object.entries(buckets).map(([label, count]) => [
      label,
      String(count),
      pct(count, counts.length),
      "█".repeat(Math.round((count / Math.max(1, counts.length)) * 40)),
    ]),
  );
}

// ═══════════════════════════════════════════════════
// 指标 3：SubAgent 工具错误率
// ═══════════════════════════════════════════════════

interface ToolErrorStats {
  toolUseCount: number;
  errorCount: number;
}

/** 统计单个 SubAgent 的工具错误率 */
function computeToolErrorRate(messages: MessageRow[]): ToolErrorStats {
  const toolUseIds = new Set<string>();
  const errorIds = new Set<string>();

  for (const msg of messages) {
    const parsed = DataLoader.parseContent(msg.content);
    if (!parsed) continue;

    if (parsed.role === "assistant") {
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use") {
          toolUseIds.add(block.id);
        }
      }
    } else if (parsed.role === "tool") {
      const err = DataLoader.parseToolError(parsed);
      if (err && err.isError) {
        errorIds.add(err.toolCallId);
      }
    }
  }

  return {
    toolUseCount: toolUseIds.size,
    errorCount: errorIds.size,
  };
}

function analyzeToolErrorRate(subAgents: SubAgentAnalysis[]): void {
  printSection("指标 3：SubAgent 工具错误率");

  let totalToolUse = 0;
  let totalErrors = 0;
  const perSubAgent: { id: string; toolUse: number; errors: number; rate: number }[] = [];

  for (const sa of subAgents) {
    const stats = computeToolErrorRate(sa.messages);
    totalToolUse += stats.toolUseCount;
    totalErrors += stats.errorCount;
    if (stats.toolUseCount > 0) {
      perSubAgent.push({
        id: sa.thread.id,
        toolUse: stats.toolUseCount,
        errors: stats.errorCount,
        rate: stats.errorCount / stats.toolUseCount,
      });
    }
  }

  if (totalToolUse === 0) {
    printWarning("无数据", "没有可用的工具调用数据");
    return;
  }

  const overallRate = totalErrors / totalToolUse;
  printMetric("总工具调用数", totalToolUse);
  printMetric("总错误数", totalErrors);
  printMetric("总体错误率", pct(totalErrors, totalToolUse));

  // 每个 SubAgent 的错误率分布
  const rates = perSubAgent.map((s) => s.rate);
  printMetric("P50 错误率", pct(p50(rates), 1));
  printMetric("P95 错误率", pct(p95(rates), 1));

  // Top 10 最高错误率
  perSubAgent.sort((a, b) => b.rate - a.rate);
  const top10 = perSubAgent.slice(0, 10);

  console.log("");
  printTable(
    ["子Agent ID", "工具调用数", "错误数", "错误率"],
    top10.map((s) => [
      s.id.slice(0, 14) + "...",
      String(s.toolUse),
      String(s.errors),
      pct(s.errors, s.toolUse),
    ]),
  );

  printBar("总体错误率", overallRate);
}

// ═══════════════════════════════════════════════════
// 指标 4：SubAgent 产出比
// ═══════════════════════════════════════════════════

function computeOutputRatio(messages: MessageRow[]): { total: number; edit: number } {
  let total = 0;
  let edit = 0;

  for (const msg of messages) {
    const parsed = DataLoader.parseContent(msg.content);
    if (!parsed || parsed.role !== "assistant") continue;
    const ai = parsed as AiContent;
    const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
    for (const block of blocks) {
      if (block.type !== "tool_use") continue;
      total++;
      if (EDIT_OUTPUT_TOOLS.has(block.name)) edit++;
    }
  }

  return { total, edit };
}

function analyzeOutputRatio(subAgents: SubAgentAnalysis[]): void {
  printSection("指标 4：SubAgent 产出比（编辑类工具 / 总 tool_use）");

  // 总体统计
  let totalToolUse = 0;
  let totalEditUse = 0;
  const ratios: { id: string; type: string; ratio: number; total: number; edit: number }[] = [];

  // 按类型统计
  const typeStats = new Map<string, { total: number; edit: number }>();

  for (const sa of subAgents) {
    const stats = computeOutputRatio(sa.messages);
    totalToolUse += stats.total;
    totalEditUse += stats.edit;

    // 按类型聚合
    if (!typeStats.has(sa.subagentType)) {
      typeStats.set(sa.subagentType, { total: 0, edit: 0 });
    }
    const ts = typeStats.get(sa.subagentType)!;
    ts.total += stats.total;
    ts.edit += stats.edit;

    if (stats.total > 0) {
      ratios.push({
        id: sa.thread.id,
        type: sa.subagentType,
        ratio: stats.edit / stats.total,
        total: stats.total,
        edit: stats.edit,
      });
    }
  }

  if (totalToolUse === 0) {
    printWarning("无数据", "没有可用的工具调用数据");
    return;
  }

  const overallRatio = totalEditUse / totalToolUse;
  printMetric("总 tool_use 数", totalToolUse);
  printMetric("编辑类 tool_use 数", totalEditUse);
  printMetric("总体产出比", pct(totalEditUse, totalToolUse));

  // 按类型分层
  printSection("按 SubAgent 类型分层");
  printTable(
    ["类型", "SubAgent 数", "tool_use 总数", "编辑类", "产出比"],
    [...typeStats.entries()]
      .sort((a, b) => b[1].total - a[1].total)
      .map(([t, s]) => [
        t + (NON_EDITING_TYPES.has(t) ? " *" : ""),
        String(subAgents.filter((sa) => sa.subagentType === t).length),
        String(s.total),
        String(s.edit),
        pct(s.edit, s.total || 1),
      ]),
  );
  console.log("  * 非编辑型（本职不含编辑任务）");

  // 编辑型单独统计
  printSection("编辑型 SubAgent 产出比（排除非编辑型）");
  const editingRatios = ratios.filter((r) => !NON_EDITING_TYPES.has(r.type));
  const editingTotal = editingRatios.reduce((s, r) => s + r.total, 0);
  const editingEdit = editingRatios.reduce((s, r) => s + r.edit, 0);

  printMetric("编辑型总 tool_use", editingTotal);
  printMetric("编辑型编辑类产出", editingEdit);
  printMetric("编辑型产出比", pct(editingEdit, editingTotal || 1));

  if (editingRatios.length > 0) {
    printMetric(
      "P50 产出比",
      pct(p50(editingRatios.map((r) => r.ratio)), 1),
    );
    printMetric(
      "P95 产出比",
      pct(p95(editingRatios.map((r) => r.ratio)), 1),
    );

    // 分布桶（仅编辑型）
    const buckets: Record<string, number> = {
      "0": 0,
      "0-20%": 0,
      "20-50%": 0,
      "50-80%": 0,
      "80%+": 0,
    };
    for (const r of editingRatios) {
      if (r.ratio === 0) buckets["0"]++;
      else if (r.ratio <= 0.2) buckets["0-20%"]++;
      else if (r.ratio <= 0.5) buckets["20-50%"]++;
      else if (r.ratio <= 0.8) buckets["50-80%"]++;
      else buckets["80%+"]++;
    }

    console.log("");
    printTable(
      ["产出比范围", "数量", "占比", "分布"],
      Object.entries(buckets).map(([label, count]) => [
        label,
        String(count),
        pct(count, editingRatios.length),
        "█".repeat(Math.round((count / Math.max(1, editingRatios.length)) * 40)),
      ]),
    );
  }

  printBar("编辑型产出比", editingEdit / (editingTotal || 1));
}

// ═══════════════════════════════════════════════════
// 指标 5：SubAgent 类型分布与工具使用模式
// ═══════════════════════════════════════════════════

const SEARCH_TOOLS = new Set(["Read", "Grep", "Glob", "WebFetch", "WebSearch", "folder_operations"]);
const EXEC_TOOLS = new Set(["Bash", "Agent", "AgentResult", "TodoWrite", "AskUserQuestion"]);

// ═══════════════════════════════════════════════════
// 研究方向：general-purpose 场景特化
// ═══════════════════════════════════════════════════

/** 任务关键词分类 */
function classifyTask(prompt: string): string {
  const lower = prompt.toLowerCase();
  if (/implement|实现|write code|编写|修改|添加|新增/.test(lower)) return "实现";
  if (/fix|修复|bug|issue/.test(lower)) return "修复";
  if (/explore|探索|search|查找|了解|调研|分析/.test(lower)) return "探索";
  if (/review|审查|检查|评审/.test(lower)) return "审查";
  if (/refactor|重构/.test(lower)) return "重构";
  if (/create|创建|新建|生成/.test(lower)) return "创建";
  return "其他";
}

function analyzeGeneralPurposeResearch(
  loader: DataLoader,
  subAgentThreads: ThreadRow[],
  typeMap: Map<string, string>,
): void {
  printSeparator();
  printSection("研究方向：general-purpose 场景特化分析");
  console.log(
    chalk.dim("  目标：分析 general-purpose SubAgent 的实际使用模式，为创建新的特化 agent 提供依据\n"),
  );

  const gpThreads = subAgentThreads.filter(
    (t) => typeMap.get(t.id) === "general-purpose",
  );

  if (gpThreads.length === 0) {
    printWarning("无数据", "未找到 general-purpose SubAgent");
    return;
  }

  // 按父线程分组，提取 Agent tool_use 的 prompt
  const byParent = new Map<string, { threadId: string; msgCount: number }[]>();
  for (const t of gpThreads) {
    if (!t.parent_thread_id) continue;
    if (!byParent.has(t.parent_thread_id)) byParent.set(t.parent_thread_id, []);
    byParent.get(t.parent_thread_id)!.push({ threadId: t.id, msgCount: t.message_count });
  }

  // 对每个 general-purpose 实例收集数据
  interface GpInstance {
    id: string;
    msgCount: number;
    prompt: string;
    taskType: string;
    tools: string[];
  }
  const instances: GpInstance[] = [];

  for (const [pid, children] of byParent) {
    const msgs = loader.loadMessages(pid);
    const agentCalls: { subagentType: string; prompt: string }[] = [];

    for (const m of msgs) {
      const parsed = DataLoader.parseContent(m.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const blocks: ContentBlock[] = Array.isArray(parsed.content) ? parsed.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use" && block.name === "Agent") {
          const input = block.input as any;
          agentCalls.push({
            subagentType: input?.subagent_type || input?.type || "",
            prompt:
              typeof input?.prompt === "string"
                ? input.prompt
                : JSON.stringify(input?.prompt || ""),
          });
        }
      }
    }

    for (let i = 0; i < children.length && i < agentCalls.length; i++) {
      const call = agentCalls[i];
      if (call.subagentType !== "general-purpose") continue;

      const childMsgs = loader.loadMessages(children[i].threadId);
      const tools = new Set<string>();
      for (const cm of childMsgs) {
        const parsed = DataLoader.parseContent(cm.content);
        if (!parsed || parsed.role !== "assistant") continue;
        const blocks: ContentBlock[] = Array.isArray(parsed.content)
          ? parsed.content
          : [];
        for (const block of blocks) {
          if (block.type === "tool_use") tools.add(block.name);
        }
      }

      instances.push({
        id: children[i].threadId,
        msgCount: children[i].msgCount,
        prompt: call.prompt,
        taskType: classifyTask(call.prompt),
        tools: [...tools].sort(),
      });

      // 收集每实例的工具计数（用于 Grep 重复分析）
      const toolCounts = new Map<string, number>();
      for (const cm of childMsgs) {
        const parsed = DataLoader.parseContent(cm.content);
        if (!parsed || parsed.role !== "assistant") continue;
        const blocks: ContentBlock[] = Array.isArray(parsed.content) ? parsed.content : [];
        for (const block of blocks) {
          if (block.type === "tool_use") {
            toolCounts.set(block.name, (toolCounts.get(block.name) || 0) + 1);
          }
        }
      }
      // 将计数存在实例上（动态属性）
      (instances[instances.length - 1] as any).toolCounts = Object.fromEntries(toolCounts);
      (instances[instances.length - 1] as any).errorCount = 0;
      // 统计 tool_result 错误
      for (const cm of childMsgs) {
        const parsed = DataLoader.parseContent(cm.content);
        if (parsed && parsed.role === "tool") {
          if ((parsed as any).is_error)
            (instances[instances.length - 1] as any).errorCount++;
        }
      }
    }
  }

  // ── 分类 ──
  const searchEditors: GpInstance[] = [];
  const pureSearchers: GpInstance[] = [];

  for (const inst of instances) {
    const hasEdit = inst.tools.some((t) =>
      ["Write"].includes(t),
    );
    const hasSearch = inst.tools.some((t) => SEARCH_TOOLS.has(t));
    if (hasEdit && hasSearch) searchEditors.push(inst);
    else if (!hasEdit) pureSearchers.push(inst);
  }

  // ── 输出 ──
  printSection("总体分布");
  console.log("");
  printTable(
    ["模式", "数量", "占比", "均消息", "P95", "方向"],
    [
      [
        "搜索+编辑",
        String(searchEditors.length),
        pct(searchEditors.length, instances.length || 1),
        String(Math.round(avg(searchEditors.map((i) => i.msgCount)))),
        String(Math.round(p95(searchEditors.map((i) => i.msgCount)))),
        "coder",
      ],
      [
        "纯搜索",
        String(pureSearchers.length),
        pct(pureSearchers.length, instances.length || 1),
        String(Math.round(avg(pureSearchers.map((i) => i.msgCount)))),
        String(Math.round(p95(pureSearchers.map((i) => i.msgCount)))),
        "explore",
      ],
    ],
  );

  // ── 每会话工具明细 ──
  const allGp = [...searchEditors, ...pureSearchers]
    .sort((a, b) => b.msgCount - a.msgCount);

  printSeparator();
  printSection("会话工具明细（按消息数降序）");
  console.log("");

  const detailRows: string[][] = [];
  for (const inst of allGp) {
    const tc = (inst as any).toolCounts as Record<string, number> || {};
    const top3 = Object.entries(tc)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 3)
      .map(([t, c]) => `${t}×${c}`)
      .join(" ");
    const hasEdit = inst.tools.some((t) => EDIT_OUTPUT_TOOLS.has(t));
    const hasSearch = inst.tools.some((t) => SEARCH_TOOLS.has(t));
    const pattern = hasEdit ? "编辑" : "搜索";
    const toolCount = Object.keys(tc).length;
    const errorCount = (inst as any).errorCount || 0;
    const errorTag = errorCount > 0 ? ` err:${errorCount}` : "";
    // Highlight sessions with extreme Grep usage
    const grepHits = tc["Grep"] || 0;
    const tag = grepHits > 100
      ? chalk.red(` ⚠ Grep×${grepHits}`)
      : grepHits > 50
        ? chalk.yellow(` Grep×${grepHits}`)
        : "";

    detailRows.push([
      inst.id.slice(0, 14) + "...",
      String(inst.msgCount),
      inst.taskType,
      pattern,
      String(toolCount) + "种",
      top3 + errorTag,
      tag,
    ]);
  }

  printTable(
    ["ID", "消息", "任务", "模式", "工具种", "Top 3 工具", "异常"],
    detailRows,
  );

  // ── Grep 重复分析 ──
  printSeparator();
  printSection("Grep 重复搜索分析");
  const highGrepInstances = allGp.filter(
    (inst) => ((inst as any).toolCounts?.Grep || 0) > 30,
  );
  if (highGrepInstances.length > 0) {
    console.log(
      chalk.yellow(
        `  检测到 ${highGrepInstances.length} 个会话 Grep 调用 > 30 次，可能存在搜索循环`,
      ),
    );
    for (const inst of highGrepInstances) {
      const grepHits = (inst as any).toolCounts?.Grep || 0;
      const totalTools = Object.values(
        (inst as any).toolCounts || {},
      ).reduce((s: number, c: number) => s + c, 0);
      const grepPct = pct(grepHits, totalTools || 1);
      console.log(
        chalk.dim(
          `    ${inst.id.slice(0, 8)}  Grep ${grepHits}/${totalTools} (${grepPct})  ${inst.taskType}  [${inst.msgCount}条]`,
        ),
      );
    }
    console.log(
      chalk.dim("\n  根因推测: 上下文丢失 → 忘记搜索结果 → 重复搜索 → 上下文进一步膨胀"),
    );
  } else {
    console.log("  未检测到异常 Grep 模式");
  }

  // ── Coder Agent 完整 spec ──
  printSeparator();
  printSection("Coder Agent 特化规格");

  // 统计所有编辑+搜索实例的工具使用
  const allEditTools = new Map<string, number>();
  let editTotal = 0;
  for (const inst of searchEditors) {
    const tc = (inst as any).toolCounts as Record<string, number> || {};
    for (const [t, c] of Object.entries(tc)) {
      allEditTools.set(t, (allEditTools.get(t) || 0) + c);
      editTotal += c;
    }
  }

  const usedTools = [...allEditTools.entries()]
    .sort((a, b) => b[1] - a[1]);

  console.log("");
  printTable(
    ["工具", "调用数", "占比", "保留", "说明"],
    usedTools.map(([tool, count]) => {
      const use = count / (editTotal || 1);
      const keep =
        ["Read", "Grep", "Glob", "Bash", "Write", "TodoWrite"].includes(tool);
      const reason = use === 0
        ? "从未使用"
        : use < 0.005
          ? "几乎不用"
          : keep
            ? ""
            : "低使用率";
      return [
        tool,
        String(count),
        pct(count, editTotal || 1),
        keep ? chalk.green("✓") : chalk.red("✗"),
        reason,
      ];
    }),
  );

  console.log("");
  console.log(chalk.bold("  Coder Agent 定义:"));
  console.log(chalk.green("  工具集 (6个):"));
  console.log(chalk.dim("    Read   — 必读源码，理解上下文"));
  console.log(chalk.dim("    Grep   — 查找引用，确认影响面"));
  console.log(chalk.dim("    Glob   — 文件发现，目录结构"));
  console.log(chalk.dim("    Bash   — 目录探索 + 构建/测试验证"));
  console.log(chalk.dim("    Write  — 文件写入"));
  console.log(chalk.dim("    TodoWrite — 多步骤任务追踪"));
  console.log(chalk.red("  移除:"));
  console.log(
    chalk.dim(
      `    WebSearch(${allEditTools.get("WebSearch") || 0}) WebFetch(${allEditTools.get("WebFetch") || 0}) Agent(${allEditTools.get("Agent") || 0}) AskUserQuestion(${allEditTools.get("AskUserQuestion") || 0})`,
    ),
  );

  const msgVals = searchEditors.map((i) => i.msgCount);
  const p95Val = Math.round(p95(msgVals));
  const maxVal = msgVals.sort((a, b) => b - a)[0];
  const p50Val = Math.round(p50(msgVals));

  console.log("");
  console.log(chalk.bold("  迭代上限:"));
  console.log(
    chalk.dim(
      `    200 轮（成功案例 P50=${p50Val} P95=${p95Val}，给安全余量）`,
    ),
  );

  console.log("");
  console.log(chalk.bold("  上下文预算:"));
  console.log(
    chalk.dim(
      `    比 general-purpose 小 ~30%（压缩描述 token + 迭代上限，减少 Grep 搜索循环风险）`,
    ),
  );
  console.log(
    chalk.dim(
      `    依据: 同任务对比 — coder 候选版 153msgs 完成，全功能版 717msgs(584×Grep) 失败`,
    ),
  );

  console.log("");
  console.log(chalk.bold("  适用任务:"));
  const taskDist = new Map<string, number>();
  for (const inst of searchEditors) taskDist.set(inst.taskType, (taskDist.get(inst.taskType) || 0) + 1);
  console.log(
    chalk.dim(
      `    ${[...taskDist.entries()].sort((a,b) => b[1]-a[1]).map(([t,c]) => `${t}(${c})`).join(", ")}`,
    ),
  );

  // ── Explore 建议 ──
  if (pureSearchers.length > 0) {
    printSeparator();
    printSection("Explore 替代建议");
    const pctGp = pct(pureSearchers.length, gpThreads.length);
    console.log(
      chalk.green(
        `  ${pureSearchers.length} 个 (${pctGp}) general-purpose 仅做搜索/阅读 → 可用 explore 替代`,
      ),
    );
    console.log(chalk.dim("  节省: 去掉编辑工具描述 + 迭代上限降至 20 轮"));
    const msgAll = pureSearchers.map((i) => i.msgCount);
    console.log(
      chalk.dim(
        `  当前消耗: ${msgAll.reduce((a, b) => a + b, 0)} 条消息, 均 ${Math.round(avg(msgAll))} 条/次`,
      ),
    );
  }

  console.log("");
}

// ═══════════════════════════════════════════════════
// 会话导出：供人工评估
// ═══════════════════════════════════════════════════

function parseExportCount(): number {
  const idx = process.argv.indexOf("--export");
  if (idx < 0) return 0;
  const val = parseInt(process.argv[idx + 1], 10);
  return val > 0 ? val : 0;
}

function exportGeneralPurposeSessions(
  loader: DataLoader,
  subAgentThreads: ThreadRow[],
  typeMap: Map<string, string>,
  exportCount: number,
): void {
  if (exportCount <= 0) return;

  const fs = require("fs");
  const path = require("path");
  const exportDir = path.join(
    path.dirname(new URL(import.meta.url).pathname),
    "..",
    "..",
    "exports",
  );
  if (!fs.existsSync(exportDir)) fs.mkdirSync(exportDir, { recursive: true });

  const gpThreads = subAgentThreads.filter(
    (t) => typeMap.get(t.id) === "general-purpose" && t.parent_thread_id,
  );

  if (gpThreads.length === 0) {
    console.log(chalk.yellow("  没有 general-purpose SubAgent 可导出"));
    return;
  }

  printSeparator();
  printSection(`导出 general-purpose 会话文本（前 ${exportCount} 个）`);

  // 按消息数降序，取前 N 个
  const selected = [...gpThreads]
    .sort((a, b) => b.message_count - a.message_count)
    .slice(0, exportCount);

  for (let i = 0; i < selected.length; i++) {
    const sa = selected[i];
    const parentMsgs = loader.loadMessages(sa.parent_thread_id!);
    const childMsgs = loader.loadMessages(sa.id);

    // 找父线程中创建此 SubAgent 的 Agent 调用
    let parentPrompt = "(未提取到 prompt)";
    for (const m of parentMsgs) {
      const parsed = DataLoader.parseContent(m.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const blocks: ContentBlock[] = Array.isArray(parsed.content) ? parsed.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use" && block.name === "Agent") {
          const tp = (block.input as any)?.prompt;
          if (tp) parentPrompt = typeof tp === "string" ? tp : JSON.stringify(tp);
        }
      }
    }

    // 构建会话文本
    let text = "";
    text += `═`.repeat(80) + "\n";
    text += `General-purpose SubAgent 会话 #${i + 1}\n`;
    text += `═`.repeat(80) + "\n";
    text += `ID: ${sa.id}\n`;
    text += `消息数: ${sa.message_count}\n`;
    text += `创建时间: ${sa.created_at}\n`;
    text += `更新時間: ${sa.updated_at}\n`;
    text += `狀態: ${sa.agent_status}\n`;
    text += "\n";
    text += `── 父线程 prompt ──\n`;
    text += parentPrompt + "\n";
    text += "\n";
    text += `── SubAgent 消息序列（${childMsgs.length} 条）──\n`;

    for (let j = 0; j < childMsgs.length; j++) {
      const msg = childMsgs[j];
      const parsed = DataLoader.parseContent(msg.content);

      text += `\n[#${j}] ${msg.role}\n`;

      if (!parsed) {
        text += `  (parse error)\n`;
        continue;
      }

      if (parsed.role === "user" || parsed.role === "system") {
        const content = typeof parsed.content === "string" ? parsed.content : JSON.stringify(parsed.content);
        text += `  ${content.slice(0, 500)}\n`;
      } else if (parsed.role === "assistant") {
        const blocks: ContentBlock[] = Array.isArray(parsed.content) ? parsed.content : [];
        for (const block of blocks) {
          if (block.type === "text") {
            text += `  [text] ${(block as any).text.slice(0, 300)}\n`;
          } else if (block.type === "tool_use") {
            const tu = block as any;
            const argsStr = typeof tu.input === "object" ? JSON.stringify(tu.input).slice(0, 200) : String(tu.input || "").slice(0, 200);
            text += `  [tool_use] ${tu.name} @${tu.id.slice(0, 8)} ${argsStr}\n`;
          } else if (block.type === "reasoning" || block.type === "thinking") {
            const rt = (block as any).text || "";
            text += `  [${block.type}] ${rt.slice(0, 100)}\n`;
          }
        }
      } else if (parsed.role === "tool") {
        const tc = parsed as any;
        const result = typeof tc.content === "string" ? tc.content : JSON.stringify(tc.content);
        text += `  [tool_result] ${tc.tool_call_id?.slice(0, 8) || "?"} error=${tc.is_error || false} ${result.slice(0, 300)}\n`;
      }
    }

    const filePath = path.join(
      exportDir,
      `gp_session_${sa.id.slice(0, 8)}_${sa.message_count}msgs.txt`,
    );
    fs.writeFileSync(filePath, text, "utf-8");
    console.log(`  ${chalk.green("✓")} ${filePath}`);
  }

  console.log(
    chalk.dim(`\n  导出完成，文件位于 ${exportDir}/`),
  );
}

// ═══════════════════════════════════════════════════
// 辅助
// ═══════════════════════════════════════════════════

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

  printHeader("场景六：SubAgent 协作");

  // 加载 SubAgent 线程
  const subAgentThreads = loader.loadAllSubAgents();

  if (subAgentThreads.length === 0) {
    printWarning("无 SubAgent", "数据库中没有 SubAgent 线程");
    loader.close();
    return;
  }

  printMetric("SubAgent 总数", subAgentThreads.length);

  // 时间过滤：通过父线程判断
  let filteredSubAgents: ThreadRow[];
  if (sinceHours) {
    // SubAgent 没有直接的 updated_at 过滤，通过加载所有主线程再关联
    const mainThreads = loader.loadVisibleThreadsSince(sinceHours);
    const mainIds = new Set(mainThreads.map((t) => t.id));
    filteredSubAgents = subAgentThreads.filter(
      (sa) => sa.parent_thread_id && mainIds.has(sa.parent_thread_id),
    );
    printMetric("时间范围", `最近 ${sinceHours} 小时`);
    printMetric("过滤后 SubAgent 数", filteredSubAgents.length);
  } else {
    filteredSubAgents = subAgentThreads;
  }

  printSeparator();

  // 推断 SubAgent 类型（从父线程的 Agent 调用中提取 subagent_type）
  const typeMap = buildSubAgentTypeMap(loader, filteredSubAgents);
  const unknownCount = [...typeMap.values()].filter((t) => t === "unknown").length;
  if (unknownCount > 0) {
    // 静默处理：类型未知时默认可编辑

  }

  // 批量加载消息（只加载一次，各指标复用）
  const analyses: SubAgentAnalysis[] = [];
  for (const t of filteredSubAgents) {
    const messages = loader.loadMessages(t.id);
    const saType = typeMap.get(t.id) || "unknown";
    analyses.push({
      thread: t,
      messages,
      subagentType: saType,
      toolUseCount: 0,
      editToolUseCount: 0,
      toolErrorCount: 0,
      toolErrorRate: 0,
    });
  }

  // ── 指标 1：内置 Agent 分类分析 ──
  analyzeAgentClassification(analyses);

  // ── 指标 2：消息量 ──
  analyzeMessageVolume(filteredSubAgents);

  // ── 指标 3：工具错误率 ──
  analyzeToolErrorRate(analyses);

  // ── 指标 4：产出比 ──
  analyzeOutputRatio(analyses);

  // ── 研究方向：general-purpose 场景特化 ──
  analyzeGeneralPurposeResearch(loader, filteredSubAgents, typeMap);

  // ── 导出会话文本 ──
  const exportCount = parseExportCount();
  exportGeneralPurposeSessions(loader, filteredSubAgents, typeMap, exportCount);

  loader.close();
}

main();
