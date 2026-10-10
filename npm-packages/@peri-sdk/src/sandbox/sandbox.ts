import type { Transport } from "../transport/types";
import type { SessionStorage } from "../storage/types";
import type { SessionSummary } from "../storage/session-summary";

export interface HttpWorkspace {
  url: string;
  headers?: Record<string, string>;
}

export interface SandboxOptions {
  id: string;
  /** Optional default path; Agent sessions may supply their own path. */
  path?: string;
  workspace?: HttpWorkspace;
  storage?: SessionStorage;
  /**
   * 宿主拥有的 ACP transport；SDK 不启动任何 Peri 或 Workspace 进程。
   * WASM 部署用 `startPeriWasmHost` 装配，其他部署注入自有实现。
   */
  transportFactory: (path: string) => Transport | Promise<Transport>;
}

/** 工作区身份、Store 读取与 ACP transport 装配点。 */
export class Sandbox {
  readonly id: string;
  readonly path?: string;
  private readonly workspace?: HttpWorkspace;
  private readonly storage?: SessionStorage;
  private readonly transportFactory: SandboxOptions["transportFactory"];

  constructor(options: SandboxOptions) {
    if (!options.id) throw new TypeError("Sandbox id is required");
    if (typeof options.transportFactory !== "function")
      throw new TypeError("Sandbox transportFactory is required; the SDK does not start ACP processes");
    this.id = options.id;
    this.path = options.path;
    this.workspace = options.workspace;
    this.storage = options.storage;
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
    return this.workspace;
  }

  createTransport(path: string): Promise<Transport> {
    return Promise.resolve(this.transportFactory(path));
  }
}
