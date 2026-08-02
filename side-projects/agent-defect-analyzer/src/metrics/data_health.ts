//! 数据健康检查 — 检测线程/消息数据的完整性异常。
//!
//! 背景：2026-07-16 起 sub-agent 消息不再写入 threads.db（message_count 恒为 0、
//! messages 表无记录），导致 SubAgent 协作分析全部失明。此类数据层异常
//! 无指标可反映（工具失败率等全部正常），只有直接检查数据健康才能发现。
//!
//! 检查项：
//! 1. Sub-agent 消息缺失 —— 按天统计 hidden 子线程 message_count=0 占比与趋势，
//!    识别"消息持久化中断"类回归（高优，跨分析器盲区）
//! 2. 孤儿 tool_result —— tool_result 找不到配对 tool_use（compact/rewind 残留，
//!    导致 tool_reliability 出现 call_00_* 伪工具名）
//! 3. 极短会话 —— 放弃信号占比 + 项目分布

import {
  DataLoader,
  type ThreadRow,
  type AiContent,
  type ToolContent,
  type ContentBlock,
} from "../data/loader.js";
import {
  pct,
  parseSinceArg,
  printHeader,
  printSection,
  printMetric,
  printWarning,
  printTable,
  printSeparator,
} from "../lib/utils.js";

const METRIC_TITLE = "数据健康检查";

// ── Main ──

const sinceHours = parseSinceArg();
const loader = new DataLoader();

printHeader(METRIC_TITLE);
if (sinceHours) printMetric("时间范围", `最近 ${sinceHours} 小时`);

const allThreads = loader.loadAllThreads();
printMetric("线程总数（含 hidden）", allThreads.length);

analyzeSubAgentPersistence(allThreads);
analyzeOrphanToolResults(loader, sinceHours);
analyzeAbandonedSessions(loader, sinceHours);

loader.close();

// ── 指标 1: Sub-agent 消息缺失检测 ──

function analyzeSubAgentPersistence(allThreads: ThreadRow[]): void {
  printSection("1. Sub-agent 消息持久化检查");

  const subAgents = allThreads.filter((t) => t.parent_thread_id !== null);
  if (subAgents.length === 0) {
    printWarning("无 Sub-agent", "数据库中没有 sub-agent 线程");
    return;
  }

  printMetric("Sub-agent 总数", subAgents.length);

  // 按天聚合：创建数 / 有消息数 / 空消息数
  const byDay = new Map<string, { total: number; withMsg: number }>();
  for (const sa of subAgents) {
    const day = sa.created_at.slice(0, 10);
    const d = byDay.get(day) ?? { total: 0, withMsg: 0 };
    d.total++;
    if (sa.message_count > 0) d.withMsg++;
    byDay.set(day, d);
  }

  const days = [...byDay.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1)).slice(-14);

  const rows = days.map(([day, d]) => [
    day,
    String(d.total),
    String(d.withMsg),
    pct(d.withMsg, d.total),
  ]);
  printTable(["日期", "子线程数", "有消息数", "持久化率"], rows);

  // 最近 7 天窗口判定
  const recent = days.slice(-7);
  const recentTotal = recent.reduce((s, d) => s + d[1].total, 0);
  const recentWithMsg = recent.reduce((s, d) => s + d[1].withMsg, 0);
  const rate = recentTotal > 0 ? recentWithMsg / recentTotal : 0;

  printMetric("最近 7 天持久化率", pct(recentWithMsg, recentTotal));
  printSeparator();

  if (recentTotal > 0 && rate < 0.05) {
    printWarning(
      "⚠ Sub-agent 消息持久化疑似中断",
      `最近 7 天 ${recentTotal} 个子线程仅 ${recentWithMsg} 个有消息（${pct(recentWithMsg, recentTotal)}）。` +
        "子线程记录存在但消息未写入 messages 表——所有依赖 sub-agent 消息的分析（SubAgent 协作）将失明。" +
        "请检查 peri 主仓库 sub-agent 会话是否绑定了 thread_store/thread_id（参考 v2_bridge.rs 的 Session 构造）。",
    );
  } else if (recentTotal > 0 && rate < 0.9) {
    printWarning(
      "⚠ Sub-agent 消息持久化率偏低",
      `最近 7 天 ${pct(recentWithMsg, recentTotal)} 的子线程有消息。若趋势下降，可能正在走向持久化中断。`,
    );
  } else {
    printMetric("状态", "🟢 正常（≥90% 子线程有消息）");
  }
}

