import { createServer, type Server, type Socket } from "node:net";
import { chmodSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import type { StdioTransport } from "../transport/stdio-transport";

const MAX_QUERY_BYTES = 4096;

/** SDK-owned proof registry for exact stdio generations, never ACP input. */
export class ProcessSupervisor {
  readonly directory: string;
  readonly socketPath: string;
  readonly token: string;
  private readonly generations = new Map<string, StdioTransport>();
  private readonly sessionGenerations = new Map<string, Set<string>>();
  private readonly generationSessions = new Map<string, Set<string>>();
  private readonly registrations = new Map<string, Promise<void>>();

  private constructor(private readonly server: Server, directory: string) {
    this.directory = directory;
    this.socketPath = join(directory, "supervisor.sock");
    this.token = randomBytes(32).toString("hex");
  }

  static async start(): Promise<ProcessSupervisor> {
    const directory = mkdtempSync(join(tmpdir(), "peri-supervisor-"));
    chmodSync(directory, 0o700);
    const server = createServer();
    const supervisor = new ProcessSupervisor(server, directory);
    server.on("connection", (socket) => supervisor.accept(socket));
    try {
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(supervisor.socketPath, () => {
          server.off("error", reject);
          resolve();
        });
      });
      chmodSync(supervisor.socketPath, 0o600);
      return supervisor;
    } catch (error) {
      server.close();
      rmSync(directory, { recursive: true, force: true });
      throw error;
    }
  }

  /** Called immediately after spawn, before the child can receive a session request. */
  registerGeneration(transport: StdioTransport): void {
    const prior = this.generations.get(transport.generationId);
    if (prior && prior !== transport)
      throw new Error("Agent generation ID collision");
    this.generations.set(transport.generationId, transport);
  }

  async register(sessionId: string, transport: StdioTransport): Promise<void> {
    if (this.generations.get(transport.generationId) !== transport)
      throw new Error("Agent generation was not registered at spawn");
    const preceding = this.registrations.get(sessionId);
    const registration = (async () => {
      await preceding?.catch(() => {});
      const generations = this.sessionGenerations.get(sessionId) ?? new Set<string>();
      if (generations.has(transport.generationId)) return;
      for (const id of generations) {
        const previous = this.generations.get(id)!;
        if (!(await Promise.race([
          previous.waitForTerminationProof(), Bun.sleep(20_000).then(() => false),
        ]))) throw new Error("Previous Agent generation is not proven stopped");
      }
      generations.add(transport.generationId);
      this.sessionGenerations.set(sessionId, generations);
      const associated = this.generationSessions.get(transport.generationId) ?? new Set<string>();
      associated.add(sessionId);
      this.generationSessions.set(transport.generationId, associated);
    })();
    this.registrations.set(sessionId, registration);
    try { await registration; }
    finally {
      if (this.registrations.get(sessionId) === registration)
        this.registrations.delete(sessionId);
    }
  }

  private accept(socket: Socket): void {
    let input = "";
    socket.setTimeout(25_000, () => socket.destroy());
    socket.on("data", (chunk: Buffer) => {
      input += chunk.toString("utf8");
      if (input.length > MAX_QUERY_BYTES) { socket.destroy(); return; }
      const newline = input.indexOf("\n");
      if (newline < 0) return;
      socket.removeAllListeners("data");
      void this.answer(socket, input.slice(0, newline));
    });
    socket.on("error", () => socket.destroy());
  }

  private async answer(socket: Socket, line: string): Promise<void> {
    let sessionId: string | undefined;
    let generationId: string | undefined;
    try {
      const value = JSON.parse(line) as { token?: unknown; sessionId?: unknown; generationId?: unknown };
      if (value.token === this.token && typeof value.sessionId === "string" &&
          typeof value.generationId === "string") {
        sessionId = value.sessionId;
        generationId = value.generationId;
      }
    } catch { /* invalid query */ }
    const associated = generationId ? this.generationSessions.get(generationId) : undefined;
    const generation = sessionId && generationId && (!associated || associated.has(sessionId))
      ? this.generations.get(generationId) : undefined;
    const proven = generation
      ? await Promise.race([generation.waitForTerminationProof(), Bun.sleep(20_000).then(() => false)])
      : false;
    socket.end(JSON.stringify({ proven }) + "\n");
  }
}
