//! tool_token_chart.ts — 工具调用 token 消耗研究:数据可视化
//!
//! 读取 src/data/tool-token-consumption.json,用 ECharts SSR 模式渲染 SVG,
//! 输出到 reports/(与报告文档同目录,便于 md 同级引用)。2x2 组合图:
//!   左上:工具出参占比(top8 横向条形)
//!   右上:周演变(出参 MB 柱 + P95 单次出参折线,双 Y 轴)
//!   左下:浪费构成(白搜/重读/重复搜索/巨型输出,MB 与占出参%)
//!   右下:工具 token 消耗排名 top10(估算百万 token,methodA 口径)
//!
//! 用法:
//!   bun run src/tool_token_chart.ts [--in <json>] [--out <png>] [--size 1600x1200]
//!
//! 依赖: echarts(SSR SVG 渲染)、sharp(SVG→PNG)

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
const OUT = get("--out") ?? join(import.meta.dir, "..", "reports", "tool-token-consumption.svg");
const [W, H] = (get("--size") ?? "1600x1200").split("x").map(Number);

if (!existsSync(IN)) {
  console.error(`缺少源数据: ${IN}\n请先运行 bun run src/tool_token_consumption.ts`);
  process.exit(1);
}

// ── 数据 ──

const r = JSON.parse(readFileSync(IN, "utf8"));
const FONT = "'PingFang SC', 'Heiti SC', 'Microsoft YaHei', Arial, sans-serif";

// 工具出参占比 top8 + 其他
const byOut = [...r.byTool].sort((a, b) => b.outBytes - a.outBytes);
const top8 = byOut.slice(0, 8);
const otherBytes = byOut.slice(8).reduce((s: number, t: any) => s + t.outBytes, 0);
const toolShareRows = [...top8, { name: "其余", outBytes: otherBytes }];
const toolShareNames = toolShareRows.map((t: any) => t.name);
const toolShareVals = toolShareRows.map((t: any) => +(t.outBytes / 1024 / 1024).toFixed(1));

// 周演变: 出参 MB + P95(KB)
const weekRows = r.weekly;
const weekLabels = weekRows.map((w: any) => w.week.slice(5).replace("-", "/"));
const weekMB = weekRows.map((w: any) => +(w.outBytes / 1024 / 1024).toFixed(1));
const weekP95KB = weekRows.map((w: any) => +(w.p95 / 1024).toFixed(0));
const weekSessions = weekRows.map((w: any) => w.sessions);

// 浪费构成
const wasteItems = [
  { name: "重读(定义 B)", mb: +(r.waste.rereadBytes / 1024 / 1024).toFixed(1), pct: +((r.waste.rereadBytes / r.totals.outBytes) * 100).toFixed(1) },
  { name: "白搜(K=3)", mb: +(r.derived.wasteTokens.whiteSearchK3_bytes.bytes / 1024 / 1024).toFixed(1), pct: +(((r.derived.wasteTokens.whiteSearchK3_bytes.bytes) / r.totals.outBytes) * 100).toFixed(1) },
  { name: "巨型输出 >100KB", mb: +(r.waste.giantBytes / 1024 / 1024).toFixed(1), pct: +((r.waste.giantBytes / r.totals.outBytes) * 100).toFixed(1) },
  { name: "重复搜索", mb: +(r.waste.dupSearchBytes / 1024 / 1024).toFixed(1), pct: +((r.waste.dupSearchBytes / r.totals.outBytes) * 100).toFixed(1) },
];
const unionMB = +(r.waste.unionBytes / 1024 / 1024).toFixed(1);

// token 排名 top10(methodA 口径, 百万)
const tokenTop = [...r.derived.toolTokens].sort((a, b) => b.methodA - a.methodA).slice(0, 10).reverse();
const tokenNames = tokenTop.map((t: any) => t.name);
const tokenM = tokenTop.map((t: any) => +(t.methodA / 1e6).toFixed(1));

// ── ECharts SSR: 2x2 grid ──

const chart = echarts.init(null, null, { renderer: "svg", ssr: true, width: W, height: H });

const titleStyle = { textStyle: { fontFamily: FONT, fontSize: 16, fontWeight: "bold" as const, color: "#1f2937" }, left: 20, top: 12 };

