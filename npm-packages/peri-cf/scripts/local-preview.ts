import { createWriteStream } from "node:fs";
import { chmod, mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { localDnsOverrides } from "./local-dns";
import { localPreviewCommand } from "./local-preview-command";
import { relayOutput } from "./relay-output";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const logDirectory = fileURLToPath(new URL("../logs/", import.meta.url));
const sourceVars = new URL("../.dev.vars", import.meta.url);
const builtVars = new URL("../dist/peri_cf/.dev.vars", import.meta.url);
const overrides = await localDnsOverrides(sourceVars);
if (overrides) {
  const source = (await readFile(builtVars, "utf8")).replace(/^PERI_DNS_OVERRIDES=.*\n?/gm, "");
  await writeFile(builtVars, `${source.trimEnd()}\nPERI_DNS_OVERRIDES='${overrides}'\n`, { mode: 0o600 });
  await chmod(builtVars, 0o600);
}
// Wrangler 请求日志、Worker console 与 WASM 侧输出同时写入 logs/ 供事后排查。
await mkdir(logDirectory, { recursive: true });
const now = new Date();
const pad = (value: number) => String(value).padStart(2, "0");
const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-` +
  `${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
const logPath = `${logDirectory}/dev-${stamp}.log`;
const sink = createWriteStream(logPath, { flags: "a", mode: 0o600 });
console.info(`Peri CF dev logs: ${logPath}`);

const child = Bun.spawn(localPreviewCommand(packageRoot, process.argv.slice(2)), {
  cwd: packageRoot, stdin: "inherit", stdout: "pipe", stderr: "pipe",
});
const relays = [
  relayOutput(child.stdout, process.stdout, sink),
  relayOutput(child.stderr, process.stderr, sink),
];
for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => child.kill(signal));
process.exitCode = await child.exited;
await Promise.all(relays);
await new Promise<void>((resolve) => sink.end(resolve));
