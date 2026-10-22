import { describe, expect, test } from "bun:test";
import { PeriWasmHostLease, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import type { AgentEnvironment, McpAttachment, StartHost } from "../types";
import { SessionCreator } from "./creation";

const chatId = "11111111-1111-4111-8111-111111111111";
const title = "新会话";

describe("SessionCreator", () => {
  test("创建会话：session/new + rename 后返回 chatId 并释放宿主", async () => {
    const requests: { method: string; params: unknown }[] = [];
    let mcpClosed = 0;
    let hostClosed = 0;
    const transport = {
      generationId: 1,
      setRequestHandler: () => {},
      request: async (method: string, params: unknown) => {
        requests.push({ method, params });
        if (method === "session/new") return { sessionId: chatId };
        if (method === "session/rename")
          return { sessionId: chatId, title: (params as { title: string }).title };
        throw new Error(`unexpected ACP request: ${method}`);
      },
      close: async () => { hostClosed += 1; },
    } as unknown as WasmAcpTransport;
    const attachment: McpAttachment = {
      servers: [{ type: "http", name: "remote", url: "https://mcp.example.com/mcp", headers: [] }],
      handleMessage: () => { throw new Error("unexpected reverse request"); },
      freeze: () => {},
      close: async () => { mcpClosed += 1; },
    };
    const environment: AgentEnvironment = {
      cwd: "/workspace/",
      instructions: "test instructions",
      initialize: async () => {},
      openMcp: () => attachment,
      logError: () => { throw new Error("creation should not report an error"); },
    };
    let starts = 0;
    const startHost: StartHost = async () => { starts += 1; return transport; };

    const created = await new SessionCreator(
      (options) => PeriWasmHostLease.start(startHost, options), environment).create(title);

    expect(created).toBe(chatId);
    expect(starts).toBe(1);
    expect(requests.map((request) => request.method)).toEqual(["session/new", "session/rename"]);
    expect(requests[0]!.params).toEqual({
      cwd: environment.cwd,
      mcpServers: attachment.servers,
      _meta: { "peri.instructions": environment.instructions },
    });
    expect(requests[1]!.params).toEqual({ sessionId: chatId, title });
    expect(mcpClosed).toBe(1);
    expect(hostClosed).toBe(1);
  });
});