chart.setOption({
  backgroundColor: "#ffffff",
  title: [
    { ...titleStyle, text: "工具出参占比(top8)" },
    { ...titleStyle, text: "周演变:出参总量与单次 P95", left: "50%" },
    { ...titleStyle, text: "浪费构成(并集 70.3MB,占出参 26.2%)", top: "50%" },
    { ...titleStyle, text: "工具 token 消耗排名 top10(估算,百万)", left: "50%", top: "50%" },
  ],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  grid: [
    { left: 90, right: "51%", top: 60, bottom: "53%" },
    { left: "51%", right: 20, top: 60, bottom: "53%" },
    { left: 90, right: "51%", top: "53%", bottom: 30 },
    { left: "51%", right: 20, top: "53%", bottom: 30 },
  ],
  xAxis: [
    { type: "value", gridIndex: 0, name: "MB", axisLabel: { fontFamily: FONT }, nameTextStyle: { fontFamily: FONT } },
    { type: "category", gridIndex: 1, data: weekLabels, axisLabel: { fontFamily: FONT, interval: 0, rotate: 30 }, name: "周(周一)", nameLocation: "middle", nameGap: 30, nameTextStyle: { fontFamily: FONT } },
    { type: "value", gridIndex: 2, name: "MB", axisLabel: { fontFamily: FONT }, nameTextStyle: { fontFamily: FONT } },
    { type: "value", gridIndex: 3, name: "百万 token", axisLabel: { fontFamily: FONT }, nameTextStyle: { fontFamily: FONT } },
  ],
  yAxis: [
    { type: "category", gridIndex: 0, data: toolShareNames, inverse: true, axisLabel: { fontFamily: FONT, fontSize: 13 } },
    { type: "value", gridIndex: 1, name: "MB/周", axisLabel: { fontFamily: FONT }, nameTextStyle: { fontFamily: FONT } },
    { type: "value", gridIndex: 1, name: "P95(KB)", axisLabel: { fontFamily: FONT }, nameTextStyle: { fontFamily: FONT } },
    { type: "category", gridIndex: 2, data: wasteItems.map((w) => w.name), inverse: true, axisLabel: { fontFamily: FONT } },
    { type: "category", gridIndex: 3, data: tokenNames, inverse: true, axisLabel: { fontFamily: FONT, fontSize: 12 } },
  ],
  series: [
    // 1: 工具出参占比
    {
      type: "bar", xAxisIndex: 0, yAxisIndex: 0, data: toolShareVals,
      itemStyle: { color: "#2563eb", borderRadius: [0, 4, 4, 0] },
      label: { show: true, position: "right", formatter: (p: any) => `${p.value}MB (${((p.value / (r.totals.outBytes / 1024 / 1024)) * 100).toFixed(1)}%)`, fontFamily: FONT, fontSize: 11, color: "#374151" },
      barWidth: 20,
    },
    // 2: 周演变 — 出参柱 + P95 折线(双轴)
    {
      type: "bar", xAxisIndex: 1, yAxisIndex: 1, data: weekMB,
      itemStyle: { color: "#2563eb", borderRadius: [3, 3, 0, 0] },
      label: { show: true, position: "top", formatter: (p: any) => p.value, fontFamily: FONT, fontSize: 10, color: "#1f2937" },
      barWidth: 18,
    },
    {
      type: "line", xAxisIndex: 1, yAxisIndex: 2, data: weekP95KB, symbol: "circle", symbolSize: 6,
      lineStyle: { color: "#dc2626", width: 2.5 },
      itemStyle: { color: "#dc2626" },
      label: { show: true, position: "top", formatter: (p: any) => `${p.value}`, fontFamily: FONT, fontSize: 10, color: "#dc2626" },
    },
    // 3: 浪费构成
    {
      type: "bar", xAxisIndex: 2, yAxisIndex: 3, data: wasteItems.map((w) => w.mb),
      itemStyle: { color: "#f59e0b", borderRadius: [0, 4, 4, 0] },
      label: { show: true, position: "right", formatter: (p: any) => `${p.value}MB (${wasteItems[p.dataIndex].pct}%)`, fontFamily: FONT, fontSize: 11, color: "#374151" },
      barWidth: 24,
    },
    // 4: token 排名
    {
      type: "bar", xAxisIndex: 3, yAxisIndex: 4, data: tokenM,
      itemStyle: { color: "#059669", borderRadius: [0, 4, 4, 0] },
      label: { show: true, position: "right", formatter: (p: any) => `${p.value}M`, fontFamily: FONT, fontSize: 11, color: "#374151" },
      barWidth: 16,
    },
  ],
});

const svg = chart.renderToSVGString();
chart.dispose();

// ── 输出 SVG(矢量,文本可搜索,渲染端用本地字体,无字体缺失风险)──

mkdirSync(join(import.meta.dir, "..", "reports"), { recursive: true });
writeFileSync(OUT, svg);

console.log(`SVG 已生成: ${OUT} (${(svg.length / 1024).toFixed(0)}KB, ${W}x${H})`);
