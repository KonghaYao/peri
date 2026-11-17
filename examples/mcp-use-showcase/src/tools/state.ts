import type { MCPServer } from "mcp-use";
import { z } from "zod";
import { noteCount } from "../notes.ts";
import {
  STATE_URI,
  bumpRevision,
  resetState,
  revision,
  setTheme,
  theme,
} from "../state.ts";

/** 状态相关工具：主题切换与重置。 */
export function registerStateTools(server: MCPServer): void {
  server.tool(
    {
      name: "set_theme",
      title: "Set theme",
      description: "修改 demo://state 中的主题，并发送 resource updated 通知。",
      inputSchema: z.object({
        theme: z.enum(["light", "dark"]).describe("目标主题"),
      }),
      outputSchema: z.object({
        theme: z.string(),
        revision: z.number().int(),
      }),
      annotations: {
        readOnlyHint: false,
        destructiveHint: false,
        idempotentHint: true,
      },
    },
    async ({ theme: nextTheme }) => {
      setTheme(nextTheme);
      bumpRevision();
      await server.notifyResourceUpdated(STATE_URI);
      const data = { theme, revision };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: data,
      };
    },
  );

  server.tool(
    {
      name: "reset_demo_state",
      title: "Reset demo state",
      description: "恢复种子数据与默认主题。破坏性操作，适合验证 HITL 确认流程。",
      inputSchema: z.object({
        confirm: z.literal(true).describe("必须显式传 true 才执行"),
      }),
      outputSchema: z.object({
        revision: z.number().int(),
        noteCount: z.number().int(),
      }),
      annotations: {
        readOnlyHint: false,
        destructiveHint: true,
        idempotentHint: true,
      },
    },
    async () => {
      resetState();
      await server.notifyResourcesChanged();
      await server.notifyResourceUpdated(STATE_URI);
      const data = { revision, noteCount: noteCount() };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: data,
      };
    },
  );
}
