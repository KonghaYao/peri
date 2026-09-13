import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { DataLoader, DEFAULT_DB_PATH, NORMALIZER_VERSION } from "../../../src/data/loader.js";
import { exportTaskPacket } from "../../../src/research/task-packets.js";
import type { NormalizedMessage, ThreadRow } from "../../../src/data/types.js";

const WINDOW = { since: "2026-08-30T00:00:00Z", until: "2026-09-14T00:00:00Z" };
const INVENTORY_SNAPSHOT = "inventory: DataLoader.withSnapshot outer transaction";
const PAGE_SNAPSHOT = "pages: same outer DataLoader.withSnapshot transaction as inventory";
const PACKET_SNAPSHOT = "packet: independent exportTaskPacket DataLoader.withSnapshot per root";
const DEFAULT_CASES: Array<{ adlcId: string; rootIds: string[]; target: string; reason: string }> = [
  { adlcId: "2026-09-11-sandbox-mcp-server", rootIds: ["01a0906e-2489-7702-8198-87eee6bcce6e", "01a095cd-aff2-7b61-b640-72437dd85ed5"], target: "sandbox MCP 独立 server；用户 pivot 后继续收口", reason: "同一 structured adlcId 跨两个根会话续跑，必须合并为一案" },
  { adlcId: "2026-09-09-micro-compact-tool-input-p0", rootIds: ["01a08429-fe60-7b72-8f2a-6dcf13d6728d"], target: "Micro Compact tool-input P0", reason: "SkillTool + 用户调用 + 多次 Workflow 启动" },
  { adlcId: "2026-09-04-tui-long-markdown-streaming-cpu", rootIds: ["01a06bb5-eb91-72c3-948e-961cfabbe1e3"], target: "TUI 长 Markdown 流式渲染 CPU", reason: "SkillTool + 用户调用 + discovery/delivery/assessment Workflow" },
  { adlcId: "2026-09-02-steer-mq-loop-exit", rootIds: ["01a05fc2-7d69-7fe2-85f1-4995a4233bda"], target: "MQ loop exit 修复", reason: "SkillTool + 用户调用 + Workflow；含明确 preflight 错误" },
  { adlcId: "2026-09-04-meta-session", rootIds: ["01a069fd-581c-75b0-9e9a-9b7e3eefb7c7"], target: "meta-session 机制对照", reason: "任务目录与续跑/子会话证据可见，但该根的 Workflow 调用不在当前归一化消息中，保留缺口" },
  { adlcId: "2026-09-08-system-reminder-protocol", rootIds: ["01a07ed4-ee0d-7460-9063-7008f4ecd0b9"], target: "System Reminder 协议收口机制对照", reason: "任务目录与多个 child 可见；根消息没有可核对 Workflow 调用，保留为对照而非强实际执行样本" },
];

type Args = { db: string; out: string; includePages: boolean; maxBytes: number; maxMessages: number };
type Evidence = { skillCalls: number; explicitUserInvocations: number; workflowCalls: number; workflowErrors: number; workflowResultCount: number; runIds: string[]; adlcIds: string[]; workflowNames: string[]; firstEvidenceSequence: number | null };
type CaseSpec = typeof DEFAULT_CASES[number];

function parseArgs(argv: string[]): Args {
  const values = new Map<string, string>();
  const allowed = new Set(["--db", "--out", "--no-pages", "--max-bytes", "--max-messages"]);
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i]!;
    if (!allowed.has(flag)) throw new Error(`unknown argument: ${flag}`);
    if (flag === "--no-pages") { values.set(flag, "true"); continue; }
    if (!flag.startsWith("--") || !argv[i + 1] || argv[i + 1]!.startsWith("--")) throw new Error(`missing value for ${flag}`);
    values.set(flag, argv[++i]!);
  }
  const db = resolve(values.get("--db") ?? process.env.PERI_RESEARCH_DB ?? DEFAULT_DB_PATH);
  if (!values.has("--out")) throw new Error("--out is required and must be explicit");
  const out = resolve(values.get("--out")!);
  const maxBytes = Number(values.get("--max-bytes") ?? 1048576);
  const maxMessages = Number(values.get("--max-messages") ?? 1000);
  if (!existsSync(db)) throw new Error(`database does not exist: ${db}`);
  if (!Number.isInteger(maxBytes) || maxBytes < 1 || maxBytes > 1048576) throw new Error("--max-bytes must be 1..1048576");
  if (!Number.isInteger(maxMessages) || maxMessages < 1 || maxMessages > 1000) throw new Error("--max-messages must be 1..1000");
  return { db, out, includePages: !values.has("--no-pages"), maxBytes, maxMessages };
}

