#!/usr/bin/env node
/**
 * opencode → peri session 数据迁移脚本
 *
 * 将 opencode 的 session 数据（SQLite：storage/opencode.db）迁移为 peri 的
 * ThreadStore 数据库格式（对齐 peri-resources/src/sessions/sqlite_store.rs 的 schema
 * 与 peri-acp-types 的 BaseMessage JSON 格式）。
 *
 * 用法（Node >= 22.5，零依赖，使用内置 node:sqlite）：
 *   npx tsx scripts/migrate-opencode-to-peri.ts --src ~/.local/share/opencode --out /tmp/opencode-peri.db
 *   npx tsx ./migrate-opencode-to-peri.ts --src /app/opencode-data --out ./threads.db
 *
 * 选项：
 *   --src <dir>               opencode 数据目录（含 opencode.db，只读打开）
 *   --out <file>              输出 peri sqlite 数据库文件（必须为新路径）
 *   --force                   允许覆盖已存在的非空输出文件
 *   --include-subagents       同时迁移子 agent session（默认仅根 session）
 *   --include-empty           同时迁移无消息的 session（默认跳过）
 *   --limit <n>               仅迁移最近 n 个 session（按 time_created 降序，便于试跑）
 *
 * 路径字段对齐：
 *   threads.cwd 与 opencode session.directory 字段原样对齐（directory 是 path
 *   字段相对 project.worktree 的绝对化形式，从不为空），不做过任何规范化改写。
 *
 * 安全保证：
 *   1. 源数据库（opencode 生产数据）以只读模式打开，任何情况都不会写入；
 *      只读打开失败（WAL 文件缺失等）时报错退出，绝不回退为读写模式。
 *   2. 输出路径被解析为绝对路径后，若位于 ~/.peri 下则直接拒绝——
 *      peri 生产库位于 ~/.peri/threads/threads.db，脚本绝不触碰。
 *   3. 输出文件已存在且非空时拒绝覆盖（需显式 --force）。
 */

import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
import { existsSync, statSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

// ─── CLI 解析 ──────────────────────────────────────────────────────────────────

interface Options {
  src: string;
  out: string;
  force: boolean;
  includeSubagents: boolean;
  includeEmpty: boolean;
  limit: number | null;
}

function usage(): never {
  console.error(
    [
      "用法: npx tsx scripts/migrate-opencode-to-peri.ts --src <opencode目录> --out <输出db> [选项]",
      "",
      "  --src <dir>                opencode 数据目录（含 opencode.db，只读打开）",
      "  --out <file>               输出 peri sqlite 数据库文件（必须为新路径）",
      "  --force                    允许覆盖已存在的非空输出文件",
      "  --include-subagents        同时迁移子 agent session（默认仅根 session）",
      "  --include-empty            同时迁移无消息的 session（默认跳过）",
      "  --limit <n>                仅迁移最近 n 个 session（按 time_created 降序）",
      "  -h, --help                 显示帮助",
    ].join("\n"),
  );
  process.exit(2);
}

function parseArgs(argv: string[]): Options {
  const opts: Options = {
    src: "",
    out: "",
    force: false,
    includeSubagents: false,
    includeEmpty: false,
    limit: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) {
        console.error(`缺少参数值: ${arg}`);
        usage();
      }
      return argv[++i];
    };
    switch (arg) {
      case "--src":
        opts.src = next();
        break;
      case "--out":
        opts.out = next();
        break;
      case "--force":
        opts.force = true;
        break;
      case "--include-subagents":
        opts.includeSubagents = true;
        break;
      case "--include-empty":
        opts.includeEmpty = true;
        break;
      case "--limit":
        opts.limit = Number(next());
        if (!Number.isInteger(opts.limit) || opts.limit <= 0) {
          console.error("--limit 必须是正整数");
          usage();
        }
        break;
      case "-h":
      case "--help":
        usage();
        break;
      default:
        console.error(`未知参数: ${arg}`);
        usage();
    }
  }
  if (!opts.src || !opts.out) {
    console.error("--src 与 --out 均为必填参数");
    usage();
  }
  return opts;
}

// ─── 工具函数 ──────────────────────────────────────────────────────────────────

