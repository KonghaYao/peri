//! tool_token_charts.ts — token 消耗研究:每章独立数据图
//!
//! 读取 src/data/tool-token-consumption.json,用 ECharts SSR 渲染 5 张独立 SVG
//! (每个发现小节一张),输出到 --out-dir(默认 reports/):
//!   chart-ttc-share.svg    §3.2 工具出参占比 top8(横向条形,MB + 占出参%)
//!   chart-ttc-buckets.svg  §3.3 出参分桶:调用占比 vs 字节占比(双系列横向条形)
//!   chart-ttc-token.svg    §3.4 工具 token 排名 top10(入参/出参堆叠,百万)
//!   chart-ttc-waste.svg    §3.5 浪费构成与并集(横向条形,MB + 占出参%)
//!   chart-ttc-weekly.svg   §3.6 周演变:出参总量(MB 柱)+ 单次 P95(KB 线,双 Y 轴)
//!
//! 单位:字节一律按二进制前缀(1 MiB = 1,048,576 B),与已发布的 chart-1-weekly.svg 一致。
//!
//! 用法:
//!   bun run src/tool_token_charts.ts [--in <json>] [--out-dir <dir>]

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
const BLUE = "#4f7cac";
const GRAY = "#c7c4bb";
const MUTE = "#6b7280";
const MiB = (b: number) => +(b / 1048576).toFixed(1);
const pct = (b: number) => +((b / r.totals.outBytes) * 100).toFixed(1);

// ── 渲染 ──

