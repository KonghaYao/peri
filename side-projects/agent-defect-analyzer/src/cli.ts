import { existsSync, mkdirSync, statSync, writeFileSync } from "node:fs";
import type { AnalysisOptions, AnalysisScope } from "./analysis/metrics.js";
import { inspectDatabase, writeQualityReports } from "./reporting/quality.js";
import { reportDatabase, writeReportReports } from "./reporting/report.js";
import { compareReportFiles, writeCompareReports } from "./reporting/compare.js";
import { evidenceForMessage, sampleThreads } from "./research/evidence.js";

interface CommonArgs { db: string; out: string; }
interface SampleArgs extends CommonArgs {
  command: "sample"; seed: string; size: number; scope: AnalysisScope; includeHidden: boolean;
  since?: string; until?: string; minMessages: number;
}
interface EvidenceArgs extends CommonArgs {
  command: "evidence"; threadId: string; messageId: string; radius: number; includeContent: boolean;
}
interface CompareArgs { command: "compare"; baseline: string; candidate: string; out: string; }
interface InspectArgs extends CommonArgs { command: "inspect"; }
interface ReportArgs extends CommonArgs { command: "report"; }

export type CliArgs = InspectArgs | (ReportArgs & AnalysisOptions) | SampleArgs | EvidenceArgs | CompareArgs;

function usage(): string {
  return "Usage: bun run src/cli.ts inspect|report|sample|evidence|compare ...";
}

export function parseCliArgs(argv: string[]): CliArgs {
  const command = argv[0];
  const commands = ["inspect", "report", "sample", "evidence", "compare"];
  if (!commands.includes(command)) throw new Error(`expected 'inspect', 'report', 'sample', 'evidence', or 'compare' command\n${usage()}`);
  const values = new Map<string, string>();
  let includeHidden = false;
  let includeContent = false;
  for (let i = 1; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === "--include-hidden") {
      if (!["report", "sample"].includes(command) || includeHidden) throw new Error(`unknown or duplicate argument: ${flag}\n${usage()}`);
      includeHidden = true;
      continue;
    }
    if (flag === "--include-content") {
      if (command !== "evidence" || includeContent) throw new Error(`unknown or duplicate argument: ${flag}\n${usage()}`);
      includeContent = true;
      continue;
    }
    const allowedByCommand: Record<string, string[]> = {
      inspect: ["--db", "--out"], report: ["--db", "--out", "--scope", "--since", "--until"],
      sample: ["--db", "--out", "--seed", "--size", "--scope", "--since", "--until", "--min-messages"],
      evidence: ["--db", "--out", "--thread", "--message", "--radius"],
      compare: ["--baseline", "--candidate", "--out"],
    };
    const allowed = allowedByCommand[command];
    if (!allowed.includes(flag)) throw new Error(`unknown or misplaced argument: ${flag}\n${usage()}`);
    if (values.has(flag)) throw new Error(`duplicate argument: ${flag}\n${usage()}`);
    const value = argv[++i];
    if (!value || value.startsWith("--")) throw new Error(`missing value for ${flag}\n${usage()}`);
    values.set(flag, value);
  }
  const out = values.get("--out");
  if (!out) throw new Error(`--out is required\n${usage()}`);
  const db = values.get("--db");
  if (command !== "compare") {
    if (!db) throw new Error(`--db is required\n${usage()}`);
    if (!existsSync(db)) throw new Error(`database does not exist: ${db}`);
    if (!statSync(db).isFile()) throw new Error(`database is not a file: ${db}`);
  }
  if (command === "compare") {
    const baseline = values.get("--baseline"); const candidate = values.get("--candidate");
    if (!baseline || !candidate) throw new Error(`--baseline and --candidate are required\n${usage()}`);
    if (!existsSync(baseline) || !statSync(baseline).isFile()) throw new Error(`baseline is not a file: ${baseline}`);
    if (!existsSync(candidate) || !statSync(candidate).isFile()) throw new Error(`candidate is not a file: ${candidate}`);
    return { command: "compare", baseline, candidate, out };
  }
  if (command === "inspect") return { command: "inspect", db: db!, out };
  const scope = values.get("--scope") ?? "roots";
  if (scope !== "roots" && scope !== "children" && scope !== "all") throw new Error(`--scope must be roots, children, or all\n${usage()}`);
  if (command === "report") return { command: "report", db: db!, out, includeHidden, scope: scope as AnalysisScope, ...(values.has("--since") ? { since: values.get("--since") } : {}), ...(values.has("--until") ? { until: values.get("--until") } : {}) };
  if (command === "sample") {
    const seed = values.get("--seed"); const sizeText = values.get("--size");
    if (seed === undefined || sizeText === undefined) throw new Error(`--seed and --size are required\n${usage()}`);
    const size = Number(sizeText); const minMessages = Number(values.get("--min-messages") ?? "0");
    if (!Number.isInteger(size) || size < 1 || !Number.isInteger(minMessages) || minMessages < 0) throw new Error(`--size must be a positive integer and --min-messages a non-negative integer\n${usage()}`);
    return { command: "sample", db: db!, out, seed, size, scope: scope as AnalysisScope, includeHidden, minMessages, ...(values.has("--since") ? { since: values.get("--since") } : {}), ...(values.has("--until") ? { until: values.get("--until") } : {}) };
  }
  const threadId = values.get("--thread"); const messageId = values.get("--message");
  if (!threadId || !messageId) throw new Error(`--thread and --message are required\n${usage()}`);
  const radius = Number(values.get("--radius") ?? "2");
  if (!Number.isInteger(radius) || radius < 0 || radius > 20) throw new Error(`--radius must be an integer between 0 and 20\n${usage()}`);
  return { command: "evidence", db: db!, out, threadId, messageId, radius, includeContent };
}

export function run(argv: string[]): void {
  const args = parseCliArgs(argv);
  if (args.command === "inspect") {
    const dto = inspectDatabase(args.db);
    writeQualityReports(dto, args.out);
    process.stdout.write(`inspect complete: ${args.out}/quality.json\n`);
  } else if (args.command === "report") {
    const report = reportDatabase(args.db, { scope: args.scope, includeHidden: args.includeHidden, since: args.since, until: args.until });
    writeReportReports(report, args.out);
    process.stdout.write(`report complete: ${args.out}/report.json\n`);
  } else if (args.command === "sample") {
    const result = sampleThreads(args.db, { seed: args.seed, size: args.size, scope: args.scope, includeHidden: args.includeHidden, minMessages: args.minMessages, since: args.since, until: args.until });
    mkdirSync(args.out, { recursive: true });
    writeFileSync(`${args.out}/sample.json`, JSON.stringify(result, null, 2));
    process.stdout.write(`sample complete: ${args.out}/sample.json\n`);
  } else if (args.command === "evidence") {
    const result = evidenceForMessage(args.db, { threadId: args.threadId, messageId: args.messageId, radius: args.radius, includeContent: args.includeContent });
    mkdirSync(args.out, { recursive: true });
    writeFileSync(`${args.out}/evidence.json`, JSON.stringify(result, null, 2));
    process.stdout.write(`evidence complete: ${args.out}/evidence.json\n`);
  } else {
    const result = compareReportFiles(args.baseline, args.candidate);
    writeCompareReports(result, args.out);
    process.stdout.write(`compare complete: ${args.out}/compare.json\n`);
  }
}

if (import.meta.main) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    const command = process.argv[2] ?? "cli";
    process.stderr.write(`${command} failed: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 2;
  }
}
