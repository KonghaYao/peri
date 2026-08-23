import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import readline from "node:readline";
import test from "node:test";

function writeFrame(child: ChildProcessWithoutNullStreams, frame: unknown): void {
  child.stdin.write(`${JSON.stringify(frame)}\n`);
}

test("dist CLI performs handshake and emits only NDJSON on stdout", async (t) => {
  const child = spawn(process.execPath, ["dist/peri-ptc.js"], {
    cwd: new URL("..", import.meta.url),
    stdio: ["pipe", "pipe", "pipe"],
  });
  t.after(() => child.kill());

  const stdout = readline.createInterface({ input: child.stdout });
  const frames: unknown[] = [];
  stdout.on("line", (line) => frames.push(JSON.parse(line)));

  writeFrame(child, {
    jsonrpc: "2.0",
    id: 1,
    method: "ptc/start",
    params: { protocolVersion: 1 },
  });
  writeFrame(child, {
    jsonrpc: "2.0",
    id: 2,
    method: "execute",
    params: { source: "console.log('e2e'); return input.value;", input: { value: 42 } },
  });
  child.stdin.end();

  const [exitCode, stderr] = await Promise.all([
    new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", resolve);
    }),
    new Promise((resolve) => {
      let output = "";
      child.stderr.setEncoding("utf8");
      child.stderr.on("data", (chunk) => { output += chunk; });
      child.stderr.on("end", () => resolve(output));
    }),
  ]);

  assert.equal(exitCode, 0);
  assert.equal(stderr, "");
  assert.deepEqual(frames, [
    {
      jsonrpc: "2.0",
      id: 1,
      result: { protocolVersion: 1, buildId: "@peri-code/ptc@0.2.2" },
    },
    { jsonrpc: "2.0", id: 2, result: { value: 42, logs: ["e2e"] } },
  ]);
});
