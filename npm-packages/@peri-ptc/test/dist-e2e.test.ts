import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import readline from "node:readline";
import test from "node:test";

function writeFrame(child: ChildProcessWithoutNullStreams, frame: unknown): void {
  child.stdin.write(`${JSON.stringify(frame)}\n`);
}

test("dist CLI performs handshake and executes with the sanitized runtime environment", async (t) => {
  const environment: NodeJS.ProcessEnv = {};
  for (const name of ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"]) {
    const value = process.env[name];
    if (value !== undefined) environment[name] = value;
  }
  environment.PTC_TEST_SECRET = undefined;
  const child = spawn(process.execPath, ["dist/peri-ptc.js"], {
    cwd: new URL("..", import.meta.url),
    env: environment,
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
    params: {
      source: "const { tmpdir } = await import('node:os'); console.log('e2e'); return { value: input.value, secret: process.env.PTC_TEST_SECRET ?? null, tmpdir: tmpdir() };",
      input: { value: 42 },
    },
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
  assert.deepEqual(frames[0], {
    jsonrpc: "2.0",
    id: 1,
    result: { protocolVersion: 1, buildId: "@peri-code/ptc@0.2.3" },
  });
  assert.deepEqual(frames[1], {
    jsonrpc: "2.0",
    id: 2,
    result: {
      value: { value: 42, secret: null, tmpdir: (frames[1] as { result: { value: { tmpdir: unknown } } }).result.value.tmpdir },
      logs: ["e2e"],
    },
  });
  assert.equal(typeof (frames[1] as { result: { value: { tmpdir: unknown } } }).result.value.tmpdir, "string");
});
