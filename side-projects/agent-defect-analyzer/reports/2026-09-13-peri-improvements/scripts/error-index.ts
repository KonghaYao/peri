import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { DataLoader, NORMALIZER_VERSION } from "../../../src/data/loader.js";
import { validateThreadFilter } from "../../../src/data/filters.js";
import type { NormalizedMessage, ThreadRow, ToolCall } from "../../../src/data/types.js";

const ANALYZER_ROOT = resolve(import.meta.dir, "../../..");
type Options = { since: string; until: string; cwdRoot: string; db: string; out: string; inheritedFromManifest: boolean; manifestSchemaIdentity: string };
type SelectionManifest = { schemaVersion?: unknown; window?: { since?: unknown; until?: unknown }; filters?: { scope?: unknown; includeHidden?: unknown; cwdRoot?: unknown }; source?: { database?: unknown; schemaIdentity?: unknown } };

function usage(): string { return "bun run reports/2026-09-13-peri-improvements/scripts/error-index.ts --out DIR [--db PATH] [--cwd-root PATH] [--since ISO] [--until ISO]"; }
function dbPath(raw: string): string { return resolve(raw.startsWith("~/") ? join(homedir(), raw.slice(2)) : raw); }
function parseArgs(argv: string[]): Options {
  const values = new Map<string, string>();
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === "--help") { console.log(usage()); process.exit(0); }
    if (!flag?.startsWith("--") || i + 1 >= argv.length || argv[i + 1]?.startsWith("--")) throw new Error(`missing value for ${flag ?? "argument"}\n${usage()}`);
    if (values.has(flag)) throw new Error(`duplicate argument: ${flag}`);
    values.set(flag, argv[++i]!);
  }
  const outValue = values.get("--out");
  if (!outValue) throw new Error(`--out is required\n${usage()}`);
  const allowed = new Set(["--out", "--db", "--cwd-root", "--since", "--until"]);
  for (const key of values.keys()) if (!allowed.has(key)) throw new Error(`unknown argument: ${key}`);
  const out = resolve(process.cwd(), outValue);
  if (!existsSync(join(out, "manifest.json"))) throw new Error(`selection manifest is missing from --out: ${out}`);
  if (existsSync(join(out, "error-index.json"))) throw new Error(`refusing to overwrite an existing error index: ${join(out, "error-index.json")}`);
  const manifest = JSON.parse(readFileSync(join(out, "manifest.json"), "utf8")) as SelectionManifest;
  const manifestSince = manifest.window?.since;
  const manifestUntil = manifest.window?.until;
  const manifestCwd = manifest.filters?.cwdRoot;
  const manifestDb = manifest.source?.database;
  if (manifest.schemaVersion !== 1 || manifest.filters?.scope !== "roots" || manifest.filters?.includeHidden !== false) throw new Error("selection manifest must be schemaVersion 1 with roots/includeHidden=false");
  const manifestSchemaIdentity = manifest.source?.schemaIdentity;
  if (typeof manifestSince !== "string" || typeof manifestUntil !== "string" || typeof manifestCwd !== "string" || typeof manifestDb !== "string" || typeof manifestSchemaIdentity !== "string") throw new Error("selection manifest lacks required window, cwd, source database, or schema identity");
  const manifestWindow = validateThreadFilter({ scope: "roots", includeHidden: false, since: manifestSince, until: manifestUntil });
  const explicitSince = values.get("--since");
  const explicitUntil = values.get("--until");
  const finalWindow = validateThreadFilter({ scope: "roots", includeHidden: false, since: explicitSince ?? manifestWindow.sinceText!, until: explicitUntil ?? manifestWindow.untilText! });
  if (explicitSince !== undefined && finalWindow.sinceText !== manifestWindow.sinceText) throw new Error(`--since conflicts with selection manifest window (${manifestWindow.sinceText})`);
  if (explicitUntil !== undefined && finalWindow.untilText !== manifestWindow.untilText) throw new Error(`--until conflicts with selection manifest window (${manifestWindow.untilText})`);
  const manifestCwdPath = resolve(process.cwd(), manifestCwd);
  const explicitCwd = values.get("--cwd-root");
  const cwdRoot = resolve(process.cwd(), explicitCwd ?? manifestCwdPath);
  if (explicitCwd !== undefined && cwdRoot !== manifestCwdPath) throw new Error(`--cwd-root conflicts with selection manifest (${manifestCwdPath})`);
  const manifestDbPath = dbPath(manifestDb);
  const explicitDb = values.get("--db");
  const db = explicitDb === undefined ? manifestDbPath : dbPath(explicitDb);
  if (explicitDb !== undefined && db !== manifestDbPath) throw new Error(`--db conflicts with selection manifest (${manifestDbPath})`);
  if (!existsSync(db)) throw new Error(`database does not exist: ${db}`);
  return { db, out, since: finalWindow.sinceText!, until: finalWindow.untilText!, cwdRoot, inheritedFromManifest: explicitSince === undefined && explicitUntil === undefined && explicitCwd === undefined && explicitDb === undefined, manifestSchemaIdentity };
}
function stable(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${stable(object[key])}`).join(",")}}`;
}
function hash(value: unknown): string { return createHash("sha256").update(stable(value)).digest("hex"); }
function effective(call: ToolCall): { outerName: string; effectiveName: string; label: string } {
  let effectiveName = call.name;
  if (call.name === "ExecuteExtraTool" && call.arguments && typeof call.arguments === "object" && !Array.isArray(call.arguments)) {
    const target = (call.arguments as Record<string, unknown>).tool_name;
    if (typeof target === "string" && target.length > 0) effectiveName = target;
  }
  return { outerName: call.name, effectiveName, label: call.name === effectiveName ? effectiveName : `${call.name}→${effectiveName}` };
}
function invalidIds(message: NormalizedMessage): { calls: Set<string>; results: Set<string> } {
  return { calls: new Set(message.parseIssues.flatMap((issue) => { const match = /^(?:conflictingToolCall|invalidArguments):(.+)$/.exec(issue); return match ? [match[1]] : []; })), results: new Set(message.parseIssues.flatMap((issue) => { const match = /^conflictingToolResult:(.+)$/.exec(issue); return match ? [match[1]] : []; })) };
}
function inWindow(thread: ThreadRow, options: Options): boolean { const time = Date.parse(thread.created_at); return time >= Date.parse(options.since) && time < Date.parse(options.until); }
function inRepo(thread: ThreadRow, options: Options): boolean { return thread.cwd === options.cwdRoot || thread.cwd.startsWith(`${options.cwdRoot}/`); }

