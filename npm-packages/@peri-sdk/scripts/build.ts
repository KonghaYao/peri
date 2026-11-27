import { existsSync } from "node:fs";
import { mkdir, readFile, rm } from "node:fs/promises";
import { resolve } from "node:path";

const packageRoot = resolve(import.meta.dir, "..");
const output = resolve(packageRoot, "dist");

async function run(command: string, args: string[], cwd: string): Promise<void> {
  const child = Bun.spawn([command, ...args], { cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const status = await child.exited;
  if (status !== 0) throw new Error(`${command} exited with status ${status}`);
}

/** 每个发布入口单独打包；target 由该入口的宿主约束决定。 */
const entries: Array<{
  name: string;
  entry: string;
  target: "bun" | "browser" | "node";
  external?: string[];
}> = [
  { name: "SDK", entry: "src/entries/index.ts", target: "bun", external: ["yjs"] },
  { name: "Session view", entry: "src/entries/view.ts", target: "browser", external: ["yjs"] },
  { name: "Portable SDK", entry: "src/entries/portable.ts", target: "bun", external: ["yjs"] },
  { name: "Sandbox SDK", entry: "src/entries/sandbox.ts", target: "node" },
  { name: "Observe SDK", entry: "src/entries/observe.ts", target: "node" },
];

await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });

for (const { name, entry, target, external } of entries) {
  const result = await Bun.build({
    entrypoints: [resolve(packageRoot, entry)],
    outdir: output,
    target,
    format: "esm",
    ...(external === undefined ? {} : { external }),
  });
  if (!result.success) throw new Error(`${name} bundle failed: ${result.logs.join("; ")}`);
}

await run(resolve(packageRoot, "node_modules/.bin/tsc"), [
  "--noEmit", "false", "--emitDeclarationOnly", "--declaration", "--outDir", output,
], packageRoot);

// 消费方依赖主入口与 /view 都暴露 ChatState；类型与运行时各检查一次。
for (const file of ["index.js", "view.js", "entries/index.d.ts", "entries/view.d.ts"]) {
  if (!existsSync(resolve(output, file))) throw new Error(`build is missing ${file}`);
}
const viewTypes = await readFile(resolve(output, "entries/view.d.ts"), "utf8");
const chatTypes = await readFile(resolve(output, "entries/chat.d.ts"), "utf8");
const viewJs = await readFile(resolve(output, "view.js"), "utf8");
if (!viewTypes.includes("ChatState") || !chatTypes.includes("ChatState") || !viewJs.includes("ChatState")) {
  throw new Error("build does not expose ChatState from the main and /view entries");
}

console.log(`Built @peri-code/sdk into ${output}`);
