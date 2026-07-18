/**
 * HTML 报告生成器
 *
 * 读取 recordings/index.jsonl，生成单文件 HTML 报告。
 * 使用 unpkg 的 ansi_up 库在前端渲染 ANSI 颜色。
 *
 * 用法：
 *   tsx scripts/generate-report.ts           # 手动生成
 *   tsx scripts/generate-report.ts --watch   # 监听 index.jsonl 变化自动刷新
 *
 * 也作为 vitest globalTeardown 自动调用。
 */
import path from "node:path";
import fs from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { loadAllRecords, type SnapshotRecord } from "../helpers/recorder.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const RECORDINGS_DIR = path.resolve(__dirname, "..", "recordings");
const REPORT_PATH = path.resolve(__dirname, "..", "report.html");

async function generate(): Promise<void> {
  const records = await loadAllRecords();

  if (records.length === 0) {
    console.log("没有录制数据，跳过报告生成");
    return;
  }

  const html = buildHtml(records);
  await fs.writeFile(REPORT_PATH, html, "utf-8");
  console.log(`✓ 报告已生成: ${REPORT_PATH} (${records.length} 条记录)`);
}

function buildHtml(records: SnapshotRecord[]): string {
  // 按测试分组
  const groups = new Map<string, SnapshotRecord[]>();
  for (const r of records) {
    const list = groups.get(r.test) || [];
    list.push(r);
    groups.set(r.test, list);
  }

  // 汇总统计
  const total = records.length;
  const withJudge = records.filter((r) => r.verdict);
  const passed = withJudge.filter((r) => r.verdict === "pass");
  const failed = withJudge.filter((r) => r.verdict === "fail");

  // 生成 HTML
  const recordsJson = JSON.stringify(records);

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Peri E2E 测试报告</title>
<script src="https://unpkg.com/ansi_up@6.0.2/ansi_up.js"></script>
<style>
  :root {
    --bg: #1a1a2e;
    --card-bg: #16213e;
    --text: #e0e0e0;
    --muted: #888;
    --pass: #4caf50;
    --fail: #f44336;
    --accent: #7c83ff;
    --border: #2a2a4a;
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: var(--bg); color: var(--text); padding: 20px; }
  h1 { font-size: 1.5em; margin-bottom: 4px; }
  .summary { display: flex; gap: 20px; margin: 16px 0; flex-wrap: wrap; }
  .stat { background: var(--card-bg); border-radius: 8px; padding: 12px 20px; border: 1px solid var(--border); }
  .stat-val { font-size: 1.8em; font-weight: bold; }
  .stat-label { font-size: 0.8em; color: var(--muted); }
  .pass-color { color: var(--pass); }
  .fail-color { color: var(--fail); }
  .test-group { margin-top: 24px; }
  .test-group h2 { font-size: 1.1em; padding: 8px 0; border-bottom: 1px solid var(--border); margin-bottom: 12px; }
  .snapshot-card { background: var(--card-bg); border-radius: 8px; border: 1px solid var(--border); margin-bottom: 12px; overflow: hidden; }
  .snapshot-card.pass { border-left: 3px solid var(--pass); }
  .snapshot-card.fail { border-left: 3px solid var(--fail); }
  .card-header { display: flex; justify-content: space-between; align-items: center; padding: 10px 16px; background: rgba(255,255,255,0.03); cursor: pointer; user-select: none; }
  .card-header:hover { background: rgba(255,255,255,0.06); }
  .card-title { font-weight: 600; font-size: 0.95em; }
  .card-meta { font-size: 0.75em; color: var(--muted); }
  .card-body { display: none; padding: 16px; }
  .card-body.open { display: block; }
  .ansi-screen { background: #0d0d1a; color: #e0e0e0; padding: 12px; border-radius: 4px; font-family: "JetBrains Mono", "Fira Code", "Cascadia Code", monospace; font-size: 0.78em; line-height: 1.35; overflow-x: auto; white-space: pre; max-height: 500px; overflow-y: auto; }
  .checks { margin-top: 12px; }
  .check-item { display: flex; align-items: flex-start; gap: 8px; padding: 6px 0; border-bottom: 1px solid rgba(255,255,255,0.04); }
  .check-icon { font-size: 0.9em; width: 20px; flex-shrink: 0; }
  .check-icon.pass { color: var(--pass); }
  .check-icon.fail { color: var(--fail); }
  .check-criterion { flex: 1; font-size: 0.88em; }
  .check-detail { color: var(--muted); font-size: 0.8em; margin-top: 2px; }
  .no-judge { color: var(--muted); font-style: italic; font-size: 0.85em; padding: 8px 0; }
  .text-preview { color: var(--muted); font-size: 0.78em; font-family: monospace; margin-top: 8px; padding: 8px; background: rgba(255,255,255,0.02); border-radius: 4px; max-height: 80px; overflow: hidden; }
  .empty-state { text-align: center; padding: 60px 20px; color: var(--muted); }
  .timestamp { margin-top: 24px; font-size: 0.75em; color: var(--muted); text-align: center; }
</style>
</head>
<body>
<h1>🧪 Peri E2E 测试报告</h1>
<div class="summary">
  <div class="stat"><div class="stat-val">${total}</div><div class="stat-label">总 snapshot</div></div>
  <div class="stat"><div class="stat-val pass-color">${passed.length}</div><div class="stat-label">通过</div></div>
  <div class="stat"><div class="stat-val fail-color">${failed.length}</div><div class="stat-label">失败</div></div>
  <div class="stat"><div class="stat-val">${groups.size}</div><div class="stat-label">测试用例</div></div>
</div>

<div id="groups"></div>
<div class="timestamp">生成时间: ${new Date().toLocaleString("zh-CN")}</div>

<script>
const records = ${recordsJson};
const ansi_up = new AnsiUp();

// 按测试分组
const groups = {};
for (const r of records) {
  if (!groups[r.test]) groups[r.test] = [];
  groups[r.test].push(r);
}

const container = document.getElementById("groups");

for (const [testName, items] of Object.entries(groups)) {
  const group = document.createElement("div");
  group.className = "test-group";
  group.innerHTML = '<h2>' + esc(testName) + '</h2>';

  for (const item of items) {
    const verdict = item.verdict || "";
    const card = document.createElement("div");
    card.className = "snapshot-card " + verdict;

    // 头部
    const header = document.createElement("div");
    header.className = "card-header";
    header.onclick = function() {
      const body = this.nextElementSibling;
      body.classList.toggle("open");
    };

    const formatTime = (ts) => {
      try { return new Date(ts).toLocaleTimeString("zh-CN"); } catch { return ts; }
    };

    const durText = item.duration_ms != null ? item.duration_ms + "ms" : "";
    const sizeText = item.size ? item.size.cols + "×" + item.size.rows : "";

    header.innerHTML = '<span class="card-title">' +
      (verdict === "pass" ? "✅ " : verdict === "fail" ? "❌ " : "📸 ") +
      'Step ' + item.step + ': ' + esc(item.name) +
      '</span>' +
      '<span class="card-meta">' +
      formatTime(item.timestamp) +
      (durText ? " · " + durText : "") +
      (sizeText ? " · " + sizeText : "") +
      '</span>';

    // 内容
    const body = document.createElement("div");
    body.className = "card-body";

    let bodyHtml = "";

    // 文本预览
    if (item.textPreview) {
      bodyHtml += '<div class="text-preview">' + esc(item.textPreview) + '</div>';
    }

    // judge checks
    if (item.checks && item.checks.length > 0) {
      bodyHtml += '<div class="checks">';
      for (const c of item.checks) {
        bodyHtml += '<div class="check-item">' +
          '<span class="check-icon ' + (c.pass ? "pass" : "fail") + '">' + (c.pass ? "✓" : "✗") + '</span>' +
          '<div><div class="check-criterion">' + esc(c.criterion) + '</div>' +
          (c.detail ? '<div class="check-detail">' + esc(c.detail) + '</div>' : "") +
          '</div></div>';
      }
      bodyHtml += '</div>';
    } else {
      bodyHtml += '<div class="no-judge">未执行 LLM judge</div>';
    }

    body.innerHTML = bodyHtml;
    card.appendChild(header);
    card.appendChild(body);
    group.appendChild(card);
  }

  container.appendChild(group);
}

function esc(s) {
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;");
}
</script>
</body>
</html>`;
}

// ---- 入口 ----

const isWatch = process.argv.includes("--watch");

async function main() {
  if (isWatch) {
    console.log("👀 监听 index.jsonl 变化...");
    const { watch } = await import("node:fs");
    const indexPath = path.join(RECORDINGS_DIR, "index.jsonl");

    // 先跑一次
    await generate();

    // 监听文件变化
    try {
      watch(indexPath, async () => {
        await generate();
      });
    } catch {
      console.warn("⚠ 文件监听不可用（可能在 vitest teardown 中运行）");
      await generate();
    }
  } else {
    await generate();
  }
}

// 直接运行脚本时，执行 main()
const isDirectRun = process.argv[1] && import.meta.url.endsWith(process.argv[1].replace(/^\.\//, ""));
if (isDirectRun || process.argv[1]?.includes("generate-report")) {
  main().catch(console.error);
}

// 作为 vitest globalTeardown 调用时的默认导出
export default generate;
