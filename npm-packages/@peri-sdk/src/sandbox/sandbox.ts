import { StdioTransport } from "../transport/stdio-transport";
import type { StdioTransportOptions } from "../transport/types";
import type { Transport } from "../transport/types";
import type { SessionStorage } from "../storage/types";
import type { SessionSummary } from "../storage/session-summary";
import { realpathSync } from "node:fs";
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, statSync, writeSync } from "node:fs";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { WorkspaceMcpProcess, type WorkspaceMcpProcessOptions } from "./workspace-mcp-process";
import { ProcessSupervisor } from "./process-supervisor";

export interface HttpWorkspace {
  url: string;
  headers?: Record<string, string>;
  /** Trusted host configuration, shared with a separately deployed Workspace owner. */
  taskScopeSecretFile?: string;
}

export interface SandboxOptions {
  id: string;
  path: string;
  workspace?: HttpWorkspace;
  workspaceProcess?: WorkspaceMcpProcessOptions;
  storage?: SessionStorage;
  stdio?: Omit<StdioTransportOptions, "cwd" | "command"> & { command?: string };
  transportFactory?: () => Transport | Promise<Transport>;
}

/** Host-side workspace identity and local Peri process policy. */
export class Sandbox {
  readonly id: string;
  readonly path: string;
  private readonly workspace?: HttpWorkspace;
  private readonly workspaceProcessOptions?: WorkspaceMcpProcessOptions;
  private workspaceProcess?: WorkspaceMcpProcess;
  private workspaceStart?: Promise<HttpWorkspace>;
  private managedTaskScopeSecretFile?: string;
  private readonly storage?: SessionStorage;
  private readonly stdio?: SandboxOptions["stdio"];
  private readonly transportFactory?: SandboxOptions["transportFactory"];
  private supervisorStart?: Promise<ProcessSupervisor>;

  constructor(options: SandboxOptions) {
    if (!options.id || !options.path)
      throw new TypeError("Sandbox id and path are required");
    this.id = options.id;
    // ACP session/list filters by canonical cwd; use the same cwd at setup and listing.
    try {
      this.path = realpathSync(options.path);
    } catch {
      this.path = options.path;
    } // A remote Sandbox path need not exist on this host.
    this.workspace = options.workspace;
    if (options.workspace && options.workspaceProcess)
      throw new TypeError("Choose an external Workspace or a managed Workspace process");
    this.workspaceProcessOptions = options.workspaceProcess;
    this.storage = options.storage;
    this.stdio = options.stdio;
    this.transportFactory = options.transportFactory;
  }

  getWorkspace(): HttpWorkspace {
    const workspace = this.optionalWorkspace;
    if (!workspace)
      throw new Error("Sandbox has no HTTP Workspace configured");
    return workspace;
  }

  getSessions(): Promise<SessionSummary[]> {
    if (!this.storage) throw new Error("Sandbox has no Session Storage configured");
    return this.storage.getSessions(this.path);
  }

  get optionalWorkspace(): HttpWorkspace | undefined {
    return this.workspace ?? (this.workspaceProcess ? {
      url: this.workspaceProcess.url,
      taskScopeSecretFile: this.managedTaskScopeSecretFile,
    } : undefined);
  }

  /** Start and discover the Sandbox-owned MCP process before ACP session setup. */
  startWorkspace(): Promise<HttpWorkspace> {
    if (this.workspace) return Promise.resolve(this.workspace);
    if (!this.workspaceProcessOptions)
      throw new Error("Sandbox has no Workspace process configured");
    if (this.workspaceProcess) return Promise.resolve(this.optionalWorkspace!);
    if (!this.workspaceStart) {
      const secretFile = this.prepareTaskScopeSecret();
      this.workspaceStart = WorkspaceMcpProcess.start(this.path, this.workspaceProcessOptions, secretFile)
        .then((process) => {
          this.workspaceProcess = process;
          return { url: process.url, taskScopeSecretFile: secretFile };
        })
        .finally(() => { this.workspaceStart = undefined; });
    }
    return this.workspaceStart;
  }

  private prepareTaskScopeSecret(): string {
    if (this.managedTaskScopeSecretFile) return this.managedTaskScopeSecretFile;
    const directory = join(this.path, ".peri");
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const file = join(directory, "task-scope.secret");
    if (!existsSync(file)) {
      const fd = openSync(file, "wx", 0o600);
      try { writeSync(fd, randomBytes(32)); fsyncSync(fd); }
      finally { closeSync(fd); }
      const dirFd = openSync(directory, "r");
      try { fsyncSync(dirFd); } finally { closeSync(dirFd); }
    }
    const stat = statSync(file);
    if (!stat.isFile() || stat.size !== 32 || (stat.mode & 0o077) !== 0 || readFileSync(file).length !== 32)
      throw new Error("Managed Workspace task scope secret is invalid");
    this.managedTaskScopeSecretFile = file;
    return file;
  }

  async closeWorkspace(): Promise<void> {
    await this.workspaceStart?.catch(() => {});
    const process = this.workspaceProcess;
    this.workspaceProcess = undefined;
    await process?.close();
  }

  async createTransport(): Promise<Transport> {
    if (this.workspaceProcessOptions) await this.startWorkspace();
    if (this.transportFactory) return Promise.resolve(this.transportFactory());
    return this.createStdioTransport();
  }

  /** Bind an ACP child generation after session setup has returned its identity. */
  async registerSessionTransport(sessionId: string, transport: Transport): Promise<void> {
    if (!(transport instanceof StdioTransport)) return;
    const supervisor = await this.processSupervisor();
    await supervisor.register(sessionId, transport);
  }

  private processSupervisor(): Promise<ProcessSupervisor> {
    return this.supervisorStart ??= ProcessSupervisor.start();
  }

  private async createStdioTransport(): Promise<StdioTransport> {
    const supervisor = process.platform === "win32" ? undefined : await this.processSupervisor();
    const trustedWorkspace = this.optionalWorkspace;
    const transport = this.stdio;
    const deployment = this.storage?.deployment();
    const args = [...(deployment?.args ?? []), ...(transport?.args ?? [])];
    args.push("acp");
    if (transport?.settings !== undefined) args.push("--settings-stdin");
    args.push("--cwd", this.path);
    const child = await StdioTransport.start({
      command: transport?.command ?? "peri",
      args,
      cwd: this.path,
      env: {
        ...transport?.env, ...deployment?.env,
        ...(supervisor ? {
          PERI_SUPERVISOR_SOCKET: supervisor.socketPath,
          PERI_SUPERVISOR_TOKEN: supervisor.token,
        } : {}),
        ...(trustedWorkspace?.taskScopeSecretFile ? {
          PERI_TRUSTED_WORKSPACE_URL: trustedWorkspace.url,
          PERI_TRUSTED_WORKSPACE_SCOPE_SECRET_FILE: trustedWorkspace.taskScopeSecretFile,
        } : {}),
      },
      settings: transport?.settings,
    });
    supervisor?.registerGeneration(child);
    return child;
  }
}
