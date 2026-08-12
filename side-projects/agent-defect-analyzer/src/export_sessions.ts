//! export_sessions.ts — 按 id 导出会话完整消息序列，供 subagent 定性评估。
//!
//! 用法:
//!   bun run src/export_sessions.ts --ids id1,id2,... [--out-dir /tmp/long-sessions]
//!
//! 输出:
//!   <out-dir>/<id>.md   每会话一个文件(超长自动分片 .partN.md)
//!   <out-dir>/index.md  抽样清单
//!
//! 导出格式:
//!   [#n role] 序号消息; user 全文; assistant text 全文; reasoning 截断 200 字符;
//!   tool_use 一行摘要(工具+关键参数); tool_result 摘要(字节/是否 error)
//! 过滤: excluded 消息(从未进入 LLM 上下文)跳过, 但计入占位行

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { mkdirSync, writeFileSync } from "fs";

const argv = process.argv.slice(2);
const get = (key: string): string | undefined => {
  const i = argv.indexOf(key);
  return i >= 0 ? argv[i + 1] : undefined;
};
const DB_PATH = get("--db") ?? join(homedir(), ".peri/threads/threads.db");
const ids = (get("--ids") ?? "").split(",").map((s) => s.trim()).filter(Boolean);
const OUT_DIR = get("--out-dir") ?? "/tmp/long-sessions";

if (ids.length === 0) {
  console.error("用法: bun run src/export_sessions.ts --ids id1,id2,...");
  process.exit(1);
}

const db = new Database(DB_PATH, { readonly: true });
mkdirSync(OUT_DIR, { recursive: true });

// ── 工具参数摘要 ──

function summarizeArgs(name: string, input: Record<string, unknown>): string {
  const pick = (keys: string[]) => {
    const out: string[] = [];
    for (const k of keys) {
      const v = input[k];
      if (v !== undefined && v !== null) out.push(`${k}=${String(v).slice(0, 100)}`);
    }
    return out.join(", ");
  };
  switch (name) {
    case "Grep": return pick(["pattern", "path", "glob", "head_limit", "output_mode"]);
    case "Glob": return pick(["pattern", "path"]);
    case "Read": return pick(["file_path", "path", "offset", "limit"]);
    case "Edit": return pick(["file_path", "old_string", "replace_all"]) + (input.new_string ? ` | new=${String(input.new_string).slice(0, 60)}` : "");
    case "Write": return pick(["file_path"]) + (input.content ? ` | len=${String(input.content).length}` : "");
    case "Bash": return pick(["command", "timeout", "run_in_background"]);
    case "WebFetch":
    case "WebSearch": return pick(["url", "query", "prompt"]);
    case "Agent": return pick(["subagent_type", "description", "run_in_background"]) + (input.prompt ? ` | prompt=${String(input.prompt).slice(0, 80)}` : "");
    case "SkillTool": return pick(["skill_name"]);
    case "AgentResult": return pick(["task_id"]);
    case "TodoWrite": return pick(["todos"]) || "…";
    default:
      return Object.entries(input).map(([k, v]) => `${k}=${typeof v === "string" ? v.slice(0, 60) : JSON.stringify(v).slice(0, 60)}`).join(", ").slice(0, 200);
  }
}

function textOfBlock(b: any): string | null {
  if (!b || typeof b !== "object") return null;
  if (b.type === "text" && typeof b.text === "string") return b.text;
  return null;
}

// ── 消息格式化 ──

