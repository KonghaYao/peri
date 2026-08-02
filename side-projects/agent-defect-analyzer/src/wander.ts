//! wander.ts — 闲逛模式：随机抽取长会话并导出精简文本，供 subagent 深度研究。
//!
//! 用法:
//!   bun run src/wander.ts --count 5 --min-messages 100 [--seed 20260802] [--since 2026-08-01] [--exclude id1,id2] [--out-dir /tmp/wander-2026-08-02]
//!
//! 输出:
//!   <out-dir>/index.md          抽样清单（id/标题/消息数/cwd/时间段）
//!   <out-dir>/<id>.md           每个会话一条精简消息序列（编号 + 工具调用 + 结果/错误）
//!   超长会话自动按 1800 行分片为 <id>.partN.md

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { mkdirSync, writeFileSync } from "fs";

const DEFAULT_DB_PATH = join(homedir(), ".peri/threads/threads.db");

// ── CLI ──

function parseArgs(): {
  count: number;
  minMessages: number;
  seed: number;
  since?: string;
  exclude: string[];
  outDir: string;
} {
  const argv = process.argv.slice(2);
  const get = (key: string): string | undefined => {
    const i = argv.indexOf(key);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  const count = Number(get("--count") ?? 5);
  const minMessages = Number(get("--min-messages") ?? 100);
  const seed = Number(get("--seed") ?? Date.now());
  const since = get("--since");
  const exclude = (get("--exclude") ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  const outDir = get("--out-dir") ?? `/tmp/wander-${new Date().toISOString().slice(0, 10)}`;
  return { count, minMessages, seed, since, exclude, outDir };
}

// 可复现伪随机（mulberry32）
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ── 消息格式化 ──

interface Row {
  id: string;
  title: string | null;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

interface ToolUse {
  id: string;
  name: string;
  args: Record<string, unknown>;
}

function summarizeArgs(name: string, input: Record<string, unknown>): string {
  // 高价值工具保留关键参数，其余压缩
  const pick = (keys: string[]) => {
    const out: string[] = [];
    for (const k of keys) {
      const v = input[k];
      if (v !== undefined && v !== null) out.push(`${k}=${String(v).slice(0, 120)}`);
    }
    return out.join(", ");
  };
  switch (name) {
    case "Grep":
      return pick(["pattern", "path", "glob", "head_limit", "output_mode", "case_insensitive"]);
    case "Glob":
      return pick(["pattern", "path"]);
    case "Read":
      return pick(["file_path", "path", "offset", "limit"]);
    case "Edit":
      return pick(["file_path", "old_string", "replace_all"]) + (input.new_string ? ` | new=${String(input.new_string).slice(0, 80)}` : "");
    case "Write":
      return pick(["file_path"]) + (input.content ? ` | len=${String(input.content).length}` : "");
    case "Bash":
      return pick(["command", "timeout", "run_in_background"]);
    case "WebFetch":
    case "WebSearch":
      return pick(["url", "query", "prompt"]);
    case "Agent":
      return pick(["subagent_type", "description", "prompt", "run_in_background"]);
    default:
      return Object.entries(input)
        .map(([k, v]) => `${k}=${typeof v === "string" ? v.slice(0, 80) : JSON.stringify(v).slice(0, 80)}`)
        .join(", ")
        .slice(0, 300);
  }
}

function truncate(s: string, n: number): string {
  if (s.length <= n) return s;
  return s.slice(0, n - 1) + "…";
}

function textOfBlock(block: any): string | null {
  if (!block || typeof block !== "object") return null;
  if (block.type === "text" && typeof block.text === "string") return block.text;
  if (block.type === "reasoning" && typeof block.text === "string") return block.text;
  return null;
}

function formatMessage(content: string, idx: number): string | null {
  let p: any;
  try {
    p = JSON.parse(content);
  } catch {
    return null;
  }
  const tag = `[#${idx} ${p.role}]`;
  if (p.role === "user") {
    const c = p.content;
    const text = typeof c === "string" ? c : (Array.isArray(c) ? c.map(textOfBlock).filter(Boolean).join(" ") : "");
    return `${tag} ${truncate(text.replace(/\s+/g, " ").trim(), 500)}`;
  }
  if (p.role === "assistant") {
    const parts: string[] = [];
    const blocks: any[] = Array.isArray(p.content) ? p.content : [];
    // 去重：6-05~7-31 的"双写"格式下 content[] 与顶层 tool_calls 各含同一
    // tool_use（同 id），两个来源都收集会打印两遍。按 id 去重，id 缺失时按
    // name+参数 JSON 去重。
    const seen = new Set<string>();
    const toolUses: ToolUse[] = [];
    let text = "";
    const addTu = (tu: ToolUse) => {
      const key = tu.id || `${tu.name}::${JSON.stringify(tu.args)}`;
      if (seen.has(key)) return;
      seen.add(key);
      toolUses.push(tu);
    };
    for (const b of blocks) {
      if (b.type === "tool_use") addTu({ id: b.id, name: b.name, args: b.input ?? {} });
      else text += (textOfBlock(b) ?? "") + " ";
    }
    // 顶层 tool_calls（8-01 起的新格式：tool_use 剥离到顶层）
    if (Array.isArray(p.tool_calls)) {
      for (const c of p.tool_calls) addTu({ id: c.id, name: c.name, args: c.arguments ?? {} });
    }
    for (const tu of toolUses) {
      parts.push(`⚙ ${tu.name}(${summarizeArgs(tu.name, tu.args)})`);
    }
    const textClean = truncate(text.replace(/\s+/g, " ").trim(), 400);
    if (textClean) parts.push(`"${textClean}"`);
    if (parts.length === 0) return null;
    return `${tag} ${parts.join(" | ")}`;
  }
  if (p.role === "tool") {
    const c = typeof p.content === "string" ? p.content : JSON.stringify(p.content);
    const err = p.is_error ? "✗ ERROR" : "✓ ok";
    return `${tag} ${err}: ${truncate(c.replace(/\s+/g, " ").trim(), 300)}`;
  }
  if (p.role === "system") return null; // 系统提示词不导出（太长且非行为证据）
  return null;
}

// ── 导出 ──

function exportThread(db: Database, t: Row, outDir: string): { path: string; parts: number } {
  const rows = db
    .query("SELECT content FROM messages WHERE thread_id = ? ORDER BY rowid ASC")
    .all(t.id) as { content: string }[];

  const header = [
    `# ${t.title ?? "(无标题)"}`,
    ``,
    `- id: ${t.id}`,
    `- cwd: ${t.cwd}`,
    `- created: ${t.created_at}`,
    `- updated: ${t.updated_at}`,
    `- message_count: ${t.message_count}（导出 ${rows.length} 条）`,
    ``,
    `---`,
    ``,
  ];

  const lines: string[] = [];
  rows.forEach((r, i) => {
    const line = formatMessage(r.content, i);
    if (line) lines.push(line);
  });

  const all = [...header, ...lines];
  const PART_LIMIT = 1800; // Read 工具 2000 行内可整读
  const parts = Math.ceil(all.length / PART_LIMIT);
  const base = join(outDir, t.id.slice(0, 13));
  if (parts <= 1) {
    writeFileSync(`${base}.md`, all.join("\n"), "utf8");
    return { path: `${base}.md`, parts: 1 };
  }
  const paths: string[] = [];
  for (let i = 0; i < parts; i++) {
    const chunk = all.slice(i * PART_LIMIT, (i + 1) * PART_LIMIT);
    const p = `${base}.part${i + 1}.md`;
    writeFileSync(p, chunk.join("\n"), "utf8");
    paths.push(p);
  }
  return { path: paths.join(" + "), parts };
}

// ── main ──

const { count, minMessages, seed, since, exclude, outDir } = parseArgs();
const db = new Database(DEFAULT_DB_PATH, { readonly: true });

const where = ["hidden = 0", "parent_thread_id IS NULL", "message_count >= ?"];
const params: unknown[] = [minMessages];
if (since) {
  where.push("created_at >= ?");
  params.push(since.length <= 10 ? `${since}T00:00:00` : since);
}
const threads = db
  .query(
    `SELECT id, title, cwd, created_at, updated_at, message_count FROM threads
     WHERE ${where.join(" AND ")}
     ORDER BY created_at ASC`,
  )
  .all(...params) as Row[];

// 排除指定 id（前缀匹配，如当前调查会话自身）
const pool = threads.filter(
  (t) => !exclude.some((e) => t.id.startsWith(e)),
);

// 种子洗牌
const rand = mulberry32(seed);
for (let i = pool.length - 1; i > 0; i--) {
  const j = Math.floor(rand() * (i + 1));
  [pool[i], pool[j]] = [pool[j], pool[i]];
}
const picked = pool.slice(0, Math.min(count, pool.length));

mkdirSync(outDir, { recursive: true });
const indexLines: string[] = [
  `# Wander 抽样清单`,
  ``,
  `- seed: ${seed}，候选池: ${threads.length} 个长会话（message_count >= ${minMessages}），抽样: ${picked.length}`,
  ``,
  `| # | id | 消息数 | cwd | 标题 | 文件 |`,
  `|---|----|-------:|-----|------|------|`,
];
picked.forEach((t, i) => {
  const { path, parts } = exportThread(db, t, outDir);
  indexLines.push(
    `| ${i + 1} | ${t.id.slice(0, 13)} | ${t.message_count} | ${t.cwd.replace("/Users/konghayao/code/", "~/code/")} | ${(t.title ?? "").slice(0, 40).replace(/\|/g, "\\|")} | ${path.replace(outDir + "/", "")} (${parts}p) |`,
  );
});
indexLines.push(``, `- 生成: ${new Date().toISOString()}`);
writeFileSync(join(outDir, "index.md"), indexLines.join("\n"), "utf8");

console.log(`seed=${seed} 候选=${threads.length} 抽取=${picked.length} → ${outDir}/`);
for (const t of picked) {
  console.log(`  ${t.id.slice(0, 13)}  msg=${t.message_count}  ${(t.title ?? "").slice(0, 40)}`);
}
db.close();
