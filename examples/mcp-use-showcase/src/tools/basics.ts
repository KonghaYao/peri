import type { MCPServer } from "mcp-use";
import { z } from "zod";

/** 基础工具：连接与参数校验。 */
export function registerBasicsTools(server: MCPServer): void {
  server.tool(
    {
      name: "echo",
      title: "Echo",
      description: "原样返回输入文本，用于验证连接与参数校验。",
      inputSchema: z.object({
        text: z.string().min(1).describe("要返回的文本"),
        uppercase: z.boolean().optional().describe("是否转成大写"),
      }),
      outputSchema: z.object({
        text: z.string(),
        length: z.number().int(),
      }),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: false,
      },
    },
    async ({ text, uppercase }) => {
      const value = uppercase ? text.toUpperCase() : text;
      const data = { text: value, length: value.length };
      return {
        content: [{ type: "text", text: `${value} (${data.length} chars)` }],
        structuredContent: data,
      };
    },
  );
}
