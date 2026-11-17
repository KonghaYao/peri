import type { MCPServer } from "mcp-use";
import { z } from "zod";
import { listNotes, upsertNote } from "../notes.ts";
import { NOTES_INDEX_URI, bumpRevision, revision } from "../state.ts";

/** 笔记相关工具：查询与写入。 */
export function registerNotesTools(server: MCPServer): void {
  server.tool(
    {
      name: "search_notes",
      title: "Search notes",
      description: "按关键词或标签搜索演示笔记，返回结构化结果。",
      inputSchema: z.object({
        query: z.string().optional().describe("标题或正文关键词"),
        tag: z.string().optional().describe("精确匹配的标签"),
        limit: z.number().int().min(1).max(20).default(5).describe("最多返回条数"),
      }),
      outputSchema: z.object({
        total: z.number().int(),
        results: z.array(
          z.object({
            id: z.string(),
            title: z.string(),
            tags: z.array(z.string()),
            updatedAt: z.string(),
          }),
        ),
      }),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: false,
      },
    },
    async ({ query, tag, limit }) => {
      const q = query?.trim().toLowerCase();
      const matched = listNotes().filter((note) => {
        const hitQuery =
          !q ||
          note.title.toLowerCase().includes(q) ||
          note.body.toLowerCase().includes(q);
        const hitTag = !tag || note.tags.includes(tag);
        return hitQuery && hitTag;
      });
      const results = matched.slice(0, limit).map(({ id, title, tags, updatedAt }) => ({
        id,
        title,
        tags,
        updatedAt,
      }));
      const data = { total: matched.length, results };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: data,
      };
    },
  );

  server.tool(
    {
      name: "create_note",
      title: "Create note",
      description:
        "创建或覆盖一条笔记。成功后发送 resources/list_changed 与资源 updated 通知。",
      inputSchema: z.object({
        id: z
          .string()
          .regex(/^[a-z0-9-]+$/, "只允许小写字母、数字和连字符")
          .describe("笔记 id，例如 release-checklist"),
        title: z.string().min(1).describe("标题"),
        body: z.string().min(1).describe("正文"),
        tags: z.array(z.string()).default([]).describe("标签列表"),
      }),
      outputSchema: z.object({
        id: z.string(),
        created: z.boolean(),
        revision: z.number().int(),
      }),
      annotations: {
        readOnlyHint: false,
        destructiveHint: false,
        idempotentHint: true,
        openWorldHint: false,
      },
    },
    async ({ id, title, body, tags }, ctx) => {
      const created = upsertNote(id, title, body, tags);
      bumpRevision();
      await server.notifyResourcesChanged();
      await server.notifyResourceUpdated(`${NOTES_INDEX_URI}/${id}`);
      await ctx.sendLog("info", { id, created, revision }, "showcase");
      const data = { id, created, revision };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: data,
      };
    },
  );
}