/** opencode 的 id（msg_xxx / ses_xxx）确定性映射为 UUID v5，保证重复运行幂等 */
function uuidv5(name: string): string {
  // 固定命名空间（URL 命名空间的变体），保证跨运行稳定
  const ns = Buffer.from("6ba7b811-9dad-11d1-80b4-00c04fd430c8".replaceAll("-", ""), "hex");
  const hash = createHash("sha1").update(ns).update(name, "utf8").digest();
  hash[6] = (hash[6] & 0x0f) | 0x50; // version 5
  hash[8] = (hash[8] & 0x3f) | 0x80; // RFC 4122 variant
  const hex = hash.subarray(0, 16).toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20, 32)}`;
}

/** opencode 的 epoch 时间戳（ms；兼容秒）→ peri 的 RFC3339（与 chrono DateTime<Utc> 格式一致：微秒 + +00:00） */
function toRfc3339(epoch: number | null | undefined): string {
  if (!epoch) return new Date(0).toISOString().replace(/\.\d{3}Z$/, ".000000+00:00");
  const ms = epoch > 1e12 ? epoch : epoch * 1000;
  const date = new Date(ms);
  // toISOString() 是毫秒 + Z；生产库为微秒（6 位）+00:00
  return date.toISOString().replace(/\.\d{3}Z$/, (m) => `${m.slice(0, 4)}000+00:00`);
}

/** 任意值 → 文本（工具输出的字符串化） */
function stringifyValue(v: unknown): string {
  let s: string;
  if (typeof v === "string") s = v;
  else s = JSON.stringify(v, null, 2);
  // 源数据偶含 unicode 替换符（U+FFFD，二进制解码问题），替换为可读字符
  return s.replace(/\uFFFD/g, "?");
}

/** 常见无信息量问候语（agent 自动化测试会话的首条消息，如 "say hello"） */
const GREETING_RE =
  /^(say hello|saying hello|say hi|hello|hi|hey|hi there|hello there|greeting|greetings|你好|您好|嗨|哈喽|说一句话|随便说一句|来一句|打个招呼|test|测试|good morning|good evening)[.!。！~～]*$/i;

/** 用会话消息生成标题（对齐 opencode 的 "New session - ..." 占位标题无意义） */
function deriveTitle(title: string | null, userTexts: string[]): string {
  const t = (title ?? "").trim();
  if (t && !t.startsWith("New session")) return t;
  // 优先取第一条有信息量的用户消息（长度 >= 8 且非纯问候，避免 2119 个 "say hello"）
  const meaningful = userTexts.find((s) => {
    const norm = s.trim().replace(/\s+/g, " ");
    return norm.length >= 8 && !GREETING_RE.test(norm);
  });
  if (meaningful) {
    const norm = meaningful.trim().replace(/\s+/g, " ");
    return norm.length > 60 ? norm.slice(0, 60) + "…" : norm;
  }
  // 无有信息量的用户消息（纯问候/空会话）：保留 opencode 原始 title
  // （"New session - <时间戳>" 含唯一时间戳，比 "say hello" 更有区分度）
  return t || "(无标题)";
}

// ─── opencode 数据读取 ─────────────────────────────────────────────────────────

interface SrcMessage {
  id: string;
  sessionId: string;
  timeCreated: number;
  role: string;
  data: any;
}

interface SrcPart {
  messageId: string;
  timeCreated: number;
  type: string;
  data: any;
}

/** 读取 opencode 源库（只读）。返回 messages/parts 的扁平列表 */
function readOpencode(db: DatabaseSync, opts: Options) {
  // project id → worktree 映射（cwd 的兜底）
  const worktreeByProject = new Map<string, string>();
  for (const row of db.prepare("SELECT id, worktree FROM project").all() as any[]) {
    if (row.worktree) worktreeByProject.set(row.id, row.worktree);
  }

  const sessionRows = db
    .prepare(
      `SELECT id, parent_id, title, directory, project_id,
              time_created, time_updated, agent, model
       FROM session ORDER BY time_created DESC`,
    )
    .all() as any[];

  // 会话选择：默认仅根 session（不含子 agent session）
  const selected = sessionRows.filter((s) => {
    if (!opts.includeSubagents && s.parent_id) return false;
    return true;
  });
  const limit = opts.limit ?? selected.length;
  const chosen = selected.slice(0, limit);
  const chosenIds = new Set(chosen.map((s) => s.id));

  // 消息与 part（仅选中 session）
  const messages: SrcMessage[] = (
    db
      .prepare(
        `SELECT id, session_id, time_created, data FROM message
         WHERE session_id IN (${chosenIds.size ? Array(chosenIds.size).fill("?").join(",") : "''"})
         ORDER BY time_created, id`,
      )
      .all(...chosenIds) as any[]
  ).map((r) => {
    // node:sqlite 返回 TEXT 列原始字符串，data 列是 JSON，需显式解析
    const data = JSON.parse(r.data);
    return {
      id: r.id,
      sessionId: r.session_id,
      timeCreated: r.time_created,
      role: data.role ?? "",
      data,
    };
  });

  const parts: SrcPart[] = (
    db
      .prepare(
        `SELECT message_id, time_created, data FROM part
         WHERE session_id IN (${chosenIds.size ? Array(chosenIds.size).fill("?").join(",") : "''"})
         ORDER BY time_created, id`,
      )
      .all(...chosenIds) as any[]
  ).map((r) => {
    const data = JSON.parse(r.data);
    return {
      messageId: r.message_id,
      timeCreated: r.time_created,
      type: data.type ?? "",
      data,
    };
  });

  return {
    sessions: chosen.map((s) => ({
      ...s,
      // cwd 与 opencode session.directory 字段对齐（原样保留，不做规范化）
      cwd: s.directory || worktreeByProject.get(s.project_id) || "",
    })),
    messages,
    parts,
  };
}

// ─── 迁移核心：opencode → peri 消息 ────────────────────────────────────────────

/**
 * 将 opencode 的 part 列表转换为 peri 的 ContentBlock 数组 + 独立 tool 结果列表。
 *
 * 对齐生产库（~/.peri/threads/threads.db）的消息结构：
 *   - assistant 消息 content 只含 tool_use block（不含结果），并同步带 tool_calls 字段
 *   - tool 执行结果作为独立的 role="tool" 消息（{"role":"tool","tool_call_id":...}）
 *
 * 映射规则（对齐 peri-acp-types ContentBlock 反序列化）：
 *   text       → {"type":"text","text":...}
 *   reasoning  → {"type":"reasoning","text":...}
 *   tool       → {"type":"tool_use","id","name","input"}；结果进入 toolResults
 *   step-start/step-finish/compaction/file/patch → 元数据，跳过
 */
function partsToBlocks(
  parts: SrcPart[],
): { blocks: any[]; toolResults: { callID: string; text: string; isError: boolean }[] } {
  const blocks: any[] = [];
  const toolResults: { callID: string; text: string; isError: boolean }[] = [];
  for (const p of parts) {
    const d = p.data;
    switch (p.type) {
      case "text": {
        if (typeof d.text === "string" && d.text.length > 0) {
          blocks.push({ type: "text", text: stringifyValue(d.text) });
        }
        break;
      }
      case "reasoning": {
        if (typeof d.text === "string" && d.text.length > 0) {
          blocks.push({ type: "reasoning", text: stringifyValue(d.text) });
        }
        break;
      }
      case "tool": {
        const state = d.state ?? {};
        const status = state.status ?? "";
        let isError = false;
        let resultText: string | null = null;
        if (status === "error" || state.error) {
          isError = true;
          resultText = stringifyValue(state.error ?? state.output);
        } else if (status === "completed" && state.output !== undefined) {
          resultText = stringifyValue(state.output);
        }
        // 无结果（running/pending/中断）：调用未完成，不迁移——
        // 避免产生没有对应 tool 结果的孤儿 tool_use
        if (resultText === null) break;
        const callID = d.callID ?? "";
        const name = d.tool ?? "unknown";
        const input = d.state?.input ?? {};
        blocks.push({ type: "tool_use", id: callID, name, input });
        toolResults.push({ callID, text: resultText, isError });
        break;
      }
      default:
        // step-start / step-finish / compaction / file / patch 等元数据 part 跳过
        break;
    }
  }
  return { blocks, toolResults };
}

/**
 * 单条消息：组装 BaseMessage JSON（对齐 BaseMessage 的 serde 格式）。
 * 对齐生产库：纯文本用字符串 content；assistant 有工具调用时带 tool_calls 字段
 * （{"id","name","arguments"}，arguments 为 input 对象）。
 */
function toBaseMessage(
  role: string,
  id: string,
  blocks: any[],
  toolCalls?: { id: string; name: string; arguments: unknown }[],
): string {
  // 对齐生产库：单块纯文本直接序列化为字符串，其余才用 blocks 数组
  let content: unknown = blocks;
  if (blocks.length === 1 && blocks[0].type === "text") {
    content = blocks[0].text;
  }
  const msg: any = { role, id, content };
  if (toolCalls && toolCalls.length > 0) msg.tool_calls = toolCalls;
  return JSON.stringify(msg);
}

// ─── peri schema（与 sqlite_store.rs init_schema 保持一致）────────────────────

const SCHEMA_SQL = [
  `CREATE TABLE IF NOT EXISTS threads (
      id          TEXT PRIMARY KEY,
      title       TEXT,
      cwd         TEXT NOT NULL DEFAULT '',
      created_at  TEXT NOT NULL,
      updated_at  TEXT NOT NULL,
      message_count INTEGER NOT NULL DEFAULT 0
  )`,
  `CREATE TABLE IF NOT EXISTS messages (
      message_id  TEXT PRIMARY KEY,
      thread_id   TEXT NOT NULL,
      role        TEXT NOT NULL,
      content     TEXT NOT NULL,
      FOREIGN KEY (thread_id) REFERENCES threads(id) ON DELETE CASCADE
  )`,
  `CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages (thread_id ASC)`,
  `ALTER TABLE threads ADD COLUMN parent_thread_id TEXT`,
  `ALTER TABLE threads ADD COLUMN snapshot_at_message_id TEXT`,
  `ALTER TABLE threads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT 0`,
  `ALTER TABLE threads ADD COLUMN cancel_policy TEXT NOT NULL DEFAULT 'cascade'`,
  `ALTER TABLE threads ADD COLUMN config TEXT`,
  `ALTER TABLE threads ADD COLUMN cached_context TEXT`,
  `ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active'`,
  `ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0`,
  `ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0`,
  `ALTER TABLE messages ADD COLUMN projection TEXT`,
  `ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0`,
];

