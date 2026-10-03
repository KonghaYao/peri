import { createServer, type Server, type Socket } from "node:net";
import { chmodSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { execFileSync } from "node:child_process";

const ACK = 0x06;
const HELLO_BYTES = 65;
const REGISTER_BYTES = 5;
const ADMISSION_TIMEOUT_MS = 5_000;
const STOP_TIMEOUT_MS = 10_000;

/** Trusted SDK owner for subprocess groups spawned by one ACP generation. */
export class ProcessBroker {
  readonly directory: string;
  readonly socketPath: string;
  readonly token: string;
  private readonly server: Server;
  private readonly sockets = new Set<Socket>();
  private readonly groups = new Set<number>();
  private readonly admissions = new Set<Socket>();
  private readonly verifications = new Set<Promise<void>>();
  private uncertain = false;
  private stopped = false;
  private settlement?: Promise<boolean>;

  private constructor(server: Server, directory: string, token: string) {
    this.server = server;
    this.directory = directory;
    this.socketPath = join(directory, "broker.sock");
    this.token = token;
  }

  static async start(): Promise<ProcessBroker> {
    const directory = mkdtempSync(join(tmpdir(), "peri-process-broker-"));
    chmodSync(directory, 0o700);
    const server = createServer();
    const broker = new ProcessBroker(server, directory, randomBytes(32).toString("hex"));
    server.on("connection", (socket) => broker.accept(socket));
    try {
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(broker.socketPath, () => {
          server.off("error", reject);
          resolve();
        });
      });
      chmodSync(broker.socketPath, 0o600);
      return broker;
    } catch (error) {
      server.close();
      rmSync(directory, { recursive: true, force: true });
      throw error;
    }
  }

  private accept(socket: Socket): void {
    if (this.stopped) { socket.destroy(); return; }
    this.sockets.add(socket);
    this.admissions.add(socket);
    let phase: "hello" | "register" | "done" | "settling" | "released" = "hello";
    let registeredPid: number | undefined;
    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        if (phase === "hello") {
          if (buffer.length < HELLO_BYTES) return;
          if (buffer[0] !== 0x42 || buffer.subarray(1, HELLO_BYTES).toString() !== this.token) {
            socket.destroy(); return;
          }
          buffer = buffer.subarray(HELLO_BYTES);
          phase = "register";
          socket.write(Buffer.from([ACK]));
        } else if (phase === "register") {
          if (buffer.length < REGISTER_BYTES) return;
          const pid = buffer.readUInt32LE(1);
          if (buffer[0] !== 0x50 || pid <= 1 || pid > 0x7fffffff) {
            socket.destroy(); return;
          }
          this.groups.add(pid);
          registeredPid = pid;
          this.admissions.delete(socket);
          buffer = buffer.subarray(REGISTER_BYTES);
          phase = "done";
          socket.write(Buffer.from([ACK]));
        } else if (phase === "done") {
          if (buffer.length < 1) return;
          if (buffer[0] !== 0x45 || buffer.length !== 1 || registeredPid === undefined) {
            socket.destroy(); return;
          }
          buffer = Buffer.alloc(0);
          phase = "settling";
          void this.releaseAnchoredGroup(socket, registeredPid).then((released) => {
            if (!released || socket.destroyed) return;
            phase = "released";
            socket.write(Buffer.from([ACK]));
          });
          return;
        } else {
          if (buffer.length) socket.destroy();
          return;
        }
      }
    });
    socket.on("close", () => {
      this.sockets.delete(socket);
      this.admissions.delete(socket);
      if (registeredPid !== undefined && phase !== "released" && !this.stopped) {
        // The group anchor disappeared without a normal release. Its numeric
        // PGID may now be recycled; never signal it during a later takeover.
        this.groups.delete(registeredPid);
        const verification = this.verifyUnanchoredExit(registeredPid);
        this.verifications.add(verification);
        void verification.finally(() => this.verifications.delete(verification));
      }
    });
    socket.on("error", () => socket.destroy());
  }

  private async verifyUnanchoredExit(pid: number): Promise<void> {
    const deadline = Date.now() + 5_000;
    while (Date.now() < deadline) {
      const members = groupMembers(pid);
      if (members?.length === 0) return;
      if (!members) break;
      await Bun.sleep(50);
    }
    this.uncertain = true;
  }

  private async releaseAnchoredGroup(socket: Socket, pid: number): Promise<boolean> {
    while (!this.stopped && !socket.destroyed) {
      const members = groupMembers(pid);
      if (members?.length === 1 && members[0] === pid) {
        // The anchor still owns this PGID while the registry drops it. The
        // release ACK lets the anchor exit only after this deletion.
        this.groups.delete(pid);
        return true;
      }
      if (!members) {
        this.uncertain = true;
        // Membership is unknown, so the numeric PGID cannot be trusted.
        // Keep the anchor blocked and withhold recovery proof.
        return false;
      }
      await Bun.sleep(50);
    }
    return false;
  }

  /** Called only after Bun has observed the matching ACP child exit. */
  settleAfterAgentExit(): Promise<boolean> {
    return this.settlement ??= this.settle();
  }

  private async settle(): Promise<boolean> {
    this.stopped = true;
    this.server.close();
    try {
      const deadline = Date.now() + ADMISSION_TIMEOUT_MS;
      while (this.admissions.size && Date.now() < deadline) {
        await Bun.sleep(20);
      }
      await Promise.all(this.verifications);
      if (this.admissions.size || this.uncertain) return false;
      for (const pid of this.groups) {
        try { process.kill(-pid, "SIGKILL"); }
        catch (error) { if (!isAbsent(error)) return false; }
      }
      const stopDeadline = Date.now() + STOP_TIMEOUT_MS;
      while (Date.now() < stopDeadline) {
        if ([...this.groups].every((pid) => groupAbsent(pid))) return true;
        await Bun.sleep(20);
      }
      return false;
    } finally {
      rmSync(this.directory, { recursive: true, force: true });
    }
  }
}

function groupMembers(group: number): number[] | undefined {
  try {
    const output = execFileSync("ps", ["-axo", "pid=,pgid="], {
      encoding: "utf8", timeout: 1000, maxBuffer: 4 * 1024 * 1024,
    });
    return output.split("\n").flatMap((line) => {
      const [pid, pgid] = line.trim().split(/\s+/).map(Number);
      return pgid === group && Number.isSafeInteger(pid) ? [pid!] : [];
    });
  } catch { return undefined; }
}

function isAbsent(error: unknown): boolean {
  return (error as NodeJS.ErrnoException)?.code === "ESRCH";
}

function groupAbsent(pid: number): boolean {
  try { process.kill(-pid, 0); return false; }
  catch (error) { return isAbsent(error); }
}
