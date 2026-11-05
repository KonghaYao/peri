#!/usr/bin/env bun
/**
 * 回填 peri-cf sandbox 会话的占位标题「新会话」。
 *
 * 默认 dry-run：只读候选与首条用户消息，打印拟写入的标题；显式传 `--apply` 才执行
 * CAS UPDATE。连接信息与 server 相同：PERI_STORAGE_URL / PERI_STORAGE_TOKEN
 * （在 npm-packages/peri-cf 下运行，Bun 会自动读取 .env）。
 *
 * 标题规则与 Store 的首条用户消息自动标题一致
 * （peri-resources/src/sessions/canonical.rs:418-448）：
 * 取最早一条有文本的 user 消息，blocks 以空格拼接，按 Unicode 码点截取前 50 字，
 * 不 trim、不摘要。只回填 cwd 位于 `.peri-sandbox/sandboxes/` 的会话，
 * 排除 /Users/mino 等非 peri-cf 历史行。
 */

const PLACEHOLDER = "新会话";
const SANDBOX_CWD_LIKE = "%/.peri-sandbox/sandboxes/%";
const TITLE_MAX_CHARS = 50;

type JsonRecord = Record<string, unknown>;

interface HranaValue {
  type: string;
  value?: string | null;
  base64?: string;
}

interface HranaExecuteResult {
  cols: { name: string }[];
  rows: HranaValue[][];
  affected_row_count: number;
}

type HranaResult =
  | { type: "ok"; response: { type: string; result?: HranaExecuteResult } }
  | { type: "error"; error: { message: string } };

interface Candidate {
  id: string;
  cwd: string;
}

interface Proposal {
  candidate: Candidate;
  title: string;
}

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function fail(message: string): never {
  console.error(`回填失败：${message}`);
  process.exit(1);
}

function textArg(value: string): HranaValue {
  return { type: "text", value };
}

function storageConfig(): { url: string; token: string } {
  const rawUrl = process.env.PERI_STORAGE_URL?.trim();
  const token = process.env.PERI_STORAGE_TOKEN;
  if (!rawUrl) {
    fail("缺少 PERI_STORAGE_URL（与 server 相同；可在 npm-packages/peri-cf/.env 中配置）。");
  }
  if (!token) {
    fail("缺少 PERI_STORAGE_TOKEN（与 server 相同；脚本不会打印该值）。");
  }
  const url = rawUrl.replace(/\/+$/, "");
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    fail("PERI_STORAGE_URL 不是合法 URL。");
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    fail("PERI_STORAGE_URL 必须是 http(s) 地址。");
  }
  return { url, token };
}

/** 只展示端点本身，去掉可能含凭据的 userinfo / query / hash。 */
function displayEndpoint(url: string): string {
  const parsed = new URL(url);
  parsed.username = "";
  parsed.password = "";
  parsed.search = "";
  parsed.hash = "";
  return parsed.toString();
}

async function pipeline(
  config: { url: string; token: string },
  requests: unknown[],
): Promise<HranaResult[]> {
  let response: Response;
  try {
    response = await fetch(`${config.url}/v2/pipeline`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${config.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ requests }),
    });
  } catch (error) {
    fail(
      `无法访问 Store ${displayEndpoint(config.url)}：` +
        (error instanceof Error ? error.message : String(error)),
    );
  }
  if (!response.ok) {
    fail(`Store ${displayEndpoint(config.url)} 返回 HTTP ${response.status}。`);
  }
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    fail("Store 响应不是合法 JSON。");
  }
  if (!isRecord(body) || !Array.isArray(body.results)) {
    fail("Store 响应缺少 results（不是 Hrana /v2/pipeline 格式）。");
  }
  return body.results as HranaResult[];
}

function executeResult(result: HranaResult, label: string): HranaExecuteResult {
  if (result.type === "error") fail(`${label}：${result.error.message}`);
  if (result.type !== "ok" || result.response.type !== "execute" || !result.response.result) {
    fail(`${label}：Store 返回了非 execute 结果。`);
  }
  return result.response.result;
}

function textCell(value: HranaValue | undefined): string | null {
  if (!value || value.type !== "text") return null;
  return value.value ?? null;
}

/** canonical.rs `extract_title` 的单条消息规则：Text 直取；Blocks 只拼 text 块（空格分隔）。 */
function contentText(content: unknown): string | null {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return null;
  const parts: string[] = [];
  for (const block of content) {
    if (isRecord(block) && block.type === "text" && typeof block.text === "string") {
      parts.push(block.text);
    }
  }
  return parts.join(" ");
}

