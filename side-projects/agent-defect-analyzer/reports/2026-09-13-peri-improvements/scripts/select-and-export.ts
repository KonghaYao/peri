import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { DataLoader, NORMALIZER_VERSION } from "../../../src/data/loader.js";
import { validateThreadFilter } from "../../../src/data/filters.js";
import { exportTaskPacket } from "../../../src/research/task-packets.js";
import type { ThreadRow } from "../../../src/data/types.js";

const ANALYZER_ROOT = resolve(import.meta.dir, "../../..");
const REPO_ROOT = resolve(ANALYZER_ROOT, "../..");
const DEFAULT_DB = join(homedir(), ".peri/threads/threads.db");
const DEFAULT_PRIOR = join(ANALYZER_ROOT, "reports/2026-09-13-task-effectiveness/pilot-manifest.json");
const DEFAULTS = { since: "2026-09-06T00:00:00Z", until: "2026-09-13T00:00:00Z", seed: "peri-improvements-2026-09-13-v1", perStratum: 2, cwdRoot: REPO_ROOT };
const MAX_BYTES = 1048576;
const MAX_MESSAGES = 1000;
type Stratum = "short" | "medium" | "long";
type Candidate = { thread: ThreadRow; ownMessageCount: number };
type Options = typeof DEFAULTS & { db: string; out: string; priorManifest: string };

function usage(): string { return "bun run reports/2026-09-13-peri-improvements/scripts/select-and-export.ts --out DIR [--db PATH] [--cwd-root PATH] [--since ISO] [--until ISO] [--seed VALUE] [--per-stratum N] [--prior-manifest PATH]"; }
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
  const allowed = new Set(["--out", "--db", "--cwd-root", "--since", "--until", "--seed", "--per-stratum", "--prior-manifest"]);
  for (const key of values.keys()) if (!allowed.has(key)) throw new Error(`unknown argument: ${key}`);
  const perStratum = Number(values.get("--per-stratum") ?? DEFAULTS.perStratum);
  if (!Number.isInteger(perStratum) || perStratum < 1 || perStratum > 10) throw new Error("--per-stratum must be an integer between 1 and 10");
  const db = values.get("--db") ?? process.env.PERI_RESEARCH_DB ?? DEFAULT_DB;
  const out = resolve(process.cwd(), outValue);
  const priorManifest = resolve(process.cwd(), values.get("--prior-manifest") ?? DEFAULT_PRIOR);
  if (!existsSync(db)) throw new Error(`database does not exist: ${db}`);
  if (!existsSync(priorManifest)) throw new Error(`prior manifest does not exist: ${priorManifest}`);
  return { db, out, priorManifest, since: values.get("--since") ?? DEFAULTS.since, until: values.get("--until") ?? DEFAULTS.until, seed: values.get("--seed") ?? DEFAULTS.seed, perStratum, cwdRoot: resolve(process.cwd(), values.get("--cwd-root") ?? DEFAULTS.cwdRoot) };
}
function stable(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${stable(object[key])}`).join(",")}}`;
}
function sha256(value: string | Buffer): string { return createHash("sha256").update(value).digest("hex"); }
function fileSha256(path: string): string { return sha256(readFileSync(path)); }
function bucket(count: number): Stratum | "none" { return count < 1 ? "none" : count <= 20 ? "short" : count <= 100 ? "medium" : "long"; }
function seedNumber(seed: string): number { let n = 2166136261; for (const b of new TextEncoder().encode(seed)) n = Math.imul(n ^ b, 16777619); return n >>> 0; }
function shuffle<T>(items: T[], seed: string): T[] { let state = seedNumber(seed) || 1; const out = [...items]; for (let i = out.length - 1; i > 0; i--) { state = Math.imul(state ^ (state >>> 16), 2246822519) >>> 0; state = Math.imul(state ^ (state >>> 13), 3266489917) >>> 0; const j = state % (i + 1); [out[i], out[j]] = [out[j], out[i]]; } return out; }
function candidateMeta(item: Candidate) { const t = item.thread; return { threadId: t.id, createdAt: t.created_at, cwd: t.cwd, ownMessageCount: item.ownMessageCount, parentThreadId: t.parent_thread_id, hidden: t.hidden !== 0 }; }
function inBucket(items: Candidate[]): Record<Stratum, Candidate[]> { return { short: items.filter((x) => bucket(x.ownMessageCount) === "short"), medium: items.filter((x) => bucket(x.ownMessageCount) === "medium"), long: items.filter((x) => bucket(x.ownMessageCount) === "long") }; }
function sorted(items: Candidate[]): Candidate[] { return [...items].sort((a, b) => a.thread.created_at.localeCompare(b.thread.created_at) || a.thread.id.localeCompare(b.thread.id)); }
function ensureFreshOutput(out: string): void { if (existsSync(join(out, "manifest.json")) || existsSync(join(out, "cases"))) throw new Error(`refusing to overwrite an existing manifest/cases output: ${out}`); mkdirSync(join(out, "cases"), { recursive: true }); }

