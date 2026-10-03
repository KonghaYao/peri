export interface WorkspaceMcpProcessOptions {
  /** Peri dispatcher installed beside peri-mcp-workspace. */
  command?: string;
  /** Fixed loopback address, for example 127.0.0.1:8765. */
  bind: string;
  env?: Record<string, string>;
}

// MCP 2026-07-28 has no initialize request; use the newest handshake revision
// for startup discovery. Peri establishes its own MCP connection afterward.
const HANDSHAKE_VERSION = "2025-11-25";

type RpcResponse = {
  result?: Record<string, unknown>;
  error?: { message?: string };
};

/** Sandbox-owned Workspace MCP process; Agent closure does not stop it. */
export class WorkspaceMcpProcess {
  readonly url: string;
  private readonly child: Bun.Subprocess<"ignore", "ignore", "pipe">;
  private readonly diagnostics: string[] = [];
  private exitCode?: number;

  private constructor(
    child: Bun.Subprocess<"ignore", "ignore", "pipe">,
    bind: string,
  ) {
    this.child = child;
    this.url = `http://${bind}/mcp`;
    void this.readStderr();
    void child.exited.then((code) => { this.exitCode = code; });
  }

  static async start(
    path: string,
    options: WorkspaceMcpProcessOptions,
    taskScopeSecretFile: string,
  ): Promise<WorkspaceMcpProcess> {
    const address = new URL(`http://${options.bind}/mcp`);
    if (!address.port || address.pathname !== "/mcp")
      throw new TypeError("Workspace bind must include a TCP port");
    // The tool process needs only its execution environment. Provider and
    // Store credentials belong to the Peri process, not Workspace Bash.
    const env: Record<string, string> = {};
    for (const key of ["PATH", "HOME", "TMPDIR", "LANG", "USER", "SHELL", "SystemRoot", "WINDIR", "PATHEXT"])
      if (process.env[key]) env[key] = process.env[key]!;
    Object.assign(env, options.env);
    const child = Bun.spawn([
      options.command ?? "peri", "mcp-start", "workspace", "--http",
      "--workspace", path, "--bind", options.bind,
      "--task-scope-secret-file", taskScopeSecretFile,
    ], {
      cwd: path,
      env,
      stdin: "ignore",
      stdout: "ignore",
      stderr: "pipe",
    });
    const workspace = new WorkspaceMcpProcess(child, options.bind);
    try {
      await workspace.waitUntilReady();
      return workspace;
    } catch (error) {
      await workspace.close();
      throw error;
    }
  }

  private async readStderr(): Promise<void> {
    if (!this.child.stderr) return;
    try {
      for await (const chunk of this.child.stderr) {
        this.diagnostics.push(new TextDecoder().decode(chunk));
        if (this.diagnostics.length > 8) this.diagnostics.shift();
      }
    } catch {
      // The child may close stderr while Sandbox is stopping it.
    }
  }

  private async rpc(
    method: string,
    id: number | undefined,
    sessionId?: string,
    params?: unknown,
  ): Promise<{ response: RpcResponse; sessionId?: string }> {
    const response = await fetch(this.url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
        "mcp-protocol-version": HANDSHAKE_VERSION,
        ...(sessionId ? { "mcp-session-id": sessionId } : {}),
      },
      body: JSON.stringify({ jsonrpc: "2.0", ...(id ? { id } : {}), method, ...(params === undefined ? {} : { params }) }),
      signal: AbortSignal.timeout(1_000),
    });
    if (!response.ok)
      throw new Error(`Workspace MCP ${method}: HTTP ${response.status}`);
    if (id === undefined) return { response: {}, sessionId };
    const body = await response.text();
    const json = response.headers.get("content-type")?.includes("text/event-stream")
      ? body.split("\n").filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trim()).find(Boolean)
      : body;
    if (!json) throw new Error(`Workspace MCP ${method}: empty response`);
    const parsed = JSON.parse(json) as RpcResponse;
    if (parsed.error)
      throw new Error(`Workspace MCP ${method}: ${parsed.error.message ?? "request failed"}`);
    return { response: parsed, sessionId: response.headers.get("mcp-session-id") ?? sessionId };
  }

  private async waitUntilReady(): Promise<void> {
    const deadline = Date.now() + 10_000;
    let lastError: unknown;
    while (Date.now() < deadline) {
      if (this.exitCode !== undefined)
        throw new Error(`Workspace MCP exited (${this.exitCode}): ${this.diagnostics.join("").slice(-500)}`);
      try {
        const initialized = await this.rpc("initialize", 1, undefined, {
          protocolVersion: HANDSHAKE_VERSION,
          capabilities: {},
          clientInfo: { name: "peri-sdk-workspace-probe", version: "0.0.0" },
        });
        if (initialized.response.result?.protocolVersion !== HANDSHAKE_VERSION)
          throw new Error("Workspace MCP negotiated an unexpected version");
        await this.rpc("notifications/initialized", undefined, initialized.sessionId);
        const listed = await this.rpc("tools/list", 2, initialized.sessionId);
        if (!Array.isArray(listed.response.result?.tools))
          throw new Error("Workspace MCP returned no tools list");
        if (this.exitCode !== undefined)
          throw new Error(`Workspace MCP exited (${this.exitCode}) after discovery`);
        return;
      } catch (error) {
        lastError = error;
        await Bun.sleep(100);
      }
    }
    throw new Error(`Workspace MCP did not become ready: ${String(lastError)}`);
  }

  async close(): Promise<void> {
    this.child.kill("SIGTERM");
    await this.child.exited;
  }
}
