import { JsonRpcTransport } from "./json-rpc-transport";
import { ProcessBroker } from "./process-broker";
import { randomBytes } from "node:crypto";
import type { StdioTransportOptions } from "./types";

const MAX_SETTINGS_BYTES = 1024 * 1024;
const MAX_ACP_LINE_BYTES = 8 * 1024 * 1024;
const encoder = new TextEncoder();

function bootstrapFrame(options: StdioTransportOptions): Uint8Array {
  if (
    options.settings === null ||
    Array.isArray(options.settings) ||
    typeof options.settings !== "object"
  ) {
    throw new Error("settings bootstrap must be a JSON object");
  }
  let json: string;
  try {
    json = JSON.stringify(options.settings);
  } catch {
    throw new Error("settings bootstrap is not JSON serializable");
  }
  if (!json) throw new Error("settings bootstrap is not JSON serializable");
  const bytes = encoder.encode(json);
  if (bytes.length === 0 || bytes.length > MAX_SETTINGS_BYTES) {
    throw new Error("settings bootstrap size is invalid");
  }
  const frame = new Uint8Array(4 + bytes.length);
  new DataView(frame.buffer).setUint32(0, bytes.length, false);
  frame.set(bytes, 4);
  return frame;
}

export class StdioTransport extends JsonRpcTransport {
  private readonly process: Bun.Subprocess<"pipe", "pipe", "pipe">;
  private readonly terminationProof: Promise<boolean>;
  readonly generationId: string;

  private constructor(process: Bun.Subprocess<"pipe", "pipe", "pipe">, generationId: string, broker?: ProcessBroker) {
    super(async (frame) => {
      await process.stdin.write(encoder.encode(frame + "\n"));
      await process.stdin.flush();
    }, async () => {
      process.stdin.end();
      const exited = await Promise.race([
        process.exited.then(() => true),
        Bun.sleep(5_000).then(() => false),
      ]);
      if (!exited) process.kill();
      await process.exited;
      if (broker && !(await this.terminationProof)) {
        throw new Error("ACP subprocess cleanup could not be confirmed");
      }
    });
    this.process = process;
    this.generationId = generationId;
    this.terminationProof = process.exited.then(
      () => broker?.settleAfterAgentExit() ?? false,
      () => broker?.settleAfterAgentExit() ?? false,
    );
    void this.readOutput();
    void this.drainStderr();
    void process.exited.then(
      (code) => this.fail(new Error(`ACP process exited (code ${code})`)),
      () => this.fail(new Error("ACP process exited")),
    );
  }

  static async start(options: StdioTransportOptions): Promise<StdioTransport> {
    const frame =
      options.settings === undefined ? undefined : bootstrapFrame(options);
    const env = { ...process.env, ...options.env };
    const generationId = randomBytes(16).toString("hex");
    const broker = process.platform === "win32" ? undefined : await ProcessBroker.start();
    if (broker) {
      env.PERI_PROCESS_BROKER_SOCKET = broker.socketPath;
      env.PERI_PROCESS_BROKER_TOKEN = broker.token;
    }
    for (const [key, value] of Object.entries(env))
      if (value === undefined) delete env[key];
    let child: Bun.Subprocess<"pipe", "pipe", "pipe">;
    try {
      child = Bun.spawn([options.command, ...(options.args ?? [])], {
        cwd: options.cwd,
        env: env as Record<string, string>,
        stdin: "pipe",
        stdout: "pipe",
        stderr: "pipe",
      });
    } catch (error) {
      await broker?.settleAfterAgentExit();
      throw error;
    }
    const transport = new StdioTransport(child, generationId, broker);
    if (frame) {
      try {
        await child.stdin.write(frame);
        await child.stdin.flush();
      } catch {
        await transport.close();
        throw new Error("failed to write settings bootstrap");
      }
    }
    return transport;
  }

  /** Trusted supervisor evidence for this exact spawned ACP generation. */
  waitForTerminationProof(): Promise<boolean> {
    return this.terminationProof;
  }

  /** OS identity for host supervision and crash testing. */
  get pid(): number { return this.process.pid; }
  async executionStopped(): Promise<boolean> {
    if (this.process.exitCode === null && this.process.signalCode === null) return false;
    return this.terminationProof;
  }

  private async readOutput(): Promise<void> {
    try {
      const stream = this.process.stdout;
      if (!stream) throw new Error("ACP stdout is unavailable");
      let fragments: Uint8Array[] = [];
      let length = 0;
      for await (const chunk of stream) {
        let start = 0;
        for (let index = 0; index < chunk.length; index++) {
          if (chunk[index] !== 10) continue;
          const part = chunk.subarray(start, index);
          length += part.length;
          if (length > MAX_ACP_LINE_BYTES)
            throw new Error("ACP line exceeds size limit");
          if (length) {
            const line = new Uint8Array(length);
            let offset = 0;
            for (const fragment of fragments) {
              line.set(fragment, offset);
              offset += fragment.length;
            }
            line.set(part, offset);
            this.acceptFrame(new TextDecoder("utf-8", { fatal: true }).decode(line));
          }
          fragments = [];
          length = 0;
          start = index + 1;
        }
        const rest = chunk.subarray(start);
        if (rest.length) {
          length += rest.length;
          if (length > MAX_ACP_LINE_BYTES)
            throw new Error("ACP line exceeds size limit");
          fragments.push(rest);
        }
      }
      const code = await this.process.exited;
      this.fail(new Error(`ACP process exited (code ${code})`));
    } catch {
      this.fail(new Error("ACP stdout failed"));
      this.process.kill();
    }
  }

  private async drainStderr(): Promise<void> {
    try {
      for await (const _chunk of this.process.stderr) {
        /* never expose secrets from stderr */
      }
    } catch {
      /* exit path settles pending requests */
    }
  }

}
