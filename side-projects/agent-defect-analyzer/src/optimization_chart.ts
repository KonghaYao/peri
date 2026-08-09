//! optimization_chart.ts — 优化场景实证研究:每场景独立数据图
//!
//! 读取 src/data/tool-token-consumption.json 的 weekly 字段 + 调查报告口径数据,
//! 用 ECharts SSR 模式渲染 4 张独立 SVG(每场景一章一张),输出到 --out-dir(默认 reports/):
//!   chart-1-weekly.svg      场景 1:周出参总量(MB 柱)+ 单次 P95(KB 线,双 Y 轴)
//!   chart-2-truncation.svg  场景 2:截断提示 vs 落盘提示 月度次数(双柱)
//!   chart-4-notfound.svg    场景 4:not found 调用 月度次数(柱)
//!   chart-5-polling.svg     场景 5:轮询等待事件数与最长连续链(柱 + 线)
//!
//! 数据口径:
//! - 场景 1 图:脚本实时计算(src/data/tool-token-consumption.json, weekly 字段)
//! - 其余图:threads.db 消息文本统计(调查口径,月度;8 月仅含 1-9 日),
//!   数值与《reports/optimization-verification.md》(2026-08-09)一致,硬编码于此并注明来源
//!
//! 用法:
//!   bun run src/optimization_chart.ts [--in <json>] [--out-dir <dir>]

import { readFileSync, writeFileSync, existsSync, mkdirSync } from "fs";
import { join } from "path";
import * as echarts from "echarts";

// ── CLI ──

const args = process.argv.slice(2);
const get = (k: string): string | undefined => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : undefined;
};
const IN = get("--in") ?? join(import.meta.dir, "data", "tool-token-consumption.json");
const OUT_DIR = get("--out-dir") ?? join(import.meta.dir, "..", "reports");
const W = Number(get("--width") ?? 1100);
const H = Number(get("--height") ?? 600);

if (!existsSync(IN)) {
  console.error(`缺少源数据: ${IN}\n请先运行 bun run src/tool_token_consumption.ts`);
  process.exit(1);
}

const r = JSON.parse(readFileSync(IN, "utf8"));
const FONT = "'PingFang SC', 'Heiti SC', 'Microsoft YaHei', Arial, sans-serif";
const CLAY = "#d97757";
const INK = "#1f2937";
const GRAY = "#9ca3af";
const MUTE = "#6b7280";

// ── 数据 ──

// 场景 1:周演变(脚本计算)——出参 MB + P95(KB)
const weekRows = r.weekly;
const weekLabels = weekRows.map((w: any) => w.week.slice(5).replace("-", "/"));
const weekMB = weekRows.map((w: any) => +(w.outBytes / 1024 / 1024).toFixed(1));
const weekP95KB = weekRows.map((w: any) => +(w.p95 / 1024).toFixed(0));

// 其余:调查口径(来源 optimization-verification.md,2026-08-09)
const monthLabels = ["6 月", "7 月", "8 月(1-9 日)"];
const nfCounts = [106, 285, 0]; // 场景 4 not found(§3.3)
const pollEvents = [91, 75, 3]; // 场景 5 轮询事件(§3.4)
const pollMaxChain = [44, 32, 3]; // 场景 5 最长连续链(8 月均 ≤3,疑似 workflow 测试)
const truncCounts = [266, 4884, 1880]; // 场景 2 Output truncated(§3.2)
const savedCounts = [1706, 2953, 525]; // 场景 2 Full output saved(§3.2)

// ── 渲染 ──

function render(file: string, option: any): void {
  const chart = echarts.init(null, null, { renderer: "svg", ssr: true, width: W, height: H });
  chart.setOption({ backgroundColor: "#ffffff", ...option });
  writeFileSync(join(OUT_DIR, file), chart.renderToSVGString());
  console.log(`已输出: ${join(OUT_DIR, file)}`);
}

const titleStyle = (text: string) => ({
  text,
  left: 20,
  top: 12,
  textStyle: { fontFamily: FONT, fontSize: 17, fontWeight: "bold" as const, color: INK },
});
const axisLabel = { fontFamily: FONT };
const note = (text: string) => ({
  type: "text" as const,
  left: 22,
  bottom: 6,
  style: { text, fontFamily: FONT, fontSize: 11, fill: MUTE, textAlign: "left" as const },
});
const valueLabel = { show: true, position: "top" as const, fontFamily: FONT, color: INK, fontSize: 11 };

