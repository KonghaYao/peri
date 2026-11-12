import type { MCPServer } from "mcp-use";
import { z } from "zod";
import { noteCount } from "../notes.ts";
import { revision, taskRuns, theme } from "../state.ts";
import { TESTING_GUIDE } from "../testing-guide.ts";

/** 验收与内部诊断工具。 */
export function registerTestingTools(server: MCPServer): void {
  server.tool(
    {
      name: "get_testing_guide",
      title: "Get testing guide",
      description:
        "返回本 server 的验收步骤。Skills over MCP 自动发现不可用时的等价入口。",
      inputSchema: z.object({}),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: false,
      },
    },
    async () => ({
      content: [
        {
          type: "text",
          text: TESTING_GUIDE,
        },
      ],
    }),
  );

  // 被 mcp:tools/list 中间件过滤、但仍可直接调用：验证“发现面”和“可执行面”分离。
  server.tool(
    {
      name: "_internal_diagnostics",
      title: "Internal diagnostics",
      description: "内部诊断工具，不会出现在 tools/list 中。",
      inputSchema: z.object({}),
      annotations: { readOnlyHint: true, destructiveHint: false },
    },
    async () => ({
      content: [
        {
          type: "text",
          text: JSON.stringify({ revision, notes: noteCount(), taskRuns, theme }),
        },
      ],
    }),
  );
}
