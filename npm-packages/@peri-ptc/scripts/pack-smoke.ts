import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { spawn, type SpawnOptionsWithoutStdio } from "node:child_process";
import { tmpdir } from "node:os";
import path from "node:path";
import readline from "node:readline";

interface RunResult {
  stdout: string;
  stderr: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function run(command: string, args: string[], options: SpawnOptionsWithoutStdio = {}): Promise<RunResult> {
  return new Promise<RunResult>((resolve, reject) => {
    const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (code) => code === 0 ? resolve({ stdout, stderr }) : reject(new Error(`${command} failed (${code}): ${stderr}`)));
  });
}

const temp = await mkdtemp(path.join(tmpdir(), "peri-ptc-pack-"));
try {
  const pack = await run("npm", ["pack", "--json", "--pack-destination", temp]);
  const parsedPack: unknown = JSON.parse(pack.stdout);
  assert.ok(Array.isArray(parsedPack) && isRecord(parsedPack[0]) && typeof parsedPack[0].filename === "string");
  const filename = parsedPack[0].filename;
  const app = path.join(temp, "app");
  await mkdir(app);
  await run("npm", ["init", "-y"], { cwd: temp });
  await run("npm", ["install", "--ignore-scripts", path.join(temp, filename)], { cwd: temp });
  await writeFile(path.join(temp, "runtime.mjs"), `
import assert from "node:assert/strict";
import * as rootImported from "@peri-code/ptc";
import * as adapterImported from "@peri-code/ptc/adapter";
assert.equal(typeof rootImported.createPtcAdapter, "function");
assert.equal(typeof adapterImported.createPtcAdapter, "function");
`);
  await run(process.execPath, ["runtime.mjs"], { cwd: temp });

  await writeFile(path.join(temp, "types.ts"), `
import { createPtcAdapter as createRootAdapter } from "@peri-code/ptc";
import { createPtcAdapter, type PtcAdapter } from "@peri-code/ptc/adapter";
const rootAdapter: PtcAdapter = createRootAdapter(() => {});
const adapter: PtcAdapter = createPtcAdapter(() => {});
void [rootAdapter, adapter];
`);
  await writeFile(path.join(temp, "tsconfig.json"), `${JSON.stringify({
    compilerOptions: {
      strict: true,
      noEmit: true,
      target: "ES2022",
      module: "NodeNext",
      moduleResolution: "NodeNext",
    },
    files: ["types.ts"],
  }, null, 2)}\n`);
  await run(path.join(process.cwd(), "node_modules/.bin/tsc"), ["--project", "tsconfig.json"], { cwd: temp });

  const child = spawn(path.join(temp, "node_modules/.bin/peri-ptc"), [], {
    cwd: app,
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.stdin.end(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ptc/start", params: { protocolVersion: 1 } })}\n`);
  const lines = readline.createInterface({ input: child.stdout });
  const frame = await new Promise<Record<string, unknown>>((resolve, reject) => {
    lines.once("line", (line) => {
      const parsed: unknown = JSON.parse(line);
      if (isRecord(parsed)) resolve(parsed);
      else reject(new Error("CLI returned a non-object frame"));
    });
    child.once("error", reject);
  });
  assert.deepEqual(frame.result, { protocolVersion: 1, buildId: "@peri-code/ptc@0.2.2" });
  child.kill();
  await readFile(path.join(temp, "node_modules/@peri-code/ptc/dist/index.d.ts"));
} finally {
  await rm(temp, { recursive: true, force: true });
}
