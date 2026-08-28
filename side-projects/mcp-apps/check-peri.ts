import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const projectDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(projectDir, "../..");
const fixtureServer = path.join(projectDir, "stdio-server.ts");
const tsxCli = path.join(projectDir, "node_modules", "tsx", "dist", "cli.mjs");
const serverId = "official-apps-fixture";
const toolName = "get-time";
const effectiveToolName = `mcp__${serverId}__${toolName}`;
const invocationToken = "fixture-model-call-1";
const resourceUri = "ui://get-time/mcp-app.html";
const envelopeVersion = "1";
const appsProtocolVersion = "2026-01-26";
const appRequestId = "app-call-1";

interface JsonRpcResponse {
  id?: number;
  result?: Record<string, unknown>;
  error?: { code: number; message: string; data?: { kind?: string } };
}

function invariant(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function findToolCallId(value: unknown, title: string): string | undefined {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findToolCallId(item, title);
      if (found) return found;
    }
    return undefined;
  }
  if (!value || typeof value !== "object") return undefined;
  const object = value as Record<string, unknown>;
  if (object.title === title && typeof object.toolCallId === "string") return object.toolCallId;
  for (const child of Object.values(object)) {
    const found = findToolCallId(child, title);
    if (found) return found;
  }
  return undefined;
}

function hasCompletedToolCall(value: unknown, toolCallId: string): boolean {
  if (Array.isArray(value)) return value.some((item) => hasCompletedToolCall(item, toolCallId));
  if (!value || typeof value !== "object") return false;
  const object = value as Record<string, unknown>;
  if (object.toolCallId === toolCallId && object.status === "completed") return true;
  return Object.values(object).some((child) => hasCompletedToolCall(child, toolCallId));
}

class AcpClient {
  private readonly pending = new Map<
    number,
    { resolve: (response: JsonRpcResponse) => void; reject: (error: Error) => void; timeout: NodeJS.Timeout }
  >();
  private readonly notifications: Record<string, unknown>[] = [];
  private readonly waiters: Array<{
    predicate: (message: Record<string, unknown>) => boolean;
    resolve: (message: Record<string, unknown>) => void;
    timeout: NodeJS.Timeout;
  }> = [];
  private readonly lines: readline.Interface;
  private stderr = "";

  constructor(readonly process: ChildProcessWithoutNullStreams) {
    process.stderr.on("data", (chunk) => {
      this.stderr += String(chunk);
    });
    this.lines = readline.createInterface({ input: process.stdout });
    this.lines.on("line", (line) => {
      let response: JsonRpcResponse;
      try {
        response = JSON.parse(line) as JsonRpcResponse;
      } catch {
        return;
      }
      if (typeof response.id !== "number") {
        const notification = response as Record<string, unknown>;
        this.notifications.push(notification);
        for (let index = this.waiters.length - 1; index >= 0; index -= 1) {
          const waiter = this.waiters[index];
          if (!waiter.predicate(notification)) continue;
          clearTimeout(waiter.timeout);
          this.waiters.splice(index, 1);
          waiter.resolve(notification);
        }
        return;
      }
      const pending = this.pending.get(response.id);
      if (!pending) return;
      clearTimeout(pending.timeout);
      this.pending.delete(response.id);
      pending.resolve(response);
    });
    process.once("exit", (code, signal) => {
      const error = new Error(`Peri exited: code=${String(code)} signal=${String(signal)}\n${this.stderr}`);
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timeout);
        pending.reject(error);
      }
      this.pending.clear();
    });
  }

  request(id: number, method: string, params: unknown): Promise<JsonRpcResponse> {
    const response = new Promise<JsonRpcResponse>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`timed out waiting for ACP response ${id}\n${this.stderr}`));
      }, 120_000);
      this.pending.set(id, { resolve, reject, timeout });
    });
    this.process.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return response;
  }

  waitForNotification(
    predicate: (message: Record<string, unknown>) => boolean,
    description: string,
  ): Promise<Record<string, unknown>> {
    const existing = this.notifications.find(predicate);
    if (existing) return Promise.resolve(existing);
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        const index = this.waiters.findIndex((waiter) => waiter.resolve === resolve);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new Error(`timed out waiting for ${description}\n${this.stderr}`));
      }, 120_000);
      this.waiters.push({ predicate, resolve, timeout });
    });
  }

  diagnostics() {
    return this.stderr;
  }

  close() {
    this.lines.close();
    this.process.stdin.end();
    this.process.kill("SIGTERM");
  }
}