// ── 指标 2: 孤儿 tool_result 检测 ──

function analyzeOrphanToolResults(loader: DataLoader, sinceHours?: number): void {
  printSection("2. 孤儿 tool_result 检测");

  const threads = sinceHours
    ? loader.loadVisibleThreadsSince(sinceHours)
    : loader.loadVisibleThreads();

  let orphan = 0;
  let totalResults = 0;
  let orphanSamples: string[] = [];

  for (const t of threads) {
    const messages = loader.loadMessages(t.id);
    const toolUseIds = new Set<string>();
    for (const msg of messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "assistant") continue;
      const ai = parsed as AiContent;
      const blocks: ContentBlock[] = Array.isArray(ai.content) ? ai.content : [];
      for (const block of blocks) {
        if (block.type === "tool_use") toolUseIds.add(block.id);
      }
    }
    for (const msg of messages) {
      const parsed = DataLoader.parseContent(msg.content);
      if (!parsed || parsed.role !== "tool") continue;
      const tc = parsed as ToolContent;
      if (!tc.tool_call_id) continue;
      totalResults++;
      if (!toolUseIds.has(tc.tool_call_id)) {
        orphan++;
        if (orphanSamples.length < 3) orphanSamples.push(tc.tool_call_id.slice(0, 24));
      }
    }
  }

  printMetric("tool_result 总数", totalResults);
  printMetric("孤儿数（无配对 tool_use）", orphan);
  printMetric("孤儿率", pct(orphan, totalResults));
  printSeparator();

  if (totalResults > 0 && orphan / totalResults > 0.05) {
    printWarning(
      "⚠ 孤儿 tool_result 比例偏高",
      `样本: ${orphanSamples.join(", ")}。` +
        "通常由 compact/rewind 后 tool_use 被替换但 tool_result 残留导致，" +
        "会使 tool_reliability 出现 call_00_* 伪工具名并抬高工具种类数。",
    );
  }
}

// ── 指标 3: 极短会话（放弃信号）──

function analyzeAbandonedSessions(loader: DataLoader, sinceHours?: number): void {
  printSection("3. 极短会话（放弃信号）");

  const threads = sinceHours
    ? loader.loadVisibleThreadsSince(sinceHours)
    : loader.loadVisibleThreads();
  if (threads.length === 0) {
    printWarning("无会话", "窗口内无可见会话");
    return;
  }

  const short = threads.filter((t) => t.message_count <= 3);
  const withTools = short.filter((t) => t.message_count >= 2); // 有工具调用至少 2 条消息
  printMetric("极短会话（≤3 消息）", `${short.length} / ${threads.length}`);
  printMetric("放弃信号率", pct(short.length, threads.length));

  // cwd 分布（项目维度）
  const byCwd = new Map<string, number>();
  for (const t of short) {
    const key = t.cwd.split("/").slice(-2).join("/") || t.cwd;
    byCwd.set(key, (byCwd.get(key) ?? 0) + 1);
  }
  const rows = [...byCwd.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 8)
    .map(([cwd, n]) => [cwd, String(n), pct(n, short.length)]);
  printTable(["项目", "极短会话数", "占比"], rows);

  if (short.length / threads.length > 0.35) {
    printWarning(
      "⚠ 极短会话占比高",
      ">35% 会话仅 ≤3 条消息。若集中在某项目，多为快速问答模式；若普遍，需检查" +
        "是否大量会话被提前放弃（用户不满/响应异常）。结合 cwd 分布甄别。",
    );
  }
}
