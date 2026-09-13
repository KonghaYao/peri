import { existsSync, statSync } from "node:fs";
import { inspectDatabase, writeQualityReports } from "./reporting/quality.js";

interface InspectArgs {
  db: string;
  out: string;
}

function usage(): string {
  return "Usage: bun run src/cli.ts inspect --db <threads.db> --out <directory>";
}

export function parseCliArgs(argv: string[]): InspectArgs {
  if (argv[0] !== "inspect") throw new Error(`expected 'inspect' command\n${usage()}`);
  const values = new Map<string, string>();
  for (let i = 1; i < argv.length; i++) {
    const flag = argv[i];
    if (flag !== "--db" && flag !== "--out") throw new Error(`unknown or misplaced argument: ${flag}\n${usage()}`);
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
  return { db, out };
}

export function run(argv: string[]): void {
  const args = parseCliArgs(argv);
  const dto = inspectDatabase(args.db);
  writeQualityReports(dto, args.out);
  process.stdout.write(`inspect complete: ${args.out}/quality.json\n`);
}

if (import.meta.main) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`inspect failed: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 2;
  }
}
