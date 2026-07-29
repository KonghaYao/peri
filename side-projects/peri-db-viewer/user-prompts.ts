import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";

const DB_PATH = join(homedir(), ".peri/threads/threads.db");
const db = new Database(DB_PATH, { readonly: true });

// 提取 perihelion 项目最近 50 个会话的主题和第一条用户消息
const threads = db.query(`
  SELECT t.id, t.title, t.updated_at,
    (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id) as msg_cnt,
    (SELECT COUNT(*) FROM threads sub WHERE sub.parent_thread_id = t.id) as sub_cnt
  FROM threads t
  WHERE t.hidden = 0 AND t.cwd = '/Users/konghayao/code/ai/perihelion'
    AND (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id) > 5
  ORDER BY t.updated_at DESC
  LIMIT 50
`).all() as any[];

for (const thread of threads) {
  const userMsgs = db.query(`
    SELECT content FROM messages
    WHERE thread_id = ? AND role = 'user'
    ORDER BY rowid ASC
  `).all(thread.id) as { content: string }[];

  const texts: string[] = [];
  for (const m of userMsgs) {
    try {
      const parsed = JSON.parse(m.content);
      if (typeof parsed.content === "string") {
        texts.push(parsed.content);
      } else if (Array.isArray(parsed.content)) {
        const t = parsed.content.filter((b: any) => b.type === "text").map((b: any) => b.text).join("\n");
        if (t) texts.push(t);
      }
    } catch {}
  }

  // 只打印非空、有意义的输入
  const meaningful = texts.filter(t => t.trim().length > 3);
  if (meaningful.length > 0) {
    const date = thread.updated_at?.slice(0, 10) || '?';
    const title = (thread.title || '').slice(0, 80);
    const first = meaningful[0].slice(0, 120).replace(/\n/g, ' ');
    console.log(`[${date}] ${thread.msg_cnt}msgs ${thread.sub_cnt}subs | ${first}`);
  }
}

db.close();