const options = parseArgs(process.argv.slice(2));
const validatedWindow = validateThreadFilter({ scope: "roots", includeHidden: false, since: options.since, until: options.until });
options.since = validatedWindow.sinceText!;
options.until = validatedWindow.untilText!;
ensureFreshOutput(options.out);
const prior = JSON.parse(readFileSync(options.priorManifest, "utf8")) as { cases?: Array<{ caseId?: string; threadId?: string }> };
const priorIds = new Set((prior.cases ?? []).flatMap((item) => [item.caseId, item.threadId].filter((id): id is string => typeof id === "string")));
const loader = new DataLoader(options.db);
const schemaIdentity = sha256(stable(loader.capabilities));
let frame: { dateVisibleRoots: Candidate[]; cwdRoots: Candidate[]; excludedPrior: Candidate[]; eligible: Candidate[]; beforeStrata: Record<Stratum, Candidate[]>; eligibleStrata: Record<Stratum, Candidate[]>; noOwnBefore: number; noOwnCwd: number; snapshotStatements: number };
try {
  const snapshot = loader.withSnapshot((db) => {
    const since = Date.parse(options.since), until = Date.parse(options.until);
    const dateVisibleRoots = db.loadVisibleMainThreads().filter((thread) => { const time = Date.parse(thread.created_at); if (!Number.isFinite(time)) throw new Error(`invalid created_at: ${thread.id}`); return time >= since && time < until; }).map((thread) => ({ thread, ownMessageCount: db.loadNormalizedMessages(thread.id).length }));
    const cwdRoots = dateVisibleRoots.filter((item) => item.thread.cwd === options.cwdRoot || item.thread.cwd.startsWith(`${options.cwdRoot}/`));
    const excludedPrior = cwdRoots.filter((item) => priorIds.has(item.thread.id));
    const eligible = cwdRoots.filter((item) => !priorIds.has(item.thread.id));
    return { dateVisibleRoots, cwdRoots, excludedPrior, eligible, beforeStrata: inBucket(cwdRoots), eligibleStrata: inBucket(eligible), noOwnBefore: dateVisibleRoots.filter((item) => item.ownMessageCount === 0).length, noOwnCwd: cwdRoots.filter((item) => item.ownMessageCount === 0).length };
  });
  frame = { ...snapshot.value, snapshotStatements: snapshot.statements };
} finally { loader.close(); }

