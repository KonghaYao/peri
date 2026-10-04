import { StdioTransport } from "../transport/stdio-transport";
import type { StdioTransportOptions } from "../transport/types";
import type { Transport } from "../transport/types";
import type { SessionStorage } from "../storage/types";
import type { SessionSummary } from "../storage/session-summary";
import { realpathSync } from "node:fs";
import { WorkspaceMcpProcess, type WorkspaceMcpProcessOptions } from "./workspace-mcp-process";
import { ProcessSupervisor } from "./process-supervisor";

export interface HttpWorkspace {
  url: string;
  headers?: Record<string, string>;
}

export interface SandboxOptions {
  id: string;
  /** Optional default path; Agent sessions may supply their own path. */
  path?: string;
  workspace?: HttpWorkspace;
  workspaceProcess?: WorkspaceMcpProcessOptions;
  storage?: SessionStorage;
  stdio?: Omit<StdioTransportOptions, "command"> & { command?: string };
  transportFactory?: (path: string) => Transport | Promise<Transport>;
}

/** Host-side workspace identity and local Peri process policy. */
export class Sandbox {
  readonly id: string;
  readonly path?: string;
  private readonly workspace?: HttpWorkspace;
  private readonly workspaceProcessOptions?: WorkspaceMcpProcessOptions;
  private workspaceProcess?: WorkspaceMcpProcess;
  private workspacePath?: string;
  private workspaceStart?: Promise<HttpWorkspace>;
  private readonly storage?: SessionStorage;
  private readonly stdio?: SandboxOptions["stdio"];
  private readonly transportFactory?: SandboxOptions["transportFactory"];
  private supervisorStart?: Promise<ProcessSupervisor>;

  constructor(options: SandboxOptions) {
    if (!options.id)
      throw new TypeError("Sandbox id is required");
    if (options.workspace && options.workspaceProcess)
      throw new TypeError("Choose an external Workspace or a managed Workspace process");
    this.id = options.id;
    if (options.path) {
      // External Workspace paths belong to the remote environment.
      if (options.workspace) this.path = options.path;
      else {
        try { this.path = realpathSync(options.path); }
        catch { this.path = options.path; }
      }
    }
    this.workspace = options.workspace;
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

  getSessions(path = this.path): Promise<SessionSummary[]> {
    if (!this.storage) throw new Error("Sandbox has no Session Storage configured");
    if (!path) throw new Error("Sandbox session path is required");
    return this.storage.getSessions(path);
  }

  getSession(id: string): Promise<SessionSummary | null> {
    if (!this.storage) throw new Error("Sandbox has no Session Storage configured");
    return this.storage.getSession(id);
  }

  get optionalWorkspace(): HttpWorkspace | undefined {
    return this.workspace ?? (this.workspaceProcess ? { url: this.workspaceProcess.url } : undefined);
  }

  /** Start and discover the Sandbox-owned MCP process before ACP session setup. */
  startWorkspace(path: string): Promise<HttpWorkspace> {
    if (this.workspace) return Promise.resolve(this.workspace);
    if (!this.workspaceProcessOptions)
      throw new Error("Sandbox has no Workspace process configured");
    if (this.workspacePath && this.workspacePath !== path)
      throw new Error("Managed Workspace is already bound to another path");
    if (this.workspaceProcess) return Promise.resolve(this.optionalWorkspace!);
    if (!this.workspaceStart) {
      this.workspacePath = path;
      this.workspaceStart = WorkspaceMcpProcess.start(path, this.workspaceProcessOptions)
        .then((process) => {
          this.workspaceProcess = process;
          return { url: process.url };
        })
        .catch((error) => {
          this.workspacePath = undefined;
          throw error;
        })
        .finally(() => { this.workspaceStart = undefined; });
    }
    return this.workspaceStart;
  }

  async closeWorkspace(): Promise<void> {
    await this.workspaceStart?.catch(() => {});
    const process = this.workspaceProcess;
    this.workspaceProcess = undefined;
    await process?.close();
    this.workspacePath = undefined;
  }

  async createTransport(path: string): Promise<Transport> {
    if (this.workspaceProcessOptions) await this.startWorkspace(path);
    if (this.transportFactory) return Promise.resolve(this.transportFactory(path));
    return this.createStdioTransport(path);
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

  private async createStdioTransport(path: string): Promise<StdioTransport> {
    const supervisor = process.platform === "win32" ? undefined : await this.processSupervisor();
    const trustedWorkspace = this.optionalWorkspace;
    const transport = this.stdio;
    const deployment = this.storage?.deployment();
    const args = [...(deployment?.args ?? []), ...(transport?.args ?? [])];
    args.push("acp");
    if (transport?.settings !== undefined) args.push("--settings-stdin");
    args.push("--cwd", path);
    const child = await StdioTransport.start({
      command: transport?.command ?? "peri",
      args,
      cwd: transport?.cwd ?? (this.workspace ? undefined : path),
      env: {
        ...transport?.env, ...deployment?.env,
        ...(supervisor ? {
          PERI_SUPERVISOR_SOCKET: supervisor.socketPath,
          PERI_SUPERVISOR_TOKEN: supervisor.token,
        } : {}),
        ...(trustedWorkspace ? { PERI_TRUSTED_WORKSPACE_URL: trustedWorkspace.url } : {}),
      },
      settings: transport?.settings,
    });
    supervisor?.registerGeneration(child);
    return child;
  }
}
