import { mkdir, rm } from "node:fs/promises";
import { spawn } from "node:child_process";

async function run(command: string, args: string[]): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const child = spawn(command, args, { stdio: "inherit" });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${command} exited with code ${code}`));
    });
  });
}

await rm("dist", { recursive: true, force: true });
await mkdir("dist", { recursive: true });
await run("bun", ["build", "src/cli.ts", "--outfile=dist/peri-ptc.js", "--target=node", "--format=esm", "--banner=#!/usr/bin/env node"]);
await run("bun", ["build", "src/index.ts", "--outfile=dist/index.js", "--target=node", "--format=esm"]);
await run("bunx", ["tsc", "--declaration", "--emitDeclarationOnly", "--outDir", "dist", "--noEmit", "false"]);
