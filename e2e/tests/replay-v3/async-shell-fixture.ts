import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import type { ReplayTui, ReplayWorld } from "../../helpers/llm-replay-tui.js";
import { contentText, resultText, type CallIdentity } from "../replay-v2/agents-fixture.js";

export interface ShellSpec { id: string; alias: string; command: string }
export interface ShellResult { id: string; text: string; stdout?: string; stderr?: string; state?: string; exitCode?: string; remoteTaskId?: string }

export const shellCall = (call: string, execution = "start", owner = "root"): CallIdentity =>
  ({ owner, execution, call, purpose: "reason", attempt: 1 });

/** A real file gate controls the external process; its bounded polling is not a completion oracle. */
export function gatedCommand(name: string, command: string): string {
  return `printf '%s\\n' "$BASHPID" > ${name}.pid; printf 'started\\n' >> ${name}.starts; for i in {1..6000}; do [ -f ${name}.release ] && break; sleep 0.01; done; [ -f ${name}.release ] || exit 70; ${command}`;
}

export async function fileReady(ctx: ReplayScenarioContext, name: string): Promise<string> {
  await expect.poll(async () => {
    expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
    try { return (await readFile(path.join(ctx.world.work, name), "utf8")).trim().length > 0; }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; return false; }
  }, { timeout: 20_000, interval: 20 }).toBe(true);
  return readFile(path.join(ctx.world.work, name), "utf8");
}

export async function releaseShell(ctx: ReplayScenarioContext, name: string): Promise<void> {
  await writeFile(path.join(ctx.world.work, `${name}.release`), "release\n");
}

export async function configureShell(world: ReplayWorld): Promise<void> {
  await mkdir(path.join(world.home, "tmp"), { recursive: true });
}

export async function startShellTui(ctx: ReplayScenarioContext, alias = "root", rows = 50): Promise<ReplayTui> {
  return ctx.startTui(alias, { cols: 150, rows, env: {
    TMPDIR: path.join(ctx.world.home, "tmp"), TMP: path.join(ctx.world.home, "tmp"), TEMP: path.join(ctx.world.home, "tmp"),
  } });
}

export function requestText(body: any): string {
  return (body.messages ?? []).map((message: any) => contentText(message.content)).join("\n");
}

export function resultFromBody(body: any, taskId: string): ShellResult | undefined {
  const entities: Record<string, string> = { "&lt;": "<", "&gt;": ">", "&quot;": '"', "&apos;": "'", "&amp;": "&" };
  const decode = (value: string) => value.replace(/&(?:lt|gt|quot|apos|amp);/gu, entity => entities[entity]!);
  const chunks = [...requestText(body).matchAll(/<system-reminder\b([^>]*)>([\s\S]*?)<\/system-reminder>/gu)]
    .map(match => ({ attributes: match[1]!, text: decode(match[2]!) }));
  const chunk = chunks.find(chunk => chunk.text.includes(`task_id: ${taskId}\n`) && chunk.text.includes("\nstate: "));
  if (!chunk) return undefined;
  const { text } = chunk;
  const metadataText = chunk.attributes.match(/\bmetadata="([^"]*)"/u)?.[1];
  const metadata = metadataText ? JSON.parse(decode(metadataText)) : undefined;
  const field = (name: string) => text.match(new RegExp(`(?:^|\\n)${name}: ([^\\r\\n]+)`, "u"))?.[1];
  return { id: taskId, text, stdout: field("stdout"), stderr: field("stderr"), state: field("state"), exitCode: field("exit_code"), remoteTaskId: metadata?.remote_task_id };
}