const selected: Array<Candidate & { stratum: Stratum }> = [];
for (const name of ["short", "medium", "long"] as Stratum[]) selected.push(...shuffle(sorted(frame.eligibleStrata[name]), `${options.seed}:${name}`).slice(0, options.perStratum).map((item) => ({ ...item, stratum: name })));
selected.sort((a, b) => a.stratum.localeCompare(b.stratum) || a.thread.created_at.localeCompare(b.thread.created_at) || a.thread.id.localeCompare(b.thread.id));
const packetCases: Array<Record<string, unknown>> = [];
for (const item of selected) {
  const packet = exportTaskPacket(options.db, item.thread.id, { includeContent: true, maxBytes: MAX_BYTES, maxMessages: MAX_MESSAGES });
  const casePath = join(options.out, "cases", `${item.thread.id}.json`);
  writeFileSync(casePath, JSON.stringify(packet, null, 2) + "\n");
  packetCases.push({ caseId: packet.caseId, threadId: packet.threadId, stratum: item.stratum, createdAt: item.thread.created_at, cwd: item.thread.cwd, ownMessageCount: packet.ownMessageCount, packetHash: packet.packetHash, coverage: packet.coverage, path: `cases/${item.thread.id}.json` });
}
const rangeHash = (items: Candidate[]) => sha256(stable(sorted(items).map(candidateMeta)));
const strata = Object.fromEntries((["short", "medium", "long"] as Stratum[]).map((name) => { const candidates = sorted(frame.eligibleStrata[name]); const picked = selected.filter((item) => item.stratum === name); return [name, { range: name === "short" ? "1..20" : name === "medium" ? "21..100" : ">100", candidateCount: candidates.length, selectedCount: picked.length, candidateIds: candidates.map((x) => x.thread.id), selectedIds: picked.map((x) => x.thread.id), candidates: candidates.map(candidateMeta), selected: picked.map(candidateMeta), rangeHash: rangeHash(candidates) }]; })) as Record<Stratum, unknown>;
const quality = { selectedCount: packetCases.length, sourceTruncatedCases: packetCases.filter((x) => (x.coverage as { sourceTruncatedMessages: number }).sourceTruncatedMessages > 0).length, exportTruncatedCases: packetCases.filter((x) => (x.coverage as { exportTruncated: boolean }).exportTruncated).length, omittedMessages: packetCases.reduce((sum, x) => sum + Number((x.coverage as { omittedMessages: number }).omittedMessages), 0), omittedFields: [...new Set(packetCases.flatMap((x) => (x.coverage as { omittedFields: string[] }).omittedFields))].sort(), sourceMessageCount: packetCases.reduce((sum, x) => sum + Number((x.coverage as { sourceMessageCount: number }).sourceMessageCount), 0), exportedMessageCount: packetCases.reduce((sum, x) => sum + Number((x.coverage as { exportedMessageCount: number }).exportedMessageCount), 0) };
const manifest = { schemaVersion: 1, purpose: "Exploratory stratified task evidence selection for Peri improvements; no task outcome judgments.", defaults: DEFAULTS, arguments: { ...options, db: options.db, out: options.out, priorManifest: options.priorManifest }, window: { since: options.since, until: options.until, interval: "[since, until)", timeBasis: "visible root thread created_at" }, filters: { scope: "roots", includeHidden: false, cwdRoot: options.cwdRoot, cwdPolicy: "thread.cwd === cwdRoot or startsWith(cwdRoot + '/')", beforeCwdCount: frame.dateVisibleRoots.length, afterCwdCount: frame.cwdRoots.length, noOwnMessagesBeforeCwd: frame.noOwnBefore, noOwnMessagesAfterCwd: frame.noOwnCwd, priorExclusion: { policy: "exclude every candidate ID present in the prior pilot manifest before stratification", manifest: options.priorManifest, priorCaseIdCount: priorIds.size, matchedCandidateCount: frame.excludedPrior.length, matchedCandidateIds: frame.excludedPrior.map((x) => x.thread.id) } }, sampling: { seed: options.seed, perStratum: options.perStratum, scope: "roots", includeHidden: false, strata, noOwnMessagesEligible: frame.eligible.filter((x) => x.ownMessageCount === 0).length, selectedIds: selected.map((x) => x.thread.id) }, source: { database: options.db, normalizer: NORMALIZER_VERSION, schemaIdentity, snapshot: "readonly-transaction", selectionSnapshotStatements: frame.snapshotStatements }, methods: { taskPackets: "src/research/task-packets.ts", taskPacketsSha256: fileSha256(join(ANALYZER_ROOT, "src/research/task-packets.ts")), taskEvaluationSha256: fileSha256(join(ANALYZER_ROOT, "TASK-EVALUATION.md")), activeSkillSha256: fileSha256(join(REPO_ROOT, ".agents/skills/agent-task-evaluator/SKILL.md")), priorManifestSha256: fileSha256(options.priorManifest) }, quality, cases: packetCases };
writeFileSync(join(options.out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(JSON.stringify({ output: options.out, beforeCwd: frame.dateVisibleRoots.length, afterCwd: frame.cwdRoots.length, noOwnAfterCwd: frame.noOwnCwd, priorMatched: frame.excludedPrior.length, strata: Object.fromEntries((["short", "medium", "long"] as Stratum[]).map((name) => [name, { candidates: frame.eligibleStrata[name].length, selected: selected.filter((x) => x.stratum === name).length }])), selectedIds: selected.map((x) => x.thread.id) }, null, 2));
