/**
 * HTML 报告生成器
 *
 * 读取 recordings/index.jsonl + *.ansi 文件，生成单文件 HTML 报告。
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

// 是否由 vitest globalTeardown 调用（此时不可 watch）
const isVitestTeardown = !!process.env.VITEST_POOL_ID || process.argv.some((a) => a.includes("vitest"));

interface ReportRecord extends SnapshotRecord {
  /** 全量 ANSI 原始文本（嵌入 HTML 供前端渲染） */
  ansiRaw?: string;
}

interface ReportGroup {
  name: string;
  records: ReportRecord[];
}

async function generate(): Promise<void> {
  const baseRecords = await loadAllRecords();

  if (baseRecords.length === 0) {
    console.log("没有录制数据，跳过报告生成");
    return;
  }

  // 读取每个 snapshot 的原始文本，嵌入 report
  // 优先读 snapshotFile 指向的文件，若不存在则尝试另一扩展名
  const records: ReportRecord[] = await Promise.all(
    baseRecords.map(async (r) => {
      let ansiRaw: string | undefined;
      const primaryPath = path.join(RECORDINGS_DIR, r.snapshotFile);
      try {
        ansiRaw = await fs.readFile(primaryPath, "utf-8");
      } catch {
        // 回退：尝试另一扩展名（.txt ↔ .ansi）
        const altExt = r.snapshotFile.endsWith(".txt") ? ".ansi" : ".txt";
        const altPath = path.join(
          RECORDINGS_DIR,
          r.snapshotFile.replace(/\.(txt|ansi)$/, altExt),
        );
        try {
          ansiRaw = await fs.readFile(altPath, "utf-8");
        } catch {
          // 两个扩展名都不存在
        }
      }
      return { ...r, ansiRaw };
    }),
  );

  // 服务端预分组
  const groups: ReportGroup[] = [];
  const groupMap = new Map<string, SnapshotRecord[]>();
  for (const r of records) {
    const list = groupMap.get(r.test) || [];
    list.push(r);
    groupMap.set(r.test, list);
  }
  for (const [name, items] of groupMap) {
    groups.push({ name, records: items });
  }

  // 汇总统计
  const total = records.length;
  const withJudge = records.filter((r) => r.verdict);
  const passed = withJudge.filter((r) => r.verdict === "pass");
  const failed = withJudge.filter((r) => r.verdict === "fail");

  const html = buildHtml({ groups, total, passed: passed.length, failed: failed.length });
  await fs.writeFile(REPORT_PATH, html, "utf-8");
  console.log(`✓ 报告已生成: ${REPORT_PATH} (${total} 条记录, ${passed.length} 通过, ${failed.length} 失败)`);
}

