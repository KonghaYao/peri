import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { REPLAY_BINARY, replayEnvironment } from "../../helpers/llm-replay-tui.js";

/** A protocol client for the actual bundled monitor server, without an Agent-side mock. */
export class MonitorWire {
  readonly child: ChildProcessWithoutNullStreams;
  readonly wire: Array<{ direction: string; message: any }> = [];
  readonly closed: Promise<{ code: number | null; signal: string | null }>;
  private readonly pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private readonly lines;
  private sequence = 0;
  private stderr = "";
  private failure?: Error;
  private ended = false;
  private exit?: { code: number | null; signal: string | null };
  constructor(readonly ctx: ReplayScenarioContext, readonly label: string) {
    this.child = spawn(REPLAY_BINARY, ["mcp-start", "monitor", "--stdio", "--workspace", ctx.world.work], {
      cwd: ctx.world.work, env: replayEnvironment(ctx.world), stdio: ["pipe", "pipe", "pipe"], detached: true,
    });
    this.lines = createInterface({ input: this.child.stdout });
    this.child.stderr.on("data", (data) => { this.stderr = (this.stderr + data.toString()).slice(-16_384); });
    this.child.on("error", (error) => this.fail(error));
    this.child.stdin.on("error", (error) => this.fail(error));
    this.lines.on("line", (line: string) => {
      try {
        const message = JSON.parse(line); this.wire.push({ direction: "receive", message });
        if (message.method) { if (message.id !== undefined) this.send({ id: message.id, error: { code: -32601, message: "unsupported fixture client request" } }); return; }
        const pending = this.pending.get(message.id); if (!pending) throw new Error(`unmatched MCP response ${message.id}`);
        this.pending.delete(message.id); clearTimeout(pending.timer); pending.resolve(message);
      } catch (error) { this.fail(error instanceof Error ? error : new Error(String(error))); }
    });
    this.closed = new Promise((resolve) => this.child.once("close", (code, signal) => {
      this.ended = true; this.exit = { code, signal }; this.fail(new Error(`monitor MCP closed ${code ?? signal}: ${this.stderr}`)); resolve({ code, signal });
    }));
  }
  private fail(error: Error) { this.failure ??= error; for (const call of this.pending.values()) { clearTimeout(call.timer); call.reject(this.failure); } this.pending.clear(); }
  private send(message: any) { this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`); }
  call(method: string, params: any = {}, owner?: string): Promise<any> {
    if (this.failure) throw this.failure;
    const id = ++this.sequence, message = { id, method, params: { ...params, _meta: {
      "io.modelcontextprotocol/protocolVersion": "2026-07-28",
      "io.modelcontextprotocol/clientInfo": { name: "peri-e2e-monitor", version: "3" },
      "io.modelcontextprotocol/clientCapabilities": { extensions: { "io.modelcontextprotocol/tasks": {} } },
      ...(owner ? { "peri/taskOwner": owner } : {}),
    } } };
    this.wire.push({ direction: "send", message });
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`monitor wire timeout ${method}: ${this.stderr}`)); }, 20_000);
      this.pending.set(id, { resolve, reject, timer }); this.send(message);
    });
  }
  async open() {
    const discovered = await this.call("server/discover"); expect(discovered.error).toBeUndefined();
    const listing = await this.call("tools/list"); expect(listing.error).toBeUndefined();
    expect(listing.result.tools.map((tool: any) => tool.name).sort()).toEqual(["monitor_list", "monitor_read", "monitor_start", "monitor_stop"]);
    return this;
  }
  async tool(name: string, args: any, owner?: string) { return this.call("tools/call", { name, arguments: args }, owner); }
  async start(command: string, owner: string, label: string) {
    const reply = await this.tool("monitor_start", { command, label }, owner);
    expect(reply.error).toBeUndefined(); expect(reply.result.resultType).toBe("task");
    expect(reply.result.taskId).toMatch(/^monitor-/u); expect(reply.result.status).toBe("working");
    return reply.result.taskId as string;
  }
  result(reply: any) {
    expect(reply.error).toBeUndefined(); expect(reply.result.isError).not.toBe(true);
    return reply.result.structuredContent ?? JSON.parse(reply.result.content[0].text);
  }
  async save() { await writeFile(path.join(this.ctx.world.evidence, `${this.label}-mcp-wire.json`), JSON.stringify({ wire: this.wire, stderr: this.stderr, exit: this.exit }, null, 2)); }
  async close() {
    if (!this.ended && this.child.stdin.writable) this.child.stdin.end();
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([this.closed, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("monitor standalone failed to close on transport EOF")), 10_000); })]);
    } finally {
      if (timer) clearTimeout(timer);
      if (!this.ended) { try { process.kill(-this.child.pid!, "SIGKILL"); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error; } await this.closed; }
      this.lines.close(); await this.save();
    }
    expect(this.exit, "normal MCP transport EOF must close the standalone server successfully").toEqual({ code: 0, signal: null });
  }
}
