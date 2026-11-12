import type { MCPServer } from "mcp-use";
import { z } from "zod";
import { statePayload } from "../state.ts";

/** 宿主信息与错误展示工具。 */
export function registerHostTools(server: MCPServer): void {
  server.tool(
    {
      name: "client_info",
      title: "Client info",
      description:
        "返回当前请求的客户端信息、能力声明、请求头与 server 状态，用于观察协议协商。",
      inputSchema: z.object({}),
      outputSchema: z.object({
        client: z.object({
          name: z.string().optional(),
          version: z.string().optional(),
        }),
        capabilities: z.object({
          sampling: z.boolean(),
          elicitation: z.boolean(),
          views: z.boolean(),
        }),
        request: z.object({
          userAgent: z.string(),
          path: z.string(),
        }),
        state: z.record(z.string(), z.unknown()),
      }),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: false,
      },
    },
    async (_input, ctx) => {
      const info = ctx.client.info();
      const data = {
        client: { name: info.name, version: info.version },
        capabilities: {
          sampling: ctx.client.can("sampling"),
          elicitation: ctx.client.can("elicitation"),
          views: ctx.client.supportsViews(),
        },
        request: {
          userAgent: ctx.request?.header("user-agent") ?? "unknown",
          path: ctx.request?.path ?? "unknown",
        },
        state: statePayload(),
      };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: data,
      };
    },
  );

  server.tool(
    {
      name: "fail_demo",
      title: "Fail demo",
      description: "故意返回 isError 结果，用于验证客户端错误展示。",
      inputSchema: z.object({
        message: z.string().default("demo failure").describe("错误信息"),
      }),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
      },
    },
    async ({ message }) => ({
      isError: true,
      content: [{ type: "text", text: `预期失败：${message}` }],
    }),
  );
}