async function startModelServer() {
  let calls = 0;
  const server = http.createServer((request, response) => {
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      response.writeHead(404).end();
      return;
    }
    request.resume();
    request.on("end", () => {
      calls += 1;
      response.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      });
      const chunk =
        calls === 1
          ? {
              id: "fixture-model-tool-call",
              choices: [
                {
                  delta: {
                    tool_calls: [
                      {
                        index: 0,
                        id: invocationToken,
                        function: { name: effectiveToolName, arguments: "{}" },
                      },
                    ],
                  },
                  finish_reason: "tool_calls",
                },
              ],
            }
          : {
              id: "fixture-model-complete",
              choices: [{ delta: { content: "fixture complete" }, finish_reason: "stop" }],
            };
      response.write(`data: ${JSON.stringify(chunk)}\n\n`);
      response.end("data: [DONE]\n\n");
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  invariant(address && typeof address === "object", "mock model server did not bind a TCP port");
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    calls: () => calls,
    close: () => new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve()))),
  };
}

async function writeWorkspace(workspace: string, modelBaseUrl: string) {
  await fs.writeFile(
    path.join(workspace, ".mcp.json"),
    JSON.stringify({
      mcpServers: {
        [serverId]: { command: process.execPath, args: [tsxCli, fixtureServer] },
      },
    }),
  );
  const settingsPath = path.join(workspace, "settings.json");
  await fs.writeFile(
    settingsPath,
    JSON.stringify({
      config: {
        active_alias: "sonnet",
        providers: [
          {
            id: "fixture-model",
            type: "openai",
            apiKey: "fixture-not-a-secret",
            baseUrl: modelBaseUrl,
            models: { sonnet: "fixture-model" },
          },
        ],
        profiles: {
          sonnet: { provider: "fixture-model", model: "fixture-model", effort: "medium", max_tokens: 1024 },
        },
      },
    }),
  );
  return settingsPath;
}

function startPeri(workspace: string, settingsPath: string, appsEnabled: boolean) {
  const env = { ...process.env };
  if (appsEnabled) env.PERI_MCP_APPS = "";
  else delete env.PERI_MCP_APPS;
  delete env.ANTHROPIC_API_KEY;
  delete env.OPENAI_API_KEY;
  const child = spawn(
    "cargo",
    [
      "run",
      "-q",
      "-p",
      "peri-tui",
      "--",
      "--config-file",
      settingsPath,
      "acp",
      "--cwd",
      workspace,
    ],
    { cwd: repoRoot, env, stdio: ["pipe", "pipe", "pipe"] },
  );
  return new AcpClient(child);
}

function expectResult(response: JsonRpcResponse, context: string): Record<string, unknown> {
  invariant(response.result, `${context} failed: ${JSON.stringify(response.error)}`);
  return response.result;
}

