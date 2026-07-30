// 深入分析 peri 协作数据
import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";

const DB_PATH = join(homedir(), ".peri/threads/threads.db");
const db = new Database(DB_PATH, { readonly: true });

// ── 1. 检查消息内容格式 ──
const sample = db.query(`
  SELECT role, content, rowid FROM messages
  WHERE thread_id = (SELECT id FROM threads WHERE hidden=0 ORDER BY updated_at DESC LIMIT 1)
  ORDER BY rowid ASC LIMIT 3
`).all() as { role: string; content: string; rowid: number }[];
console.log("=== 消息格式样本 ===");
sample.forEach(s => {
  const preview = s.content.slice(0, 500);
  console.log(`[${s.role}] ${preview}\n`);
});

// ── 2. 用更好的方式统计工具使用 ──
const asstSample = db.query(`
  SELECT content FROM messages WHERE role = 'assistant' AND content LIKE '%tool_use%'
  LIMIT 500
`).all() as { content: string }[];

const toolCount: Record<string, number> = {};
let parsedCount = 0;
for (const row of asstSample) {
  try {
    const parsed = JSON.parse(row.content);
    const blocks = Array.isArray(parsed) ? parsed : [parsed];
    for (const block of blocks) {
      if (block.type === "tool_use" && typeof block.name === "string") {
        toolCount[block.name] = (toolCount[block.name] || 0) + 1;
        parsedCount++;
      }
    }
  } catch { /* skip */ }
}

const sorted = Object.entries(toolCount).sort(([,a],[,b]) => b - a);
console.log(`\n=== 工具使用 Top 20 (从 ${asstSample.length} 条 assistant 消息, ${parsedCount} tool_use blocks) ===`);
sorted.slice(0, 20).forEach(([n,c]) => console.log(`  ${n}: ${c}`));

// ── 3. 用纯字符串匹配做补充(处理 JSON 解析失败的情况) ──
const toolPattern = /"name"\s*:\s*"([^"]+)"/g;
const fallbackCount: Record<string, number> = {};
const allAsst = db.query(`
  SELECT content FROM messages WHERE role = 'assistant' LIMIT 50000
`).all() as { content: string }[];
for (const row of allAsst) {
  let m;
  while ((m = toolPattern.exec(row.content)) !== null) {
    const tool = m[1];
    // 只计数看起来像工具名的
    if (/^[A-Z]/.test(tool) && tool.length < 50) {
      fallbackCount[tool] = (fallbackCount[tool] || 0) + 1;
    }
  }
}
const sorted2 = Object.entries(fallbackCount).sort(([,a],[,b]) => b - a);
console.log(`\n=== 工具使用 Top 30 (字符串匹配, 50K 条 assistant 消息) ===`);
sorted2.slice(0, 30).forEach(([n,c]) => console.log(`  ${n}: ${c}`));

// ── 4. 分析各项目目录的会话特征 ──
const mainProjects = [
  "/Users/konghayao/code/ai/perihelion",
  "/Users/konghayao/code/pazhou/remote-control-server",
  "/Users/konghayao/code/ai/claude-code",
  "/Users/konghayao/code/ai/agent-sites",
  "/Users/konghayao/code/ai/openwiki",
  "/Users/konghayao/code/knowledgebase",
];

console.log("\n=== 各项目会话特征 ===");
for (const cwd of mainProjects) {
  const proj = db.query(`
    SELECT
      COUNT(*) as sessions,
      AVG(msg_cnt) as avg_msgs,
      MAX(msg_cnt) as max_msgs,
      SUM(sub_cnt) as total_subs
    FROM (
      SELECT t.id,
        (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id) as msg_cnt,
        (SELECT COUNT(*) FROM threads sub WHERE sub.parent_thread_id = t.id) as sub_cnt
      FROM threads t
      WHERE t.hidden = 0 AND t.cwd = ?
    )
  `).get(cwd) as any;
  if (proj.sessions > 0) {
    console.log(`  ${cwd.split('/').pop()}: ${proj.sessions}会话 均${Math.round(proj.avg_msgs)}msg/会 最长${proj.max_msgs}msg ${proj.total_subs}子代理`);
  }
}

// ── 5. 从最近的 perihelion 会话中提取用户真实提示 ──
const periThreads = db.query(`
  SELECT id, title, updated_at,
    (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id) as msg_cnt
  FROM threads t
  WHERE t.hidden = 0 AND t.cwd = '/Users/konghayao/code/ai/perihelion'
  ORDER BY t.updated_at DESC
  LIMIT 20
`).all() as any[];

console.log("\n=== 最近 perihelion 项目的会话 ===");
for (const thread of periThreads) {
  const userMsgs = db.query(`
    SELECT content FROM messages
    WHERE thread_id = ? AND role = 'user'
    ORDER BY rowid ASC LIMIT 5
  `).all(thread.id) as { content: string }[];

  const firstQuestion = userMsgs.map(m => {
    try {
      const parsed = JSON.parse(m.content);
      if (Array.isArray(parsed)) {
        const texts = parsed.filter((b: any) => b.type === "text").map((b: any) => b.text);
        return texts.join(" ").slice(0, 100);
      }
      if (typeof parsed === "string") return parsed.slice(0, 100);
      if (parsed.type === "text" && parsed.text) return parsed.text.slice(0, 100);
      return "";
    } catch { return ""; }
  }).filter(Boolean);

  if (firstQuestion.length > 0) {
    console.log(`  [${thread.updated_at?.slice(0,16)}] ${thread.msg_cnt}msgs`);
    firstQuestion.slice(0, 3).forEach((q, i) => console.log(`    Q${i+1}: ${q}`));
  }
}

// ── 6. 错误率分析 ──
const errorStats = db.query(`
  SELECT
    (SELECT COUNT(*) FROM messages WHERE role='tool' AND content LIKE '%is_error":true%') as tool_errors,
    (SELECT COUNT(*) FROM messages WHERE role='tool') as total_tools
`).get() as any;
console.log(`\n=== 错误统计 ===`);
console.log(`  工具执行错误: ${errorStats.tool_errors}/${errorStats.total_tools} (${(errorStats.tool_errors/errorStats.total_tools*100).toFixed(1)}%)`);

// ── 7. 子代理使用模式 ──
const subDist = db.query(`
  SELECT parent_thread_id, COUNT(*) as cnt
  FROM threads WHERE parent_thread_id IS NOT NULL
  GROUP BY parent_thread_id
  ORDER BY cnt DESC LIMIT 10
`).all() as { parent_thread_id: string; cnt: number }[];

if (subDist.length > 0) {
  console.log("\n=== 子代理密集的会话 Top 10 ===");
  for (const s of subDist) {
    const parent = db.query("SELECT title, cwd FROM threads WHERE id = ?").get(s.parent_thread_id) as any;
    const cwdName = parent?.cwd?.split('/').pop() || '?';
    console.log(`  ${cwdName}: ${s.cnt}子代理 | ${(parent?.title || '').slice(0, 80)}`);
  }
}

db.close();