/** 返回首条可用的 user 消息标题；空文本继续向后找，与 canonical 一致。 */
function titleFromMessages(rows: HranaValue[][]): string | null {
  for (const row of rows) {
    const raw = textCell(row[0]);
    if (raw === null) continue;
    let envelope: unknown;
    try {
      envelope = JSON.parse(raw);
    } catch {
      continue;
    }
    if (!isRecord(envelope) || envelope.type !== "message" || !isRecord(envelope.message)) {
      continue;
    }
    const text = contentText(envelope.message.content);
    if (text === null) continue;
    const title = Array.from(text).slice(0, TITLE_MAX_CHARS).join("");
    if (title.length > 0) return title;
  }
  return null;
}

function candidateQuery(): unknown {
  return {
    type: "execute",
    stmt: {
      sql: "SELECT id, cwd FROM threads WHERE title = ?1 AND cwd LIKE ?2 ORDER BY created_at ASC, id ASC",
      args: [textArg(PLACEHOLDER), textArg(SANDBOX_CWD_LIKE)],
      want_rows: true,
    },
  };
}

function messagesQuery(threadId: string): unknown {
  return {
    type: "execute",
    stmt: {
      sql: "SELECT content FROM messages WHERE thread_id = ?1 AND role = 'user' ORDER BY rowid ASC",
      args: [textArg(threadId)],
      want_rows: true,
    },
  };
}

function updateStatement(proposal: Proposal): unknown {
  return {
    type: "execute",
    stmt: {
      // CAS：期间被 rename / prediction 改写时不覆盖；不推进 updated_at，避免侧栏重排。
      sql: "UPDATE threads SET title = ?1 WHERE id = ?2 AND title = '新会话'",
      args: [textArg(proposal.title), textArg(proposal.candidate.id)],
      want_rows: false,
    },
  };
}

async function main(): Promise<void> {
  const apply = process.argv.includes("--apply");
  const config = storageConfig();
  console.log(`Store: ${displayEndpoint(config.url)}（${apply ? "apply" : "dry-run"}）`);

  const candidatesResult = await pipeline(config, [
    candidateQuery(),
    { type: "close" },
  ]);
  const candidateRows = executeResult(candidatesResult[0]!, "读取候选会话").rows;
  const candidates: Candidate[] = [];
  for (const row of candidateRows) {
    const id = textCell(row[0]);
    if (!id) continue;
    candidates.push({ id, cwd: textCell(row[1]) ?? "" });
  }
  console.log(`候选（title = '新会话' 且 cwd 在 sandbox 下）：${candidates.length}`);

  if (candidates.length === 0) {
    console.log(apply ? "没有可回填的候选，未写入。" : "dry-run：没有可回填的候选，未写入任何数据。");
    return;
  }

  const inspectResults = await pipeline(config, [
    ...candidates.map((candidate) => messagesQuery(candidate.id)),
    { type: "close" },
  ]);
  const proposals: Proposal[] = [];
  for (const [index, candidate] of candidates.entries()) {
    const rows = executeResult(inspectResults[index]!, `读取会话 ${candidate.id} 的用户消息`).rows;
    const title = titleFromMessages(rows);
    if (title === null) {
      console.log(`- ${candidate.id} → 跳过：没有可用的 user 文本`);
      continue;
    }
    proposals.push({ candidate, title });
    console.log(`- ${candidate.id} → ${JSON.stringify(title)}`);
  }

  if (!apply) {
    console.log(
      `dry-run：${proposals.length} 条拟回填，未写入任何数据；确认后显式传 --apply。`,
    );
    return;
  }
  if (proposals.length === 0) {
    console.log("没有可回填的候选，未写入。");
    return;
  }

  const updateResults = await pipeline(config, [
    ...proposals.map(updateStatement),
    { type: "close" },
  ]);
  let applied = 0;
  let missed = 0;
  for (const [index, proposal] of proposals.entries()) {
    const result = executeResult(updateResults[index]!, `更新会话 ${proposal.candidate.id}`);
    if (result.affected_row_count === 1) {
      applied += 1;
    } else {
      missed += 1;
      console.log(`- ${proposal.candidate.id} → CAS 未命中（标题已被并发修改），未覆盖`);
    }
  }
  console.log(`apply：更新 ${applied} 条，CAS 未命中 ${missed} 条。`);
}

await main();

export {};