function buildHtml(data: {
  groups: ReportGroup[];
  total: number;
  passed: number;
  failed: number;
}): string {
  const groupsJson = JSON.stringify(data.groups);
  const groupCount = data.groups.length;

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Peri E2E 测试报告</title>
<script src="https://unpkg.com/ansi_up@6.0.2/ansi_up.js"></script>
<style>
  :root { --bg: #1a1a2e; --card-bg: #16213e; --text: #e0e0e0; --muted: #888;
    --pass: #4caf50; --fail: #f44336; --accent: #7c83ff; --border: #2a2a4a; }
  *{margin:0;padding:0;box-sizing:border-box}
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;background:var(--bg);color:var(--text);padding:20px}
  h1{font-size:1.5em;margin-bottom:4px}
  .summary{display:flex;gap:20px;margin:16px 0;flex-wrap:wrap}
  .stat{background:var(--card-bg);border-radius:8px;padding:12px 20px;border:1px solid var(--border)}
  .stat-val{font-size:1.8em;font-weight:bold}
  .stat-label{font-size:.8em;color:var(--muted)}
  .pass-color{color:var(--pass)}.fail-color{color:var(--fail)}
  .test-group{margin-top:24px}
  .test-group h2{font-size:1.1em;padding:8px 0;border-bottom:1px solid var(--border);margin-bottom:12px}
  .snapshot-card{background:var(--card-bg);border-radius:8px;border:1px solid var(--border);margin-bottom:12px;overflow:hidden}
  .snapshot-card.pass{border-left:3px solid var(--pass)}
  .snapshot-card.fail{border-left:3px solid var(--fail)}
  .card-header{display:flex;justify-content:space-between;align-items:center;padding:10px 16px;background:rgba(255,255,255,.03);cursor:pointer;user-select:none}
  .card-header:hover{background:rgba(255,255,255,.06)}
  .card-title{font-weight:600;font-size:.95em}
  .card-meta{font-size:.75em;color:var(--muted)}
  .card-body{display:none;padding:16px}
  .card-body.open{display:block}
  .ansi-screen{background:#0d0d1a;color:#e0e0e0;padding:12px;border-radius:4px;font-family:"JetBrains Mono","Fira Code","Cascadia Code",monospace;font-size:.78em;line-height:1.35;overflow-x:auto;white-space:pre;max-height:500px;overflow-y:auto}
  .checks{margin-top:12px}
  .check-item{display:flex;align-items:flex-start;gap:8px;padding:6px 0;border-bottom:1px solid rgba(255,255,255,.04)}
  .check-icon{font-size:.9em;width:20px;flex-shrink:0}
  .check-icon.pass{color:var(--pass)}.check-icon.fail{color:var(--fail)}
  .check-criterion{flex:1;font-size:.88em}
  .check-detail{color:var(--muted);font-size:.8em;margin-top:2px}
  .no-judge{color:var(--muted);font-style:italic;font-size:.85em;padding:8px 0}
  .text-preview{color:var(--muted);font-size:.78em;font-family:monospace;margin-top:8px;padding:8px;background:rgba(255,255,255,.02);border-radius:4px;max-height:80px;overflow:hidden}
  .timestamp{margin-top:24px;font-size:.75em;color:var(--muted);text-align:center}
  .screen-toggle{margin-top:8px;font-size:.8em;color:var(--accent);cursor:pointer;display:inline-block}
  .screen-toggle:hover{text-decoration:underline}
</style>
</head>
<body>
<h1>Peri E2E 测试报告</h1>
<div class="summary">
  <div class="stat"><div class="stat-val">${data.total}</div><div class="stat-label">总 snapshot</div></div>
  <div class="stat"><div class="stat-val pass-color">${data.passed}</div><div class="stat-label">通过</div></div>
  <div class="stat"><div class="stat-val fail-color">${data.failed}</div><div class="stat-label">失败</div></div>
  <div class="stat"><div class="stat-val">${groupCount}</div><div class="stat-label">测试用例</div></div>
</div>
<div id="groups"></div>
<div class="timestamp">生成时间: ${new Date().toLocaleString("zh-CN")}</div>

<script>
const groups = ${groupsJson};
const ansi_up = new AnsiUp();
ansi_up.use_classes = true;
ansi_up.escape_for_html = false;

const container = document.getElementById("groups");

for (const group of groups) {
  const el = document.createElement("div");
  el.className = "test-group";
  el.innerHTML = "<h2>" + esc(group.name) + "</h2>";

  for (const item of group.records) {
    const verdict = item.verdict || "";
    const card = document.createElement("div");
    card.className = "snapshot-card " + verdict;

    const header = document.createElement("div");
    header.className = "card-header";
    header.onclick = function() {
      this.nextElementSibling.classList.toggle("open");
    };

    const formatTime = function(ts) {
      try { return new Date(ts).toLocaleTimeString("zh-CN"); } catch(e) { return ts; }
    };

    const durText = item.duration_ms != null ? item.duration_ms + "ms" : "";
    const sizeText = item.size ? item.size.cols + "\u00d7" + item.size.rows : "";

    header.innerHTML = "<span class=\"card-title\">"
      + (verdict === "pass" ? "\u2705 " : verdict === "fail" ? "\u274c " : "\uD83D\uDCF8 ")
      + "Step " + item.step + ": " + esc(item.name)
      + "</span>"
      + "<span class=\"card-meta\">"
      + formatTime(item.timestamp)
      + (durText ? " \u00b7 " + durText : "")
      + (sizeText ? " \u00b7 " + sizeText : "")
      + "</span>";

    const body = document.createElement("div");
    body.className = "card-body";
    let bodyHtml = "";

    // 文本预览
    if (item.textPreview) {
      bodyHtml += "<div class=\"text-preview\">" + esc(item.textPreview) + "</div>";
    }

    // ANSI 屏幕渲染（可折叠）
    if (item.ansiRaw) {
      bodyHtml += "<span class=\"screen-toggle\" onclick=\"var s=this.nextElementSibling;s.style.display=s.style.display==='block'?'none':'block'\">\u25b6 查看完整屏幕</span>";
      bodyHtml += "<div class=\"ansi-screen\" style=\"display:none\">" + ansi_up.ansi_to_html(item.ansiRaw) + "</div>";
    }

    // judge checks
    if (item.checks && item.checks.length > 0) {
      bodyHtml += "<div class=\"checks\">";
      for (const c of item.checks) {
        bodyHtml += "<div class=\"check-item\">"
          + "<span class=\"check-icon " + (c.pass ? "pass" : "fail") + "\">" + (c.pass ? "\u2713" : "\u2717") + "</span>"
          + "<div><div class=\"check-criterion\">" + esc(c.criterion) + "</div>"
          + (c.detail ? "<div class=\"check-detail\">" + esc(c.detail) + "</div>" : "")
          + "</div></div>";
      }
      bodyHtml += "</div>";
    } else {
      bodyHtml += "<div class=\"no-judge\">\u672a\u6267\u884c LLM judge</div>";
    }

    body.innerHTML = bodyHtml;
    card.appendChild(header);
    card.appendChild(body);
    el.appendChild(card);
  }
  container.appendChild(el);
}

function esc(s) { return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;"); }
</script>
</body>
</html>`;
}

// ---- 入口 ----

async function main() {
  if (isVitestTeardown) {
    // vitest globalTeardown: 直接生成，不 watch
    await generate();
    return;
  }

  const isWatch = process.argv.includes("--watch");

  if (isWatch) {
    console.log("👀 监听 index.jsonl 变化...");
    const indexPath = path.join(RECORDINGS_DIR, "index.jsonl");

    await generate();
    const { watch } = await import("node:fs");

    // 用轮询 + timer 代替 fs.watch（更可靠，且可用 ref/unref 控制退出）
    let timer: ReturnType<typeof setInterval>;
    timer = setInterval(async () => {
      try {
        await generate();
      } catch (err) {
        console.error("报告生成失败:", err);
      }
    }, 3000);

    // Ctrl+C 时清理
    process.on("SIGINT", () => { clearInterval(timer); process.exit(0); });
    process.on("SIGTERM", () => { clearInterval(timer); process.exit(0); });
  } else {
    await generate();
  }
}

// 直接运行时执行
const isDirectRun = process.argv[1] && import.meta.url.endsWith(process.argv[1].replace(/^\.\//, ""));
if (isDirectRun || process.argv[1]?.includes("generate-report")) {
  main().catch(console.error);
}

// vitest globalTeardown 调用时的默认导出
export default generate;
