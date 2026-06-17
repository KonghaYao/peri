//! 场景四：功能采纳分析。
//!
//! 3 项指标：Skill 调用频率、Skill 链深度、工具使用多样性。
//! 注：LineEdit 使用率/成功率指标已随 LineEdit 移除而废弃。
//!
//! 用法：bun run src/metrics/feature_adoption.ts [--since 24]

import { DataLoader, type ThreadRow, type MessageRow, type ParsedMessage, type AiContent, type ContentBlock, type ToolCallRequest } from "../data/loader.js";
import { avg, median, p50, p95, pct, formatSize, formatDuration, parseSinceArg, printHeader, printSection, printMetric, printWarning, printTable, printBar, printSeparator } from "../lib/utils.js";

// ── 常量 ──

const SKILL_PATH_RE = /skills\/([\w][-\w]*)\/SKILL\.md/gi;

// ── 入口 ──

const sinceHours = parseSinceArg();
const loader = new DataLoader();
if (sinceHours) printMetric("时间范围", `最近 ${sinceHours} 小时`);
const threads = sinceHours ? loader.loadVisibleThreadsSince(sinceHours) : loader.loadVisibleThreads();

printHeader("场景四：功能采纳");

printMetric("可见会话数", threads.length);

// 1. Skill 调用频率（两维度）
analyzeSkillFrequency(loader, threads);

// 2. Skill 链深度
analyzeSkillChainDepth(loader, threads);

// 3. 工具使用多样性
analyzeToolDiversity(loader, threads);

loader.close();

// ═══════════════════════════════════════════════════
// 辅助：提取消息文本
// ═══════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════
// 指标 1：Skill 调用频率（两维度）
// ═══════════════════════════════════════════════════

function analyzeSkillFrequency(loader: DataLoader, threads: ThreadRow[]): void {
  printSection("1. Skill 调用频率");

  // ── 维度一：System 消息中的 skill 加载标记 ──
  const skillLoadCounts: Record<string, number> = {};

  for (const thread of threads) {
    const messages = loader.loadMessages(thread.id);
    for (const msg of messages) {
      if (msg.role !== "system") continue;
      const content = (() => {
        try {
          const parsed = JSON.parse(msg.content);
          if (typeof parsed.content === "string") return parsed.content;
          if (Array.isArray(parsed.content)) {
            return parsed.content
              .filter((b: any) => b.type === "text")
              .map((b: any) => b.text || "")
              .join("\n");
          }
        } catch { /* ignore */ }
        return "";
      })();

      // 从 SKILL.md 路径中提取 skill 名称
      const pathMatches = content.matchAll(SKILL_PATH_RE);
      const seenInMsg = new Set<string>();
      for (const pm of pathMatches) {
        const skillName = pm[1].toLowerCase();
        if (!seenInMsg.has(skillName)) {
          seenInMsg.add(skillName);
          skillLoadCounts[skillName] = (skillLoadCounts[skillName] || 0) + 1;
        }
      }
    }
  }

  printSection("  维度一：System 消息 Skill 加载频次");
  if (Object.keys(skillLoadCounts).length > 0) {
    const skillRows = Object.entries(skillLoadCounts)
      .sort((a, b) => b[1] - a[1])
      .map(([name, count]) => [name, String(count)]);
    printTable(["Skill 名称", "加载次数"], skillRows);
    printMetric("不同 Skill 种类", Object.keys(skillLoadCounts).length);
  } else {
    console.log("  未在 System 消息中检测到 Skill 加载标记");
  }

  // ── 维度二：Agent 工具调用的 subagent_type 参数 ──
  const subagentTypeCounts: Record<string, number> = {};

  for (const thread of threads) {
    const messages = loader.loadMessages(thread.id);
    for (const msg of messages) {
      if (msg.role !== "assistant") continue;
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed) continue;
      const toolCalls = DataLoader.extractToolCalls(parsed);
      for (const tc of toolCalls) {
        if (tc.name !== "Agent") continue;
        const st = tc.arguments.subagent_type;
        if (typeof st === "string" && st.length > 0) {
          subagentTypeCounts[st] = (subagentTypeCounts[st] || 0) + 1;
        }
      }
    }
  }

  printSection("  维度二：SubAgent 类型频次");
  if (Object.keys(subagentTypeCounts).length > 0) {
    const saRows = Object.entries(subagentTypeCounts)
      .sort((a, b) => b[1] - a[1])
      .map(([name, count]) => [name, String(count)]);
    printTable(["SubAgent 类型", "调用次数"], saRows);
    printMetric("不同 SubAgent 类型", Object.keys(subagentTypeCounts).length);
  } else {
    console.log("  未发现 Agent 工具调用（含 subagent_type）");
  }
}

// ═══════════════════════════════════════════════════
// 指标 2：Skill 链深度
// ═══════════════════════════════════════════════════