// 场景 1:周出参 + P95(双 Y 轴)
render("chart-1-weekly.svg", {
  title: [titleStyle("场景 1:glob 文件夹过滤限制 —— 周出参总量与单次 P95")],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  legend: { data: ["出参 MB", "P95 (KB)"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 80, right: 80, top: 90, bottom: 60 },
  xAxis: {
    type: "category",
    data: weekLabels,
    axisLabel: { ...axisLabel, interval: 0, rotate: 30 },
    name: "周(周一)",
    nameLocation: "middle",
    nameGap: 34,
    nameTextStyle: axisLabel,
  },
  yAxis: [
    { type: "value", name: "出参 MB", axisLabel, nameTextStyle: axisLabel },
    { type: "value", name: "P95 (KB)", axisLabel, nameTextStyle: axisLabel, splitLine: { show: false } },
  ],
  series: [
    { name: "出参 MB", type: "bar", data: weekMB, itemStyle: { color: CLAY }, barMaxWidth: 30, label: valueLabel },
    { name: "P95 (KB)", type: "line", yAxisIndex: 1, data: weekP95KB, symbolSize: 7, lineStyle: { width: 2.5, color: INK }, itemStyle: { color: INK } },
  ],
  graphic: [note("口径:src/data/tool-token-consumption.json weekly 字段(脚本实时计算);07-13 周为峰值,07-20 周起收敛")],
});

// 场景 2:截断 vs 落盘提示(双柱)
render("chart-2-truncation.svg", {
  title: [titleStyle("场景 2:工具大输出落盘优化 —— 截断提示与落盘提示(月度)")],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  legend: { data: ["Output truncated", "Full output saved"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 90, right: 30, top: 90, bottom: 60 },
  xAxis: { type: "category", data: monthLabels, axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "value", axisLabel, nameTextStyle: axisLabel },
  series: [
    { name: "Output truncated", type: "bar", data: truncCounts, itemStyle: { color: CLAY }, barMaxWidth: 60, label: valueLabel },
    { name: "Full output saved", type: "bar", data: savedCounts, itemStyle: { color: GRAY }, barMaxWidth: 60, label: valueLabel },
  ],
  graphic: [note("口径:threads.db 消息文本统计(调查口径,月度;8 月仅 1-9 日);截断提示 7 月 4,884 次为峰值,落盘保证内容不丢失")],
});

// 场景 4:not found 月度(柱)
render("chart-4-notfound.svg", {
  title: [titleStyle("场景 4:工具别名设计 —— not found 调用(月度)")],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  legend: { data: ["not found 次数"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 90, right: 30, top: 90, bottom: 60 },
  xAxis: { type: "category", data: monthLabels, axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "value", axisLabel, nameTextStyle: axisLabel },
  series: [
    { name: "not found 次数", type: "bar", data: nfCounts, itemStyle: { color: CLAY }, barMaxWidth: 90, label: valueLabel },
  ],
  graphic: [note("口径:threads.db 单引号格式 Tool 'X' not found(调查口径,月度;8 月仅 1-9 日);7 月峰值为工具注册缺失(07-21 集体 not found),非 LLM 幻觉;真正的幻觉工具名全窗口仅约 60 次")],
});

// 场景 5:轮询事件 + 最长链(柱 + 线)
render("chart-5-polling.svg", {
  title: [titleStyle("场景 5:Agent 轮询等待机制修复 —— 轮询事件数与最长连续链")],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  legend: { data: ["轮询事件数", "最长连续链(次)"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 90, right: 30, top: 90, bottom: 60 },
  xAxis: { type: "category", data: monthLabels, axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "value", axisLabel, nameTextStyle: axisLabel },
  series: [
    { name: "轮询事件数", type: "bar", data: pollEvents, itemStyle: { color: CLAY }, barMaxWidth: 60, label: valueLabel },
    { name: "最长连续链(次)", type: "line", data: pollMaxChain, symbolSize: 8, lineStyle: { width: 2.5, color: INK }, itemStyle: { color: INK }, label: valueLabel },
  ],
  graphic: [note("口径:同会话同参数 AgentResult/ExecuteExtraTool 连续 ≥3 次视为轮询(调查口径,月度;8 月仅 1-9 日,均 ≤3 次,疑似 workflow 测试)")],
});

mkdirSync(OUT_DIR, { recursive: true });
console.log(`完成:${OUT_DIR}`);