// ─── 主流程 ────────────────────────────────────────────────────────────────────

function main() {
  const opts = parseArgs(process.argv.slice(2));

  // ── 安全保护 1：输出路径禁止落在 ~/.peri 下（生产库所在目录） ──
  const outAbs = resolve(opts.out);
  const periHome = resolve(join(homedir(), ".peri"));
  if (outAbs === periHome || outAbs.startsWith(periHome + "/")) {
    console.error(
      `拒绝执行：输出路径 ${outAbs} 位于 peri 生产数据目录 ${periHome} 下。\n` +
        `本脚本绝不写入生产数据库（~/.peri/threads/threads.db），请指定其他输出路径。`,
    );
    process.exit(1);
  }

  // ── 安全保护 2：输出文件已存在且非空 → 需 --force（--force 时删除旧文件重建） ──
  if (existsSync(outAbs) && statSync(outAbs).size > 0) {
    if (!opts.force) {
      console.error(
        `输出文件 ${outAbs} 已存在且非空。\n` +
          `为保护已有数据，请更换输出路径，或确认后使用 --force 覆盖。`,
      );
      process.exit(1);
    }
    unlinkSync(outAbs);
  }

  // ── 打开源库（只读） ──
  const srcDbPath = join(resolve(opts.src), "opencode.db");
  if (!existsSync(srcDbPath)) {
    console.error(`未找到 opencode 数据库: ${srcDbPath}\n请确认 --src 指向 opencode 数据目录（含 opencode.db）。`);
    process.exit(1);
  }
  let srcDb: DatabaseSync;
  try {
    srcDb = new DatabaseSync(srcDbPath, { readOnly: true });
  } catch (e) {
    console.error(
      `以只读模式打开源库失败: ${e}\n` +
        `可能是 WAL 文件（opencode.db-wal / opencode.db-shm）缺失或权限不足。\n` +
        `为保护源数据，脚本不会以读写模式打开源库，请修复后重试。`,
    );
    process.exit(1);
  }
  console.log(`源库（只读）: ${srcDbPath}`);

  const { sessions, messages, parts } = readOpencode(srcDb, opts);
  srcDb.close();

  // ── 组装数据 ──
  const partsByMessage = new Map<string, SrcPart[]>();
  for (const p of parts) {
    const list = partsByMessage.get(p.messageId) ?? [];
    list.push(p);
    partsByMessage.set(p.messageId, list);
  }
  const messagesBySession = new Map<string, SrcMessage[]>();
  for (const m of messages) {
    const list = messagesBySession.get(m.sessionId) ?? [];
    list.push(m);
    messagesBySession.set(m.sessionId, list);
  }

  interface ThreadRow {
    id: string;
    title: string;
    cwd: string;
    created_at: string;
    updated_at: string;
    message_count: number;
    parent_thread_id: string | null;
    hidden: number;
    cancel_policy: string;
    config: string | null;
    agent_status: string;
  }
  interface MessageRow {
    message_id: string;
    thread_id: string;
    role: string;
    content: string;
  }

  const threadRows: ThreadRow[] = [];
  const messageRows: MessageRow[] = [];
  let skippedEmptySessions = 0;
  let skippedEmptyMessages = 0;

  for (const s of sessions) {
    const msgs = messagesBySession.get(s.id) ?? [];
    if (msgs.length === 0 && !opts.includeEmpty) {
      skippedEmptySessions++;
      continue;
    }

    const threadMessages: MessageRow[] = [];
    const userTexts: string[] = [];
    for (const m of msgs) {
      if (m.role !== "user" && m.role !== "assistant") continue; // 仅迁移 user/assistant
      const { blocks, toolResults } = partsToBlocks(partsByMessage.get(m.id) ?? []);
      if (blocks.length === 0) {
        skippedEmptyMessages++;
        continue;
      }
      if (m.role === "user") {
        for (const b of blocks) {
          if (b.type === "text") userTexts.push(b.text);
        }
      }
      const messageId = uuidv5(m.id);
      // 对齐生产库：assistant 的 tool_use block 同步派生 tool_calls 字段
      const toolCalls = blocks
        .filter((b) => b.type === "tool_use")
        .map((b) => ({ id: b.id, name: b.name, arguments: b.input }));
      threadMessages.push({
        message_id: messageId,
        thread_id: s.id,
        role: m.role,
        content: toBaseMessage(
          m.role,
          messageId,
          blocks,
          m.role === "assistant" ? toolCalls : undefined,
        ),
      });
      // 对齐生产库：tool 执行结果作为独立的 role="tool" 消息
      for (const r of toolResults) {
        const toolId = uuidv5(`${m.id}:${r.callID}`);
        threadMessages.push({
          message_id: toolId,
          thread_id: s.id,
          role: "tool",
          content: JSON.stringify({
            role: "tool",
            id: toolId,
            tool_call_id: r.callID,
            content: r.text,
            is_error: r.isError,
          }),
        });
      }
    }

    if (threadMessages.length === 0 && !opts.includeEmpty) {
      skippedEmptySessions++;
      continue;
    }

    const configParts: Record<string, unknown> = { opencode: true };
    if (s.agent) configParts.agent = s.agent;
    if (s.model) configParts.model = s.model;

    threadRows.push({
      id: s.id,
      title: deriveTitle(s.title, userTexts),
      cwd: s.cwd || "",
      created_at: toRfc3339(s.time_created),
      updated_at: toRfc3339(s.time_updated),
      message_count: threadMessages.length,
      parent_thread_id: s.parent_id ?? null,
      hidden: s.parent_id ? 1 : 0,
      cancel_policy: "cascade",
      config: JSON.stringify(configParts),
      agent_status: "done",
    });
    messageRows.push(...threadMessages);
  }

  // ── 写入输出库（单事务） ──
  const outDb = new DatabaseSync(outAbs);
  for (const sql of SCHEMA_SQL) {
    try {
      outDb.exec(sql);
    } catch (e: any) {
      // ALTER TABLE 在列已存在时抛出 duplicate column，幂等忽略
      if (!/duplicate column/i.test(String(e?.message ?? e))) throw e;
    }
  }

  outDb.exec("BEGIN");
  try {
    const insThread = outDb.prepare(
      `INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
         parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
         cached_context, agent_status, context_cache_epoch)
       VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, NULL, ?, 0)`,
    );
    for (const t of threadRows) {
      insThread.run(
        t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
        t.parent_thread_id, t.hidden, t.cancel_policy, t.config, t.agent_status,
      );
    }
    const insMessage = outDb.prepare(
      `INSERT INTO messages (message_id, thread_id, role, content) VALUES (?, ?, ?, ?)`,
    );
    for (const m of messageRows) {
      insMessage.run(m.message_id, m.thread_id, m.role, m.content);
    }
    outDb.exec("COMMIT");
  } catch (e) {
    outDb.exec("ROLLBACK");
    throw e;
  }
  outDb.close();

  // ── 统计输出 ──
  const skipped = [];
  if (skippedEmptySessions) skipped.push(`${skippedEmptySessions} 个空 session`);
  if (skippedEmptyMessages) skipped.push(`${skippedEmptyMessages} 条空消息`);
  console.log(`输出库: ${outAbs}`);
  console.log(`迁移完成: ${threadRows.length} 个 session, ${messageRows.length} 条消息`);
  if (skipped.length) console.log(`已跳过: ${skipped.join(", ")}`);
  if (opts.force && existsSync(outAbs)) console.log("（--force 已覆盖原有文件）");
}

main();
