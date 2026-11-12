import { completable, type MCPServer } from "mcp-use";
import { z } from "zod";
import { listNotes, notePayload } from "./notes.ts";

/** Prompts：参数校验、completion、动态消息。注册顺序即列表顺序。 */
export function registerPrompts(server: MCPServer): void {
  server.prompt(
    {
      name: "review-note",
      title: "Review note",
      description: "让模型审阅一条笔记，支持补全",
      schema: z.object({
        noteId: completable(
          z.string().describe("笔记 id"),
          async (value) =>
            listNotes()
              .map((note) => note.id)
              .filter((id) => id.toLowerCase().startsWith(value.toLowerCase())),
        ),
        tone: completable(
          z.enum(["concise", "detailed", "socratic"]).describe("输出风格"),
          ["concise", "detailed", "socratic"],
        ),
      }),
    },
    async ({ noteId, tone }) => {
      const note = notePayload(noteId);
      const body = note ? `${note.title}\n${note.body}` : "（笔记不存在）";
      const toneHint = {
        concise: "用三句话给出结论。",
        detailed: "给出结构化、逐点的详细分析。",
        socratic: "用追问的方式帮助作者发现盲点。",
      }[tone];
      return {
        messages: [
          {
            role: "user",
            content: {
              type: "text",
              text: `请审阅下面的笔记。${toneHint}\n\n${body}`,
            },
          },
        ],
      };
    },
  );

  server.prompt(
    {
      name: "project-brief",
      title: "Project brief",
      description: "按团队与项目生成一段工作简报",
      schema: z.object({
        team: completable(z.string().describe("团队"), ["platform", "sales", "support"]),
        projectId: completable(
          z.string().describe("项目 id"),
          async (value, context) => {
            const team = context?.arguments?.team;
            const prefix = typeof team === "string" ? team : "project";
            return [`${prefix}-${value || "next"}`, `${prefix}-roadmap`];
          },
        ),
      }),
    },
    async ({ team, projectId }) => ({
      messages: [
        {
          role: "user",
          content: {
            type: "text",
            text: `为 ${team} 团队的 ${projectId} 项目写一段 5 行以内的进展简报。`,
          },
        },
      ],
    }),
  );
}