function formatMessage(content: string, idx: number): string[] {
  let p: any;
  try { p = JSON.parse(content); } catch { return [`[#${idx} parse_fail]`]; }
  const out: string[] = [];
  const tag = `[#${idx} ${p.role}]`;
  if (p.role === "user") {
    const c = p.content;
    const text = typeof c === "string" ? c : (Array.isArray(c) ? c.map(textOfBlock).filter(Boolean).join("\n") : "");
    out.push(`${tag} USER: ${text}`);
  } else if (p.role === "assistant") {
    const blocks: any[] = Array.isArray(p.content) ? p.content : [];
    let text = "";
    const uses: any[] = [];
    const reasons: string[] = [];
    for (const b of blocks) {
      if (!b || typeof b !== "object") continue;
      if (b.type === "tool_use") uses.push(b);
      else if (b.type === "reasoning" || b.type === "thinking") reasons.push(b.text);
      else if (typeof b.text === "string") text += b.text + "\n";
    }
    if (reasons.length) out.push(`${tag} REASONING: ${reasons.join(" | ").slice(0, 400)}`);
    if (text.trim()) out.push(`${tag} TEXT: ${text.trim()}`);
    for (const u of uses) out.push(`${tag} TOOL_USE: ${u.name}(${summarizeArgs(u.name, u.input ?? {})})`);
  } else if (p.role === "tool") {
    const c = p.content;
    const s = typeof c === "string" ? c : JSON.stringify(c);
    out.push(`${tag} TOOL_RESULT${p.is_error ? " [ERROR]" : ""}: ${s.length}B${p.is_error ? ` | ${s.slice(0, 300)}` : ""}`);
  } else {
    out.push(`${tag} (${typeof p.content === "string" ? p.content.slice(0, 100) : "…"})`);
  }
  return out;
}

// ── 主流程 ──

const meta: { id: string; title: string; created: string; updated: string; messages: number; cwd: string; compact: string }[] = [];

for (const id of ids) {
  const t = db.query(`SELECT * FROM threads WHERE id = ?`).get(id) as any;
  if (!t) { console.error(`会话不存在: ${id}`); continue; }
  const rows = db.query(`SELECT role, content, excluded, truncated, projection, rowid FROM messages WHERE thread_id = ? ORDER BY rowid ASC`).all(id) as any[];
  const compactFlags: string[] = [];
  const excl = rows.filter((r) => r.excluded).length;
  const trunc = rows.filter((r) => r.truncated).length;
  const proj = rows.filter((r) => r.projection).length;
  if (excl) compactFlags.push(`excluded=${excl}`);
  if (trunc) compactFlags.push(`truncated=${trunc}`);
  if (proj) compactFlags.push(`projection=${proj}`);
  meta.push({ id, title: t.title, created: t.created_at, updated: t.updated_at, messages: rows.length, cwd: t.cwd, compact: compactFlags.join(" ") || "无" });

  const lines: string[] = [
    `# 会话 ${id}`,
    `标题: ${t.title}`,
    `时间: ${t.created_at} ~ ${t.updated_at}`,
    `cwd: ${t.cwd}`,
    `消息数: ${rows.length} | compact: ${compactFlags.join(", ") || "无"}`,
    ``,
    `说明: [#n role] 为消息序号; excluded 消息(从未进入 LLM 上下文)显示为占位行; REASONING 为思考(截断); TOOL_RESULT 只显示字节数与错误摘要。`,
    ``,
  ];
  let skipped = 0;
  let count = 0;
  for (const r of rows) {
    count++;
    if (r.excluded) { skipped++; continue; }
    lines.push(...formatMessage(r.content, count));
  }
  lines.push(``, `(excluded 跳过 ${skipped} 条)`);

  // 分片写入
  const CHUNK = 1500;
  if (lines.length <= CHUNK) {
    writeFileSync(join(OUT_DIR, `${id}.md`), lines.join("\n"));
  } else {
    for (let i = 0; i < lines.length; i += CHUNK) {
      writeFileSync(join(OUT_DIR, `${id}.part${Math.floor(i / CHUNK) + 1}.md`), lines.slice(i, i + CHUNK).join("\n"));
    }
  }
  console.log(`${id.slice(0, 8)} ${t.title?.slice(0, 30)} | ${rows.length} 消息 | 可见 ${rows.length - skipped} | ${compactFlags.join(",") || "无compact"}`);
}

writeFileSync(join(OUT_DIR, "index.md"), [
  "# 超长会话定性评估抽样清单",
  "",
  "| id | 标题 | 消息数 | 创建 | compact |",
  "| --- | --- | --- | --- | --- |",
  ...meta.map((m) => `| ${m.id.slice(0, 8)} | ${(m.title ?? "").replace(/\|/g, "\\|").slice(0, 40)} | ${m.messages} | ${m.created.slice(0, 10)} | ${m.compact} |`),
  "",
].join("\n"));
db.close();
console.log(`\n输出: ${OUT_DIR}`);