function render(file: string, option: any): void {
  mkdirSync(OUT_DIR, { recursive: true });
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
const barLabel = { show: true, position: "right" as const, fontFamily: FONT, color: INK, fontSize: 11 };

// ── 数据 ──

// §3.2 出参占比 top8 + 其余
const byOut = [...r.byTool].sort((a: any, b: any) => b.outBytes - a.outBytes);
const shareRows = byOut.slice(0, 8).map((t: any) => ({ name: t.name, mb: MiB(t.outBytes), pct: pct(t.outBytes) }));
const restBytes = byOut.slice(8).reduce((s: number, t: any) => s + t.outBytes, 0);
shareRows.push({ name: "其余 30 个", mb: MiB(restBytes), pct: pct(restBytes) });

// §3.3 分桶:调用占比 vs 字节占比
const buckets = r.buckets.map((b: any) => ({
  name: b.name,
  callsPct: +((b.calls / r.totals.calls) * 100).toFixed(1),
  bytesPct: pct(b.bytes),
}));

// §3.4 token 排名 top10(入参/出参堆叠)
const tokenTop = [...r.derived.toolTokens].sort((a: any, b: any) => b.methodA - a.methodA).slice(0, 10).reverse();

// §3.5 浪费构成 + 并集
const wasteRows = [
  { name: "重读(定义 B)", mb: MiB(r.waste.rereadBytes), pct: pct(r.waste.rereadBytes) },
  { name: "白搜(K=3)", mb: MiB(r.derived.wasteTokens.whiteSearchK3_bytes.bytes), pct: pct(r.derived.wasteTokens.whiteSearchK3_bytes.bytes) },
  { name: "巨型输出 >100KB", mb: MiB(r.giantBytes), pct: pct(r.giantBytes) },
  { name: "重复搜索", mb: MiB(r.waste.dupSearchBytes), pct: pct(r.waste.dupSearchBytes) },
];
const unionMB = MiB(r.waste.unionBytes);

// §3.6 周演变
const weekRows = r.weekly;
const weekLabels = weekRows.map((w: any) => w.week.slice(5).replace("-", "/"));
const weekMB = weekRows.map((w: any) => MiB(w.outBytes));
const weekP95KB = weekRows.map((w: any) => +(w.p95 / 1024).toFixed(0));
const weekCalls = weekRows.map((w: any) => w.calls);

// ── 图 1:§3.2 出参占比 ──

render("chart-ttc-share.svg", {
  title: [titleStyle("工具出参占比(top8 + 其余)")],
  tooltip: {
    trigger: "axis", confine: true, axisPointer: { type: "shadow" },
    textStyle: { fontFamily: FONT },
    formatter: (p: any) => {
      const d = p[0];
      return `${d.name}<br/>出参 ${d.value} MB(占出参 ${d.data.pct}%)`;
    },
  },
  grid: { left: 120, right: 140, top: 70, bottom: 50 },
  xAxis: { type: "value", name: "出参 MB", axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "category", data: shareRows.map((x) => x.name), inverse: true, axisLabel: { ...axisLabel, fontSize: 13 } },
  series: [
    {
      type: "bar", data: shareRows.map((x) => ({ value: x.mb, pct: x.pct })),
      barMaxWidth: 22, itemStyle: { color: CLAY, borderRadius: [0, 4, 4, 0] },
      label: { ...barLabel, formatter: (p: any) => `${p.value}MB (${p.data.pct}%)` },
    },
  ],
  graphic: [note(`口径:主窗口 93,997 次工具调用出参合计 ${MiB(r.totals.outBytes)}MB;Read+Grep+Bash 占 86.3%`)],
});

// ── 图 2:§3.3 分桶 ──

render("chart-ttc-buckets.svg", {
  title: [titleStyle("出参分桶:调用占比 vs 字节占比")],
  tooltip: {
    trigger: "axis", confine: true, axisPointer: { type: "shadow" },
    textStyle: { fontFamily: FONT },
    formatter: (p: any) => {
      const d = p[0];
      return `${d.name}<br/>调用 ${d.data.calls} 次(占 ${d.data.callsPct}%)<br/>字节 ${d.data.bytesMB}MB(占 ${d.data.bytesPct}%)`;
    },
  },
  legend: { data: ["调用占比 %", "字节占比 %"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 110, right: 60, top: 90, bottom: 50 },
  xAxis: { type: "value", name: "占比 %", axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "category", data: buckets.map((b: any) => b.name), inverse: true, axisLabel: { ...axisLabel, fontSize: 13 } },
  series: [
    {
      name: "调用占比 %", type: "bar", barMaxWidth: 16,
      data: buckets.map((b: any) => ({ value: b.callsPct, ...b })),
      itemStyle: { color: BLUE, borderRadius: [0, 4, 4, 0] },
      label: { ...barLabel, formatter: (p: any) => `${p.value}%` },
    },
    {
      name: "字节占比 %", type: "bar", barMaxWidth: 16,
      data: buckets.map((b: any) => ({ value: b.bytesPct, ...b })),
      itemStyle: { color: CLAY, borderRadius: [0, 4, 4, 0] },
      label: { ...barLabel, formatter: (p: any) => `${p.value}%` },
    },
  ],
  graphic: [note(`口径:${r.totals.calls} 次调用按单次出参分桶;<1KB 的调用占 59.3% 却只贡献 4.3% 字节`)],
});

// ── 图 3:§3.4 token 排名(入参/出参堆叠) ──

render("chart-ttc-token.svg", {
  title: [titleStyle("工具 token 消耗排名 top10(主口径 0.341 token/B)")],
  tooltip: {
    trigger: "axis", confine: true, axisPointer: { type: "shadow" },
    textStyle: { fontFamily: FONT },
    formatter: (p: any) => {
      const d = p[0];
      return `${d.name}<br/>入参 ${d.data.inM}M + 出参 ${d.data.outM}M = ${(d.data.inM + d.data.outM).toFixed(1)}M token`;
    },
  },
  legend: { data: ["入参 token", "出参 token"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
  grid: { left: 130, right: 80, top: 90, bottom: 50 },
  xAxis: { type: "value", name: "百万 token", axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "category", data: tokenTop.map((t: any) => t.name), inverse: true, axisLabel: { ...axisLabel, fontSize: 13 } },
  series: [
    {
      name: "入参 token", type: "bar", stack: "t", barMaxWidth: 20,
      data: tokenTop.map((t: any) => ({ value: +(t.inBytes * 0.3406 / 1e6).toFixed(1), inM: +(t.inBytes * 0.3406 / 1e6).toFixed(1), outM: +(t.outBytes * 0.3406 / 1e6).toFixed(1) })),
      itemStyle: { color: GRAY },
    },
    {
      name: "出参 token", type: "bar", stack: "t", barMaxWidth: 20,
      data: tokenTop.map((t: any) => ({ value: +(t.outBytes * 0.3406 / 1e6).toFixed(1), inM: +(t.inBytes * 0.3406 / 1e6).toFixed(1), outM: +(t.outBytes * 0.3406 / 1e6).toFixed(1) })),
      itemStyle: { color: CLAY, borderRadius: [0, 4, 4, 0] },
      label: { ...barLabel, formatter: (p: any) => `${(p.data.inM + p.data.outM).toFixed(1)}M` },
    },
  ],
  graphic: [note(`口径:入参+出参 × 0.3406(方法 A 全局比率);Read/Grep/Bash 合计约 8,660 万 token,占 75.6%`)],
});

// ── 图 4:§3.5 浪费构成 ──

render("chart-ttc-waste.svg", {
  title: [titleStyle("浪费构成(四类 + 并集)")],
  tooltip: {
    trigger: "axis", confine: true, axisPointer: { type: "shadow" },
    textStyle: { fontFamily: FONT },
    formatter: (p: any) => `${p[0].name}<br/>${p[0].value}MB(占出参 ${p[0].data.pct}%)`,
  },
  grid: { left: 150, right: 140, top: 70, bottom: 50 },
  xAxis: { type: "value", name: "出参 MB", axisLabel, nameTextStyle: axisLabel },
  yAxis: { type: "category", data: [...wasteRows.map((x) => x.name), "四者并集"], inverse: true, axisLabel: { ...axisLabel, fontSize: 13 } },
  series: [
    {
      type: "bar", barMaxWidth: 22,
      data: [
        ...wasteRows.map((x) => ({ value: x.mb, pct: x.pct })),
        { value: unionMB, pct: pct(r.waste.unionBytes) },
      ],
      itemStyle: {
        color: (p: any) => (p.dataIndex === wasteRows.length ? INK : CLAY),
        borderRadius: [0, 4, 4, 0],
      },
      label: { ...barLabel, formatter: (p: any) => `${p.value}MB (${p.data.pct}%)` },
    },
  ],
  graphic: [note(`口径:并集已去重(一次调用同时命中多项只计一次);白搜 K=3 敏感性见正文`)],
});

// ── 图 5:§3.6 周演变 ──

render("chart-ttc-weekly.svg", {
  title: [titleStyle("周演变:工具出参总量与单次 P95")],
  tooltip: { trigger: "axis", confine: true, textStyle: { fontFamily: FONT } },
  legend: { data: ["出参 MB", "P95 (KB)", "调用数"], left: 20, top: 44, textStyle: { fontFamily: FONT } },
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
    { name: "出参 MB", type: "bar", data: weekMB, itemStyle: { color: CLAY }, barMaxWidth: 30, label: { show: true, position: "top", fontFamily: FONT, color: INK, fontSize: 10 } },
    { name: "P95 (KB)", type: "line", yAxisIndex: 1, data: weekP95KB, symbolSize: 7, lineStyle: { width: 2.5, color: INK }, itemStyle: { color: INK } },
    { name: "调用数", type: "line", data: weekCalls, symbolSize: 5, lineStyle: { width: 1.5, color: BLUE, type: "dashed" }, itemStyle: { color: BLUE }, label: { show: false } },
  ],
  graphic: [note(`口径:按线程创建时间归周(周边界近似);07-13 周见顶 52.1MB,08-03 周 16.1MB`)],
});