const options = parseArgs(process.argv.slice(2));
validateThreadFilter({ scope: "roots", includeHidden: false, since: options.since, until: options.until });
const loader = new DataLoader(options.db);
const schemaIdentity = hash(loader.capabilities);
if (schemaIdentity !== options.manifestSchemaIdentity) { loader.close(); throw new Error("database schema identity conflicts with selection manifest"); }
let value: { threads: ThreadRow[]; errors: Array<Record<string, unknown>>; messages: number; duplicateCallIds: number; duplicateResultIds: number; explicitErrorResults: number; pairedExplicitErrors: number; orphanExplicitErrors: number; parseIssues: Record<string, number> };
try {
  value = loader.withSnapshot((db) => {
    const threads = db.loadVisibleMainThreads().filter((thread) => inWindow(thread, options) && inRepo(thread, options));
    const errors: Array<Record<string, unknown>> = [];
    const parseIssues: Record<string, number> = {};
    let messages = 0, duplicateCallIds = 0, duplicateResultIds = 0, explicitErrorResults = 0, pairedExplicitErrors = 0, orphanExplicitErrors = 0;
    for (const thread of threads) {
      const pending = new Map<string, { call: ToolCall; message: NormalizedMessage; names: ReturnType<typeof effective> }>();
      const seenCalls = new Set<string>();
      const seenResults = new Set<string>();
      for (const message of db.iterateNormalizedMessages(thread.id)) {
        messages++;
        for (const issue of message.parseIssues) { const category = issue.includes(":") ? issue.slice(0, issue.indexOf(":")) : issue; parseIssues[category] = (parseIssues[category] ?? 0) + 1; }
        const invalid = invalidIds(message);
        for (const call of message.calls) {
          if (invalid.calls.has(call.id)) continue;
          if (seenCalls.has(call.id)) { duplicateCallIds++; continue; }
          seenCalls.add(call.id); pending.set(call.id, { call, message, names: effective(call) });
        }
        for (const result of message.results) {
          if (invalid.results.has(result.id)) continue;
          if (seenResults.has(result.id)) { duplicateResultIds++; continue; }
          seenResults.add(result.id);
          const record = pending.get(result.id);
          if (result.isError === true) {
            explicitErrorResults++;
            if (record) pairedExplicitErrors++; else orphanExplicitErrors++;
            errors.push({ threadId: thread.id, messageId: message.messageId, callId: result.id, sequence: message.sequence, errorFirstLine: result.content.split(/\r?\n/, 1)[0] ?? "", errorText: result.content, pairing: record ? "paired" : "orphan", pairedTool: record ? { name: record.names.effectiveName, outerName: record.names.outerName, effectiveName: record.names.effectiveName, label: record.names.label, callMessageId: record.message.messageId, callSequence: record.message.sequence } : null });
          }
          pending.delete(result.id);
        }
      }
    }
    return { threads, errors, messages, duplicateCallIds, duplicateResultIds, explicitErrorResults, pairedExplicitErrors, orphanExplicitErrors, parseIssues };
  }).value;
} finally { loader.close(); }
const output = { schemaVersion: 1, purpose: "Explicit error-result index for Peri cwd visible root threads; no task outcome or root-cause judgments.", arguments: options, window: { since: options.since, until: options.until, interval: "[since, until)", timeBasis: "visible root thread created_at" }, filters: { scope: "roots", includeHidden: false, cwdRoot: options.cwdRoot, cwdPolicy: "thread.cwd === cwdRoot or startsWith(cwdRoot + '/')", threadCount: value.threads.length }, source: { database: options.db, normalizer: NORMALIZER_VERSION, schemaIdentity, snapshot: "readonly-transaction" }, pairing: { policy: "same normalized metrics pairing: ignore invalid IDs, first valid call/result ID wins, duplicate IDs counted and retained as index quality facts; explicit error results without a call remain orphan", duplicateCallIds: value.duplicateCallIds, duplicateResultIds: value.duplicateResultIds }, counts: { normalizedMessages: value.messages, explicitErrorResults: value.explicitErrorResults, pairedExplicitErrors: value.pairedExplicitErrors, orphanExplicitErrors: value.orphanExplicitErrors, parseIssueCategories: value.parseIssues }, errors: value.errors };
writeFileSync(join(options.out, "error-index.json"), JSON.stringify(output, null, 2) + "\n");
console.log(JSON.stringify({ output: join(options.out, "error-index.json"), threads: output.filters.threadCount, explicitErrorResults: output.counts.explicitErrorResults, pairedExplicitErrors: output.counts.pairedExplicitErrors, orphanExplicitErrors: output.counts.orphanExplicitErrors, duplicateCallIds: output.pairing.duplicateCallIds, duplicateResultIds: output.pairing.duplicateResultIds }, null, 2));
