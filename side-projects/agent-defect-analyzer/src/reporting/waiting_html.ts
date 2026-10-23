/** Self-contained HTML projection of the shell waiting analysis. No external assets. */
import type { WaitingDto } from "./waiting.js";

function esc(value: unknown): string {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

function pct(value: number | null): string {
  return value === null ? "—" : `${(value * 100).toFixed(2)}%`;
}

function num(value: number): string {
  return new Intl.NumberFormat("en-US").format(value);
}

function hours(seconds: number): string {
  return `${(seconds / 3600).toFixed(1)} h`;
}

function bar(value: number | null, max: number): string {
  const width = value === null || max <= 0 ? 0 : Math.max(1, (value / max) * 100);
  return `<span class="bar" style="width:${width.toFixed(1)}%"></span>`;
}

function table(headers: string[], rows: string[][], numericFrom = 1): string {
  const head = headers.map((header, index) => `<th class="${index >= numericFrom ? "num" : ""}">${esc(header)}</th>`).join("");
  const body = rows.map((row) => `<tr>${row.map((cell, index) => `<td class="${index >= numericFrom ? "num" : ""}">${cell}</td>`).join("")}</tr>`).join("");
  return `<table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>`;
}

function observations(dto: WaitingDto): string[] {
  const months = Object.entries(dto.sleep.by_month);
  const weeks = Object.entries(dto.sleep.by_week);
  const latest = months.at(-1);
  const previous = months.slice(0, -1);
  const previousCommands = previous.reduce((sum, [, value]) => sum + value.command_calls, 0);
  const previousSleep = previous.reduce((sum, [, value]) => sum + value.sleep_calls, 0);
  const baseline = previousCommands === 0 ? null : previousSleep / previousCommands;
  const lines: string[] = [];
  if (latest && baseline !== null && latest[1].sleep_share !== null) {
    const delta = (latest[1].sleep_share - baseline) * 100;
    lines.push(`最新月 <strong>${esc(latest[0])}</strong> 的 sleep 占比为 <strong>${pct(latest[1].sleep_share)}</strong>，此前各月合计为 <strong>${pct(baseline)}</strong>，相差 <strong>${delta >= 0 ? "+" : ""}${delta.toFixed(2)} 个百分点</strong>。`);
    lines.push(`最新月命令量为 <strong>${num(latest[1].command_calls)}</strong>，此前月均为 <strong>${num(Math.round(previousCommands / Math.max(1, previous.length)))}</strong>；占比与任务量同时变化，不能作因果解释。`);
  }
  const peak = weeks
    .filter(([, value]) => value.command_calls >= 50 && value.sleep_share !== null)
    .sort((left, right) => (right[1].sleep_share ?? 0) - (left[1].sleep_share ?? 0))[0];
  if (peak) {
    lines.push(`周峰值出现在 <strong>${esc(peak[0])}</strong> 起始的一周：<strong>${pct(peak[1].sleep_share)}</strong>（${num(peak[1].sleep_calls)} / ${num(peak[1].command_calls)}），仅筛命令量 ≥ 50 的周，避免小样本放大。`);
  }
  const noInfo = (dto.sleep.by_wait_kind.pure ?? 0) + (dto.sleep.by_wait_kind.noop_only ?? 0);
  lines.push(`无信息等待（纯 sleep 或仅 echo/true 等）为 <strong>${num(noInfo)}</strong> 次，占 sleep 调用的 <strong>${pct(dto.sleep.calls === 0 ? null : noInfo / dto.sleep.calls)}</strong>。`);
  return lines;
}

export function renderWaitingHtml(dto: WaitingDto): string {
  const sleep = dto.sleep;
  const months = Object.entries(sleep.by_month);
  const weeks = Object.entries(sleep.by_week);
  const maxMonthShare = Math.max(...months.map(([, value]) => value.sleep_share ?? 0), 0.0001);
  const maxWeekShare = Math.max(...weeks.map(([, value]) => value.sleep_share ?? 0), 0.0001);
  const noInfo = (sleep.by_wait_kind.pure ?? 0) + (sleep.by_wait_kind.noop_only ?? 0);
  const topTools = dto.tools.filter((tool) => tool.command_calls > 0).slice(0, 10);
  const topThreads = sleep.top_threads.slice(0, 12);

  const monthRows = months.map(([month, value]) => [
    esc(month),
    num(value.command_calls),
    num(value.sleep_calls),
    `<div class="bar-row">${bar(value.sleep_share, maxMonthShare)}<span>${pct(value.sleep_share)}</span></div>`,
  ]);
  const weekRows = weeks.map(([week, value]) => [
    esc(week),
    num(value.command_calls),
    num(value.sleep_calls),
    `<div class="bar-row">${bar(value.sleep_share, maxWeekShare)}<span>${pct(value.sleep_share)}</span></div>`,
  ]);
  const shapeRows = Object.entries(sleep.by_shape).map(([key, value]) => [esc(key), num(value)]);
  const waitRows = Object.entries(sleep.by_wait_kind).map(([key, value]) => [esc(key), num(value)]);
  const bucketRows = Object.entries(sleep.by_duration_bucket).map(([key, value]) => [esc(key), num(value)]);
  const statusRows = Object.entries(sleep.by_execution_status).map(([key, value]) => [esc(key), num(value)]);
  const toolRows = topTools.map((tool) => [esc(tool.name), num(tool.calls), num(tool.command_calls), num(tool.sleep_calls), pct(tool.sleep_share)]);
  const threadRows = topThreads.map((thread) => [esc(thread.thread_id), esc(thread.thread_kind), esc(thread.created_month), num(thread.command_calls), num(thread.sleep_calls), pct(thread.sleep_share), num(thread.sleep_seconds)]);
  const histogramRows = Object.entries(sleep.thread_sleep_histogram).map(([key, value]) => [esc(key), num(value)]);
  const observationItems = observations(dto).map((line) => `<li>${line}</li>`).join("");
  const parseIssues = Object.entries(dto.parse_issues).slice(0, 8).map(([key, value]) => `${esc(key)}: ${num(value)}`).join("; ") || "none";

  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>睡眠 shell 等待分析</title>
<style>
:root{--bg:#f5f3ee;--panel:#fffdf8;--ink:#22201c;--muted:#6b665d;--line:#d8d2c6;--accent:#b04a2f;--accent2:#2f6b5a;--warn:#8a5a12}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Hiragino Sans GB",sans-serif}
main{max-width:1180px;margin:0 auto;padding:40px 22px 64px}h1{font-size:30px;line-height:1.2;margin:0 0 8px;letter-spacing:-.02em}
h2{font-size:20px;margin:38px 0 12px;padding-bottom:7px;border-bottom:1px solid var(--line)}h3{font-size:16px;margin:20px 0 8px}
.sub{color:var(--muted);margin:0 0 20px}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:12px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:15px}.card .k{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.06em}
.card .v{font-size:27px;font-weight:700;margin:3px 0;font-variant-numeric:tabular-nums}.card .n{color:var(--muted);font-size:12px}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:18px;margin:12px 0}table{width:100%;border-collapse:collapse;font-size:13.5px}
th,td{text-align:left;padding:7px 9px;border-bottom:1px solid var(--line);vertical-align:middle}th{color:var(--muted);font-weight:600;font-size:12px}
.num{text-align:right;font-variant-numeric:tabular-nums}.bar-row{display:flex;align-items:center;gap:8px;min-width:150px}
.bar{display:inline-block;height:9px;background:var(--accent);border-radius:3px;min-width:2px}.bar-row span{font-size:12px;color:var(--muted);white-space:nowrap}
.note{border-left:3px solid var(--warn);padding:10px 14px;background:#fbf4e4;border-radius:0 8px 8px 0;color:#4b3712}
.ok{border-left-color:var(--accent2);background:#eaf2ee;color:#173f34}code{background:#ece8df;padding:1px 5px;border-radius:4px;font-size:12px}
ul{margin:8px 0 0 20px;padding:0}li{margin:5px 0}.muted{color:var(--muted)}.small{font-size:12px}
.split{display:grid;grid-template-columns:1fr 1fr;gap:18px}@media(max-width:760px){.split{grid-template-columns:1fr}}
footer{margin-top:36px;color:var(--muted);font-size:12px;border-top:1px solid var(--line);padding-top:14px}
</style>
</head>
<body><main>
<h1>睡眠 shell 等待分析</h1>
<p class="sub">范围：${esc(dto.filters.scope)}${dto.filters.include_hidden ? " + hidden" : ""}；时间：${esc(dto.filters.since ?? "最早可用")} → ${esc(dto.filters.until ?? "最新快照")}；按会话创建时间分组。<br>
生成日期：${new Date().toISOString().slice(0, 10)}；数据指纹：<code>${esc(dto.source.fingerprint.slice(0, 20))}…</code>；schema：${esc(dto.schema_version)}</p>

<div class="grid">
<div class="card"><div class="k">sleep 调用占比</div><div class="v">${pct(sleep.share_of_command_calls)}</div><div class="n">${num(sleep.calls)} / ${num(dto.totals.command_calls)} 次命令调用</div></div>
<div class="card"><div class="k">会话覆盖率</div><div class="v">${pct(sleep.thread_share)}</div><div class="n">${num(sleep.threads_with_sleep)} / ${num(dto.totals.command_threads)} 个命令会话</div></div>
<div class="card"><div class="k">无信息等待</div><div class="v">${pct(sleep.calls === 0 ? null : noInfo / sleep.calls)}</div><div class="n">${num(noInfo)} 次，纯 sleep 或仅 echo/true</div></div>
<div class="card"><div class="k">字面等待总量</div><div class="v">${hours(sleep.total_sleep_seconds)}</div><div class="n">单次 p50 ${sleep.call_sleep_seconds.p50 ?? "—"} s；p90 ${sleep.call_sleep_seconds.p90 ?? "—"} s</div></div>
</div>

<h2>按时间讨论</h2>
<div class="panel"><h3>月度</h3>
${table(["月份", "命令调用", "sleep 调用", "占比"], monthRows)}
<h3>周度（UTC 周一起始）</h3>
${table(["周起始", "命令调用", "sleep 调用", "占比"], weekRows)}
</div>

<div class="panel"><h3>从数据能说什么</h3><ul>${observationItems}</ul>
<p class="note"><strong>版本归因限制：</strong>会话表与消息表都没有持久化应用版本号；本报告只能按会话创建时间分组。Peri 迭代很快：仓库标签显示 2026-06 至 09 每月约 20–30 个 <code>agent-v*</code> 标签，2026-10-11 至 10-22 又出现 20+ 个 4.x 标签。一个长会话可能跨多个版本，因此月度/周度差异不能归因到某个具体版本。若要做版本级比较，需要先把版本号写入会话元数据，或把发布标签当作外部时间带并明确跨版本的测量误差。</p>
</div>

<h2>等待形态</h2>
<div class="split">
<div class="panel"><h3>命令形态</h3>${table(["形态", "次数"], shapeRows)}<p class="small muted">compound = sleep 与其他命令串联；loop = 循环内 sleep；standalone = 单独 sleep；variable = 秒数无法解析。</p></div>
<div class="panel"><h3>等待类型</h3>${table(["类型", "次数"], waitRows)}<p class="small muted">pure = 只有 sleep；noop_only = 仅 echo/printf/true/false/:/date；other_command = 至少一个其他命令。</p></div>
</div>

<h2>时长与执行证据</h2>
<div class="split">
<div class="panel"><h3>时长分桶</h3>${table(["区间", "次数"], bucketRows)}<p class="small muted">秒数为字面值合计；循环次数、并行、取消和实际墙钟未折算。</p></div>
<div class="panel"><h3>执行状态</h3>${table(["状态", "次数"], statusRows)}
<p class="small muted">is_error：true ${num(sleep.by_is_error.true ?? 0)}，false ${num(sleep.by_is_error.false ?? 0)}，unknown ${num(sleep.by_is_error.unknown ?? 0)}。后台 sleep ${num(sleep.background_sleep_calls)} 次；sleep 超过 timeout ${num(sleep.sleep_exceeding_timeout_calls)} 次（已排除 timeout=0）。</p></div>
</div>

<h2>工具与长尾</h2>
<div class="panel"><h3>命令工具</h3>${table(["工具", "调用", "命令调用", "sleep", "占比"], toolRows)}
<h3>sleep 次数分布</h3>${table(["每个会话的 sleep 次数", "会话数"], histogramRows)}
</div>
<div class="panel"><h3>Top 会话</h3>${table(["thread_id", "类型", "创建月份", "命令调用", "sleep 调用", "占比", "sleep 秒"], threadRows)}</div>

<h2>方法与复现</h2>
<div class="panel">
<ul>
<li>主计数只认简单命令段的有效首 token 为 <code>sleep</code>；<code>bash -c 'sleep 5'</code>、<code>xargs</code>、<code>sudo</code> 等包装形态单列为敏感性计数（本报告 ${num(dto.sensitivity.wrapper_embedded_sleep_calls)} 次），不混入主分母。</li>
<li>分母是顶层参数带 <code>command</code>/<code>cmd</code>/<code>script</code>/<code>shell_command</code> 的工具调用；<code>ExecuteExtraTool</code> 嵌套 shell 未解包。</li>
<li>执行状态只读 typed metadata；缺失记 unknown/missing。sleep 成功只表示命令退出，不表示等待目标达成。</li>
<li>复现：<code>bun run waiting --db ~/.peri/threads/threads.db --scope all --include-hidden --out output/sleep-wait-study/v3</code></li>
<li>解析问题：${esc(parseIssues)}；重复调用 ${num(dto.totals.duplicate_call_ids)}；缺失 sleep 结果 ${num(dto.totals.missing_sleep_results)}。</li>
</ul>
<p class="note ok"><strong>一句话：</strong>sleep 等待不是主导行为，但在约五分之一的命令会话中出现；多数是“等完再检查”，真正值得优化的是无信息等待和超时参数不一致。最新月的上升与任务量和版本迭代同时发生，不能作因果解释。</p>
</div>
<footer>Peri 会话离线分析 · 只读快照 · 报告不含原始命令、参数和路径 · 数字以 JSON 为准</footer>
</main></body></html>`;
}