/** Task IDs and log paths are bound only through the named real Bash receipt and its terminal reminder. */
export class ShellBindings {
  readonly tasks = new Map<string, string>();
  readonly outputs = new Map<string, ShellResult>();
  private readonly bound = new Map<string, string>();
  constructor(readonly ctx: ReplayScenarioContext, readonly specs: ShellSpec[]) {}
  task(alias: string): string {
    const task = this.tasks.get(alias);
    if (!task) throw new Error(`Bash receipt missing task ${alias}`);
    return task;
  }
  output(alias: string): ShellResult {
    const value = this.outputs.get(alias);
    if (!value) throw new Error(`Shell terminal reminder missing ${alias}`);
    return value;
  }
  private bind(kind: "task" | "path", alias: string, live: string, anchor: string) {
    const key = `${kind}:${alias}`;
    if (this.bound.has(key)) {
      expect(this.bound.get(key)).toBe(live);
      return;
    }
    this.ctx.bind({ kind, alias, live, anchor });
    this.bound.set(key, live);
  }
  onRequest = async ({ context, request }: { context: CallIdentity; request: { body: Buffer } }) => {
    const body = JSON.parse(request.body.toString("utf8"));
    const blocks = (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : []);
    for (const spec of this.specs) {
      const result = blocks.find((block: any) => block.type === "tool_result" && block.tool_use_id === spec.id);
      if (!result) continue;
      const text = contentText(result.content);
      const taskId = text.match(/Background task started: (mcp-[A-Za-z0-9_-]+)\./u)?.[1];
      if (!taskId) continue;
      if (this.tasks.has(spec.alias)) expect(this.tasks.get(spec.alias)).toBe(taskId);
      expect(Boolean(result.is_error)).toBe(false);
      this.tasks.set(spec.alias, taskId);
      this.bind("task", `shell-${spec.alias}`, taskId, `Bash tool receipt ${spec.id}`);
      const output = resultFromBody(body, taskId);
      if (!output) continue;
      expect(output.text).toContain(`command: ${[...spec.command].slice(0, 80).join("").trim()}`);
      if (output.remoteTaskId) this.bind("task", `shell-${spec.alias}-remote`, output.remoteTaskId, `MCP remote task linked to Bash tool ${spec.id}`);
      for (const stream of ["stdout", "stderr"] as const) {
        const file = output[stream];
        if (!file || !path.isAbsolute(file)) continue;
        expect(path.relative(this.ctx.world.root, file).startsWith("..")).toBe(false);
        this.bind("path", `shell-${spec.alias}-${stream}`, file, `terminal ${stream} reference for ${spec.id}`);
      }
      this.outputs.set(spec.alias, output);
    }
    if (this.ctx.mode === "record") this.ctx.upstream!.correlate(context, request.body);
  };
}

export async function finished(ctx: ReplayScenarioContext, tui: ReplayTui, marker: string): Promise<string> {
  return tui.waitFor(ctx.checked((screen: string) => screen.includes(marker) && screen.includes("Brewed for")), marker, 40_000);
}

export async function assertReceipt(ctx: ReplayScenarioContext, toolId: string, execution = "start"): Promise<string> {
  await ctx.waitForCalls((snapshot) => snapshot.calls.some((call: any) => call.identity.execution === execution && call.identity.call === "reason:2"));
  const request = ctx.requests.find((call) => call.context.execution === execution && call.context.call === "reason:2");
  expect(request).toBeDefined();
  const text = resultText(JSON.parse(request!.request.body.toString()), toolId);
  expect(text).toContain("Background task started: mcp-");
  expect(text).not.toMatch(/(?:^|\n)(?:stdout|stderr|pid):/u);
  return text;
}

export async function terminalBody(ctx: ReplayScenarioContext, execution = "completion"): Promise<any> {
  await ctx.waitForCalls((snapshot) => snapshot.calls.some((call: any) => call.identity.execution === execution && call.identity.call === "reason:1"));
  return JSON.parse(ctx.requests.find((call) => call.context.execution === execution && call.context.call === "reason:1")!.request.body.toString());
}

export async function assertStartedOnce(ctx: ReplayScenarioContext, name: string) {
  expect(await readFile(path.join(ctx.world.work, `${name}.starts`), "utf8")).toBe("started\n");
}

export async function absent(ctx: ReplayScenarioContext, name: string) {
  await expect(access(path.join(ctx.world.work, name))).rejects.toMatchObject({ code: "ENOENT" });
}

export async function programStatuses(tui: ReplayTui, stage: string): Promise<string[]> {
  await tui.save(stage);
  const raw = await readFile(path.join(tui.world.evidence, `${tui.label}-${stage}.pty`), "utf8");
  const values = [...raw.matchAll(/\x1b\]7501;state=([a-z]+)(?::[^\x1b\x07]*)?(?:\x07|\x1b\\)/gu)].map((match) => match[1]!);
  expect(values.length).toBeGreaterThan(0);
  return values;
}

export async function processExists(pid: number): Promise<boolean> {
  try {
    process.kill(pid, 0);
    try {
      const status = await readFile(`/proc/${pid}/stat`, "utf8");
      if (/\) Z /u.test(status)) return false;
    } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
    throw error;
  }
}

export async function assertExited(pids: number[]): Promise<void> {
  for (const pid of pids) expect(Number.isInteger(pid) && pid > 0).toBe(true);
  await expect.poll(async () => Promise.all(pids.map(processExists)), { timeout: 15_000, interval: 30 }).toEqual(pids.map(() => false));
}
