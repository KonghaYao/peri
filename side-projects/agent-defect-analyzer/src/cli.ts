import { existsSync, statSync } from "node:fs";
import type { AnalysisOptions, AnalysisScope } from "./analysis/metrics.js";
import { inspectDatabase, writeQualityReports } from "./reporting/quality.js";
import { reportDatabase, writeReportReports } from "./reporting/report.js";

interface CommonArgs {
  db: string;
  out: string;
}

export type CliArgs = CommonArgs & ({ command: "inspect" } | ({ command: "report" } & AnalysisOptions));

function usage(): string {
  return "Usage: bun run src/cli.ts inspect|report --db <threads.db> --out <directory> [--scope roots|children|all] [--include-hidden] [--since UTC] [--until UTC]";
}

export function parseCliArgs(argv: string[]): CliArgs {
  const command = argv[0];
  if (command !== "inspect" && command !== "report") throw new Error(`expected 'inspect' or 'report' command\n${usage()}`);
  const values = new Map<string, string>();
  let includeHidden = false;
  for (let i = 1; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === "--include-hidden") {
      if (command !== "report" || includeHidden) throw new Error(`unknown or duplicate argument: ${flag}\n${usage()}`);
      includeHidden = true;
      continue;
    }
    const allowed = command === "report"
      ? ["--db", "--out", "--scope", "--since", "--until"]
      : ["--db", "--out"];
    if (!allowed.includes(flag)) throw new Error(`unknown or misplaced argument: ${flag}\n${usage()}`);
    if (values.has(flag)) throw new Error(`duplicate argument: ${flag}\n${usage()}`);
    const value = argv[++i];
    if (!value || value.startsWith("--")) throw new Error(`missing value for ${flag}\n${usage()}`);
    values.set(flag, value);
  }
  const db = values.get("--db");
  const out = values.get("--out");
  if (!db || !out) throw new Error(`--db and --out are required\n${usage()}`);
  if (!existsSync(db)) throw new Error(`database does not exist: ${db}`);
  if (!statSync(db).isFile()) throw new Error(`database is not a file: ${db}`);
  if (command === "inspect") return { command, db, out };
  const scope = values.get("--scope") ?? "roots";
  if (scope !== "roots" && scope !== "children" && scope !== "all") throw new Error(`--scope must be roots, children, or all\n${usage()}`);
  return {
    command, db, out, includeHidden,
    scope: scope as AnalysisScope,
    ...(values.has("--since") ? { since: values.get("--since") } : {}),
    ...(values.has("--until") ? { until: values.get("--until") } : {}),
  };
}

export function run(argv: string[]): void {
  const args = parseCliArgs(argv);
  if (args.command === "inspect") {
    const dto = inspectDatabase(args.db);
    writeQualityReports(dto, args.out);
    process.stdout.write(`inspect complete: ${args.out}/quality.json\n`);
  } else {
    const report = reportDatabase(args.db, { scope: args.scope, includeHidden: args.includeHidden, since: args.since, until: args.until });
    writeReportReports(report, args.out);
    process.stdout.write(`report complete: ${args.out}/report.json\n`);
  }
}

if (import.meta.main) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`inspect failed: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 2;
  }
}