function analyzeSkillChainDepth(loader: DataLoader, threads: ThreadRow[]): void {
  printSection("2. Skill 链深度");

  // 获取主会话（parent_thread_id IS NULL 且可见）
  const mainThreads = sinceHours
    ? threads.filter((t) => t.parent_thread_id === null)
    : loader.loadAllMainThreads();

  const depths: number[] = [];

  for (const mt of mainThreads) {
    const depth = computeChainDepth(loader, mt.id, 1);
    depths.push(depth);
  }

  if (depths.length === 0) {
    console.log("  无主会话数据");
    return;
  }

  printMetric("主会话数", mainThreads.length);
  printMetric("最大链深度", Math.max(...depths));
  printMetric("平均链深度", avg(depths).toFixed(1));
  printMetric("链深度 P50", String(p50(depths)));
  printMetric("链深度 P95", String(p95(depths)));

  // 深度分布
  const buckets: Record<string, number> = {
    "深度 1": 0,
    "深度 2": 0,
    "深度 3": 0,
    "深度 4": 0,
    "深度 5+": 0,
  };

  for (const d of depths) {
    if (d === 1) buckets["深度 1"]++;
    else if (d === 2) buckets["深度 2"]++;
    else if (d === 3) buckets["深度 3"]++;
    else if (d === 4) buckets["深度 4"]++;
    else buckets["深度 5+"]++;
  }

  const distRows = Object.entries(buckets)
    .filter(([, c]) => c > 0)
    .map(([label, count]) => [label, String(count), pct(count, depths.length)]);

  printTable(["深度", "会话数", "占比"], distRows);

  // 打印各深度条
  for (const [label, count] of Object.entries(buckets)) {
    if (count > 0) {
      printBar(label, count / depths.length, 25);
    }
  }
}

function computeChainDepth(loader: DataLoader, threadId: string, currentDepth: number): number {
  const subs = loader.loadSubAgents(threadId);
  if (subs.length === 0) return currentDepth;

  let maxSubDepth = currentDepth;
  for (const sub of subs) {
    const subDepth = computeChainDepth(loader, sub.id, currentDepth + 1);
    maxSubDepth = Math.max(maxSubDepth, subDepth);
  }
  return maxSubDepth;
}

// ═══════════════════════════════════════════════════
// 指标 3：工具使用多样性
// ═══════════════════════════════════════════════════

function analyzeToolDiversity(loader: DataLoader, threads: ThreadRow[]): void {
  printSection("3. 工具使用多样性");

  const sessionDiversity: { threadId: string; uniqueTools: number; totalCalls: number }[] = [];

  for (const thread of threads) {
    const toolSet = new Set<string>();
    let totalCalls = 0;

    const messages = loader.loadMessages(thread.id);
    for (const msg of messages) {
      if (msg.role !== "assistant") continue;
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed) continue;
      const toolCalls = DataLoader.extractToolCalls(parsed);
      for (const tc of toolCalls) {
        toolSet.add(tc.name);
        totalCalls++;
      }
    }

    if (toolSet.size > 0) {
      sessionDiversity.push({
        threadId: thread.id,
        uniqueTools: toolSet.size,
        totalCalls,
      });
    }
  }

  if (sessionDiversity.length === 0) {
    console.log("  无工具调用数据");
    return;
  }

  const uniqueCounts = sessionDiversity.map((s) => s.uniqueTools);
  printMetric("有工具调用的会话数", sessionDiversity.length);
  printMetric("每会话最多工具种类", Math.max(...uniqueCounts));
  printMetric("每会话 P50 工具种类", String(p50(uniqueCounts)));
  printMetric("每会话 P95 工具种类", String(p95(uniqueCounts)));
  printMetric("每会话平均工具种类", avg(uniqueCounts).toFixed(1));

  // 分布桶
  const buckets: Record<string, number> = {
    "1-3 种": 0,
    "4-6 种": 0,
    "7-10 种": 0,
    "11-15 种": 0,
    "16+ 种": 0,
  };

  for (const { uniqueTools } of sessionDiversity) {
    if (uniqueTools <= 3) buckets["1-3 种"]++;
    else if (uniqueTools <= 6) buckets["4-6 种"]++;
    else if (uniqueTools <= 10) buckets["7-10 种"]++;
    else if (uniqueTools <= 15) buckets["11-15 种"]++;
    else buckets["16+ 种"]++;
  }

  const distRows = Object.entries(buckets)
    .filter(([, c]) => c > 0)
    .map(([label, count]) => [label, String(count), pct(count, sessionDiversity.length)]);

  printTable(["种类范围", "会话数", "占比"], distRows);

  // 打印分布条
  for (const [label, count] of Object.entries(buckets)) {
    if (count > 0) {
      printBar(label, count / sessionDiversity.length, 30);
    }
  }
}