function assertFreshOutput(out: string): void {
  for (const name of ["manifest.json", "inventory.json", "cases"]) {
    if (existsSync(join(out, name))) throw new Error(`refusing to overwrite existing ${join(out, name)}`);
  }
}

function sha256(value: string | Buffer): string { return createHash("sha256").update(value).digest("hex"); }
function stable(value: unknown): string { return JSON.stringify(value, (_key, item) => item && typeof item === "object" && !Array.isArray(item) ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b))) : item); }
function object(value: unknown): Record<string, unknown> | null { return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null; }
function stringify(value: unknown): string { try { return JSON.stringify(value); } catch { return String(value); } }
function collectKeys(value: unknown, keyNames: Set<string>, out: string[] = []): string[] {
  if (Array.isArray(value)) { for (const item of value) collectKeys(item, keyNames, out); return out; }
  const item = object(value); if (!item) return out;
  for (const [key, child] of Object.entries(item)) {
    if (keyNames.has(key) && typeof child === "string") out.push(child);
    collectKeys(child, keyNames, out);
  }
  return out;
}
function workflowCall(call: { name: string; arguments: unknown }): boolean {
  if (call.name === "Workflow") return true;
  if (call.name !== "ExecuteExtraTool") return false;
  // ExecuteExtraTool is a wrapper in persisted messages. Count it as a
  // Workflow invocation only when the wrapper's real tool_name says so;
  // RunPtcCode/other extra tools may merely mention an ADLC task path.
  return collectKeys(call.arguments, new Set(["tool_name"])).some((value) => value === "Workflow");
}
function adlcIdsFromCall(call: { name: string; arguments: unknown }): string[] {
  const values = collectKeys(call.arguments, new Set(["adlcId", "adlc_id"]));
  const taskRoots = collectKeys(call.arguments, new Set(["adlcTaskRoot"]));
  const ids = [...values, ...taskRoots.map((v) => v.match(/(?:^|\/)tasks\/(\d{4}-\d{2}-\d{2}-[^/]+)/)?.[1] ?? "")];
  return [...new Set(ids.filter(Boolean))];
}
function runIdsFromResult(content: string): string[] {
  const ids: string[] = [];
  for (const match of content.matchAll(/(?:"run[_ ]?id"|\brun_id|\brunId)\s*[:=]\s*["']?([0-9a-f]{8}-[0-9a-f-]{27,})/gi)) ids.push(match[1]!);
  return [...new Set(ids)];
}
function evidenceFor(messages: NormalizedMessage[]): Evidence {
  const results = new Map(messages.flatMap((m) => m.results.map((r) => [r.id, r] as const)));
  let skillCalls = 0, explicitUserInvocations = 0, workflowCalls = 0, workflowErrors = 0, workflowResultCount = 0;
  const runIds: string[] = [], adlcIds: string[] = [], workflowNames: string[] = [];
  let firstEvidenceSequence: number | null = null;
  for (const message of messages) {
    if (message.role === "user" && /(?:^|\s)\/ultra-adlc(?:\s|$)/i.test(message.text)) { explicitUserInvocations++; firstEvidenceSequence ??= message.sequence; }
    for (const call of message.calls) {
      const raw = stringify(call.arguments);
      if (call.name === "SkillTool" && /ultra-adlc/i.test(raw)) { skillCalls++; firstEvidenceSequence ??= message.sequence; }
      if (!workflowCall(call)) continue;
      workflowCalls++; firstEvidenceSequence ??= message.sequence;
      const ids = adlcIdsFromCall(call); adlcIds.push(...ids);
      const values = collectKeys(call.arguments, new Set(["name"]));
      workflowNames.push(...values.filter((v) => /^adlc:/i.test(v)));
      const result = results.get(call.id);
      if (result) { workflowResultCount++; if (result.isError === true) workflowErrors++; runIds.push(...runIdsFromResult(result.content)); }
    }
  }
  return { skillCalls, explicitUserInvocations, workflowCalls, workflowErrors, workflowResultCount, runIds: [...new Set(runIds)], adlcIds: [...new Set(adlcIds)], workflowNames: [...new Set(workflowNames)], firstEvidenceSequence };
}
function pageMessage(message: NormalizedMessage): Record<string, unknown> {
  return { messageId: message.messageId, threadId: message.threadId, sequence: message.sequence, origin: message.origin, role: message.role, text: message.text, calls: message.calls.map((call) => ({ id: call.id, name: call.name, source: call.sources.join(","), arguments: call.arguments })), results: message.results.map((result) => ({ id: result.id, source: result.sources.join(","), isError: result.isError, content: result.content })), excludedFromContext: message.excludedFromContext, truncated: message.truncated, parseIssues: message.parseIssues };
}
function exportPages(out: string, threadId: string, messages: NormalizedMessage[]): { pageCount: number; pageHashes: string[]; messageIds: string[]; sourceMessageCount: number; omittedMessages: number; exportTruncated: boolean; snapshot: string } {
  const dir = join(out, "pages", threadId); mkdirSync(dir, { recursive: true });
  const pageHashes: string[] = [];
  for (let offset = 0, page = 1; offset < messages.length; offset += 50, page++) {
    const rows = messages.slice(offset, offset + 50).map(pageMessage);
    const payload = { schemaVersion: 1, threadId, page, offset, limit: 50, messages: rows, coverage: { sourceMessageCount: messages.length, firstSequence: messages[offset]?.sequence ?? null, lastSequence: messages[offset + rows.length - 1]?.sequence ?? null, omittedMessages: 0, exportTruncated: false, contentFieldsEmittedOnce: true } };
    const bytes = JSON.stringify(payload, null, 2) + "\n"; const hash = sha256(bytes); pageHashes.push(hash);
    writeFileSync(join(dir, `page-${String(page).padStart(4, "0")}.json`), bytes);
  }
  return { pageCount: pageHashes.length, pageHashes, messageIds: messages.map((message) => message.messageId), sourceMessageCount: messages.length, omittedMessages: 0, exportTruncated: false, snapshot: PAGE_SNAPSHOT };
}
function assertPacketPageAgreement(packet: { messages: Array<{ messageId: string }>; coverage: { sourceMessageCount: number; exportedMessageCount: number; omittedMessages: number; exportTruncated: boolean } }, messages: NormalizedMessage[], pages: { messageIds: string[]; sourceMessageCount: number; omittedMessages: number; exportTruncated: boolean }, threadId: string): void {
  const sourceIds = messages.map((message) => message.messageId);
  const packetIds = packet.messages.map((message) => message.messageId);
  if (packet.coverage.sourceMessageCount !== sourceIds.length || pages.sourceMessageCount !== sourceIds.length) throw new Error(`snapshot coverage sourceMessageCount mismatch for ${threadId}: packet=${packet.coverage.sourceMessageCount} pages=${pages.sourceMessageCount} source=${sourceIds.length}`);
  if (packet.coverage.exportedMessageCount !== packetIds.length) throw new Error(`packet exportedMessageCount mismatch for ${threadId}: coverage=${packet.coverage.exportedMessageCount} ids=${packetIds.length}`);
  let sourceIndex = 0;
  for (const id of packetIds) {
    const found = sourceIds.indexOf(id, sourceIndex);
    if (found < sourceIndex) throw new Error(`packet message IDs are not an ordered subset of page/inventory IDs for ${threadId}`);
    sourceIndex = found + 1;
  }
  if (pages.messageIds.length !== sourceIds.length || pages.messageIds.some((id, index) => id !== sourceIds[index]) || pages.omittedMessages !== 0 || pages.exportTruncated) throw new Error(`page message IDs/coverage differ from inventory snapshot for ${threadId}`);
  // A bounded packet may omit messages or truncate content. This check only
  // proves ordered ID/count agreement across independently opened snapshots;
  // it does not claim identical content or a single point-in-time snapshot.
}
function threadMeta(thread: ThreadRow, ownMessageCount: number, child: boolean): Record<string, unknown> { return { threadId: thread.id, parentThreadId: thread.parent_thread_id, title: thread.title, cwd: thread.cwd, createdAt: thread.created_at, updatedAt: thread.updated_at, hidden: thread.hidden !== 0, child, storedMessageCount: thread.message_count, normalizedOwnMessageCount: ownMessageCount }; }

const args = parseArgs(process.argv.slice(2));
assertFreshOutput(args.out);
mkdirSync(join(args.out, "cases"), { recursive: true });
const loader = new DataLoader(args.db);
const all: Record<string, unknown>[] = [];
const caseRows: Record<string, unknown>[] = [];
try {
  const snapshot = loader.withSnapshot((db) => {
    const roots = db.loadVisibleMainThreads().filter((thread) => { const time = Date.parse(thread.created_at); return Number.isFinite(time) && time >= Date.parse(WINDOW.since) && time < Date.parse(WINDOW.until); });
    const selectedIds = new Set(DEFAULT_CASES.flatMap((spec) => spec.rootIds));
    for (const root of roots) {
      const messages = [...db.loadInheritedMessages(root.id), ...db.loadNormalizedMessages(root.id)];
      const own = messages.filter((message) => message.origin === "own");
      const evidence = evidenceFor(messages);
      all.push({ ...threadMeta(root, own.length, false), evidence, selection: selectedIds.has(root.id) ? "selected" : evidence.skillCalls || evidence.workflowCalls || evidence.explicitUserInvocations ? "candidate_or_excluded" : "no_adlc_evidence" });
    }
    for (const spec of DEFAULT_CASES) {
      const rootsForCase: ThreadRow[] = [];
      const rowsById = new Map(db.loadThreadsByIds(spec.rootIds).map((thread) => [thread.id, thread]));
      for (const rootId of spec.rootIds) {
        const root = rowsById.get(rootId);
        if (!root) throw new Error(`selected root does not exist: ${spec.adlcId}/${rootId}`);
        if (root.hidden !== 0) throw new Error(`selected root is not visible: ${spec.adlcId}/${rootId}`);
        if (root.parent_thread_id !== null) throw new Error(`selected root has a parent: ${spec.adlcId}/${rootId}/${root.parent_thread_id}`);
        const createdAt = Date.parse(root.created_at);
        if (!Number.isFinite(createdAt) || createdAt < Date.parse(WINDOW.since) || createdAt >= Date.parse(WINDOW.until)) throw new Error(`selected root is outside sampling window: ${spec.adlcId}/${rootId}`);
        if (!roots.some((candidate) => candidate.id === rootId)) throw new Error(`selected root is not in visible window roots: ${spec.adlcId}/${rootId}`);
        rootsForCase.push(root);
      }
      const caseDir = join(args.out, "cases", spec.adlcId); mkdirSync(caseDir, { recursive: true });
      const rootRows: Record<string, unknown>[] = [];
      for (const root of rootsForCase) {
        const packet = exportTaskPacket(args.db, root.id, { includeContent: true, maxBytes: args.maxBytes, maxMessages: args.maxMessages });
        const messages = [...db.loadInheritedMessages(root.id), ...db.loadNormalizedMessages(root.id)];
        const ownCount = messages.filter((message) => message.origin === "own").length;
        const evidence = evidenceFor(messages);
        const children = db.loadSubAgents(root.id).map((child) => { const childMessages = [...db.loadInheritedMessages(child.id), ...db.loadNormalizedMessages(child.id)]; return threadMeta(child, childMessages.filter((message) => message.origin === "own").length, true); });
        const sourceView = { messageIds: messages.map((message) => message.messageId), sourceMessageCount: messages.length, omittedMessages: 0, exportTruncated: false };
        assertPacketPageAgreement(packet, messages, sourceView, root.id);
        const pages = args.includePages ? exportPages(caseDir, root.id, messages) : { pageCount: 0, pageHashes: [], messageIds: messages.map((message) => message.messageId), sourceMessageCount: messages.length, omittedMessages: 0, exportTruncated: false, snapshot: "pages disabled by --no-pages; inventory source IDs used for agreement" };
        assertPacketPageAgreement(packet, messages, pages, root.id);
        const packetPath = join(caseDir, `packet-${root.id}.json`); writeFileSync(packetPath, JSON.stringify(packet, null, 2) + "\n");
        rootRows.push({ root: threadMeta(root, ownCount, false), children, evidence, packet: { path: `cases/${spec.adlcId}/packet-${root.id}.json`, sha256: sha256(readFileSync(packetPath)), packetHash: packet.packetHash, coverage: packet.coverage, snapshot: PACKET_SNAPSHOT }, pages });
      }
      caseRows.push({ caseId: spec.adlcId, target: spec.target, selectionReason: "人工目的抽样/弱Workflow筛选", selectionDetail: spec.reason, rootThreads: rootRows, associatedAdlcIds: [spec.adlcId], physicalRunCount: null, physicalRunCountNote: "not read from task files; Workflow result run IDs below are only IDs persisted in normalized tool results", workflowRunIds: [...new Set(rootRows.flatMap((row) => ((row.evidence as Evidence).runIds)))], workflowCallCount: rootRows.reduce((sum, row) => sum + (row.evidence as Evidence).workflowCalls, 0), explicitWorkflowErrors: rootRows.reduce((sum, row) => sum + (row.evidence as Evidence).workflowErrors, 0) });
    }
    return { rootCount: roots.length };
  });
  const dbStat = statSync(args.db);
  const inventory = { schemaVersion: 1, purpose: "ADLC efficiency research inventory; observations only, no outcome judgment", window: { ...WINDOW, interval: "[since, until)", timeBasis: "visible root thread created_at" }, scope: { visibleRoots: true, allCwds: true, excludedHiddenRoots: true, actualExecutionRule: "actual only when normalized evidence has SkillTool ultra-adlc and/or explicit user /ultra-adlc, with Workflow/ExecuteExtraTool ADLC association required for strong execution; documentation-only or skill-maintenance mentions remain excluded_or_weak", sameAdlcIdPolicy: "structured adlcId links multiple root sessions into one case" }, counts: { rootsInWindow: all.length, selectedCases: caseRows.length, selectedRoots: caseRows.reduce((sum, item) => sum + (item.rootThreads as unknown[]).length, 0), candidatesWithAnyAdlcSignal: all.filter((item) => { const e = item.evidence as Evidence; return e.skillCalls || e.workflowCalls || e.explicitUserInvocations; }).length }, selectionNote: "人工目的抽样/弱Workflow筛选；保留机制对照，不表示排名算法或窗口内实际 ADLC 总数", roots: all, source: { database: args.db, databaseSizeBytes: dbStat.size, databaseMtimeMs: dbStat.mtimeMs, normalizer: NORMALIZER_VERSION, schemaIdentity: sha256(stable(loader.capabilities)), snapshot: INVENTORY_SNAPSHOT, snapshotStatements: snapshot.statements, packetSnapshot: PACKET_SNAPSHOT, pageSnapshot: PAGE_SNAPSHOT } };
  writeFileSync(join(args.out, "inventory.json"), JSON.stringify(inventory, null, 2) + "\n");
  const manifest = { schemaVersion: 1, purpose: "Frozen ADLC long-process history cases for review", window: { ...WINDOW, interval: "[since, until)", timeBasis: "visible root thread created_at" }, selection: { candidateCount: all.filter((item) => { const e = item.evidence as Evidence; return e.skillCalls || e.workflowCalls || e.explicitUserInvocations; }).length, selectedCaseCount: caseRows.length, selectionNote: "人工目的抽样/弱Workflow筛选；不表示排名算法或窗口内实际 ADLC 总数", selectedCaseIds: caseRows.map((item) => item.caseId) }, timeFieldSemantics: { createdAt: "thread eligibility/window boundary", updatedAt: "thread activity metadata only", sequence: "normalized persisted row order used for pagination", ownMessageCount: "normalized messages origin=own; inherited context excluded from own count" }, source: { database: args.db, normalizer: NORMALIZER_VERSION, schemaIdentity: sha256(stable(loader.capabilities)), inventorySnapshot: INVENTORY_SNAPSHOT, pageSnapshot: PAGE_SNAPSHOT, packetSnapshot: PACKET_SNAPSHOT, exportScript: "export-adlc-cases.ts", exportScriptSha256: sha256(readFileSync(new URL(import.meta.url))), packetLimits: { maxBytes: args.maxBytes, maxMessages: args.maxMessages }, pages: { enabled: args.includePages, maxMessagesPerPage: 50, contentFieldsEmittedOnce: true } }, omissionsAndTruncation: { packet: "exportTaskPacket coverage fields are authoritative; source/export truncation and omitted messages/fields are retained per packet", pages: "each normalized message appears in exactly one page; no page-level omission/truncation unless explicitly marked", runMetadata: "physical task-file run totals not read here; only normalized Workflow result run IDs and explicit isError are recorded; null means unavailable" }, cases: caseRows };
  writeFileSync(join(args.out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(JSON.stringify({ output: args.out, selectedCases: caseRows.map((item) => ({ caseId: item.caseId, roots: (item.rootThreads as unknown[]).length, ownMessages: (item.rootThreads as Array<{ root: { normalizedOwnMessageCount: number } }>).reduce((sum, item) => sum + item.root.normalizedOwnMessageCount, 0) })), inventory: join(args.out, "inventory.json"), manifest: join(args.out, "manifest.json") }, null, 2));
} finally { loader.close(); }