async function runSuccessfulRelay(workspace: string, settingsPath: string) {
  const client = startPeri(workspace, settingsPath, true);
  try {
    expectResult(
      await client.request(1, "initialize", {
        protocolVersion: 1,
        clientCapabilities: {},
        clientInfo: { name: "mcp-apps-e2e", version: "1.0.0" },
      }),
      "initialize",
    );
    await new Promise((resolve) => setTimeout(resolve, 2_000));

    const session = expectResult(await client.request(2, "session/new", { cwd: workspace }), "session/new");
    const sessionId = session.sessionId;
    invariant(typeof sessionId === "string", "session/new did not return sessionId");

    const toolStarted = client.waitForNotification(
      (message) => findToolCallId(message, effectiveToolName) !== undefined,
      "canonical MCP ToolCall notification",
    );
    const promptResponse = client.request(3, "session/prompt", {
      sessionId,
      prompt: [{ type: "text", text: "Call the MCP Apps fixture tool exactly once." }],
    });
    const startedMessage = await toolStarted;
    const observedInvocationToken = findToolCallId(startedMessage, effectiveToolName);
    invariant(typeof observedInvocationToken === "string", "ToolCall notification did not expose toolCallId");
    invariant(
      observedInvocationToken === invocationToken,
      "ACP wire changed the MCP tool invocation token",
    );
    const toolCompleted = client.waitForNotification(
      (message) => hasCompletedToolCall(message, observedInvocationToken),
      "completed canonical MCP ToolCallUpdate notification",
    );
    await toolCompleted;

    const openResponse = await client.request(4, "peri/mcp/open", {
      envelopeVersion,
      appsProtocolVersion,
      serverId,
      ownerSessionId: sessionId,
      invocationToken: observedInvocationToken,
      toolName,
    });
    const opened = expectResult(
      openResponse,
      "peri/mcp/open",
    );
    invariant(typeof opened.appSessionId === "string", "open response is missing appSessionId");
    invariant(opened.resourceUri === resourceUri, "open response changed resourceUri");
    const appSessionId = opened.appSessionId;

    const resource = expectResult(
      await client.request(5, "peri/mcp/resource", {
        envelopeVersion,
        appsProtocolVersion,
        serverId,
        appSessionId,
        resourceUri,
      }),
      "peri/mcp/resource",
    );
    const resources = resource.resources;
    invariant(Array.isArray(resources) && resources.length === 1, "resource relay did not preserve contents[]");
    const html = resources[0] as Record<string, unknown>;
    invariant(html.uri === resourceUri, "resource relay changed uri");
    invariant(html.mimeType === "text/html;profile=mcp-app", "resource relay changed MIME");
    invariant(typeof html.text === "string" && html.text.includes("<!DOCTYPE html>"), "resource relay lost HTML text");
    const resourceMeta = html._meta as { ui?: { csp?: { connectDomains?: unknown[]; resourceDomains?: unknown[] } } };
    invariant(resourceMeta.ui?.csp?.connectDomains?.length === 0, "resource relay lost _meta.ui.csp.connectDomains");
    invariant(resourceMeta.ui?.csp?.resourceDomains?.length === 0, "resource relay lost _meta.ui.csp.resourceDomains");

    const app = expectResult(
      await client.request(6, "peri/mcp/app", {
        envelopeVersion,
        appsProtocolVersion,
        serverId,
        appSessionId,
        resourceUri,
        payload: {
          jsonrpc: "2.0",
          id: appRequestId,
          method: "tools/call",
          params: { name: toolName, arguments: {} },
        },
      }),
      "peri/mcp/app",
    );
    const payload = app.payload as { id?: unknown; result?: Record<string, unknown> };
    invariant(payload.id === appRequestId, "App JSON-RPC request id did not round-trip");
    invariant(payload.result, "App tools/call is missing result");
    invariant(payload.result.isError !== true, "App tools/call returned isError=true");
    const content = payload.result.content;
    invariant(Array.isArray(content) && content.length === 1, "App tools/call lost content[]");
    invariant(
      (content[0] as { type?: string; text?: string }).type === "text" &&
        typeof (content[0] as { text?: string }).text === "string",
      "App tools/call lost text fallback",
    );
    const structured = payload.result.structuredContent as { iso?: string } | undefined;
    invariant(typeof structured?.iso === "string" && !Number.isNaN(Date.parse(structured.iso)), "App tools/call lost structuredContent.iso");
    invariant(
      (payload.result._meta as { fixture?: string } | undefined)?.fixture === "peri-mcp-apps",
      "App tools/call lost result _meta",
    );
    expectResult(await promptResponse, "session/prompt");

    return { sessionId, appSessionId, resourceCount: resources.length, appRequestId };
  } finally {
    client.close();
  }
}

async function runDisabledProbe(workspace: string, settingsPath: string) {
  const client = startPeri(workspace, settingsPath, false);
  try {
    expectResult(
      await client.request(10, "initialize", {
        protocolVersion: 1,
        clientCapabilities: {},
        clientInfo: { name: "mcp-apps-disabled-check", version: "1.0.0" },
      }),
      "disabled initialize",
    );
    const opened = await client.request(11, "peri/mcp/open", {
      envelopeVersion,
      appsProtocolVersion,
      serverId,
      ownerSessionId: "disabled",
      invocationToken: "disabled",
      toolName,
    });
    invariant(opened.error?.data?.kind === "capability_disabled", "missing PERI_MCP_APPS did not disable relay");
    return opened.error.data.kind;
  } finally {
    client.close();
  }
}

const workspace = await fs.mkdtemp(path.join(os.tmpdir(), "peri-mcp-apps-e2e-"));
const model = await startModelServer();
try {
  const settingsPath = await writeWorkspace(workspace, model.baseUrl);
  const success = await runSuccessfulRelay(workspace, settingsPath);
  invariant(model.calls() >= 2, "canonical session/prompt did not execute the model/tool loop");
  const disabledProbe = await runDisabledProbe(workspace, settingsPath);
  process.stdout.write(JSON.stringify({ ok: true, ...success, modelCalls: model.calls(), disabledProbe }, null, 2) + "\n");
} finally {
  await model.close();
  await fs.rm(workspace, { recursive: true, force: true });
}
