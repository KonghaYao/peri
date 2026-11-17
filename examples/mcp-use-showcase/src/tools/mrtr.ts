import {
  acceptedContent,
  inputRequired,
  inputResponse,
  type MCPServer,
} from "mcp-use";
import { z } from "zod";
import { notePayload } from "../notes.ts";

/** MRTR 工具：elicitation 审批与 host model sampling。 */
export function registerMrtrTools(server: MCPServer): void {
  server.tool(
    {
      name: "request_approval",
      title: "Request approval",
      description:
        "通过 MRTR elicitation 请求用户确认，再把结果写入状态资源。客户端不支持 elicitation 时会返回错误。",
      inputSchema: z.object({
        action: z.string().min(1).describe("需要审批的动作描述"),
      }),
      outputSchema: z.object({
        approved: z.boolean(),
        note: z.string().optional(),
      }),
      annotations: {
        readOnlyHint: false,
        destructiveHint: false,
      },
    },
    async ({ action }, ctx) => {
      if (!ctx.client.can("elicitation")) {
        return {
          isError: true,
          content: [
            {
              type: "text",
              text: "当前客户端未声明 elicitation 能力；请在支持 MRTR 的客户端中调用。",
            },
          ],
        };
      }

      const schema = z.object({
        approve: z.boolean().describe("是否批准该动作"),
        note: z.string().max(200).optional().describe("审批备注"),
      });
      const key = "approval";
      const response = inputResponse(ctx.inputResponses, key);

      if (response.kind === "elicit" && response.action !== "accept") {
        const data = { approved: false };
        return {
          content: [{ type: "text", text: JSON.stringify(data) }],
          structuredContent: data,
        };
      }

      const accepted = acceptedContent(ctx.inputResponses, key, schema);
      if (accepted === undefined) {
        return inputRequired({
          inputRequests: {
            [key]: inputRequired.elicit({
              message: `是否批准：${action}？`,
              requestedSchema: schema,
            }),
          },
        });
      }

      const data = { approved: accepted.approve, note: accepted.note };
      return {
        content: [{ type: "text", text: JSON.stringify(data) }],
        structuredContent: { approved: accepted.approve, note: accepted.note },
      };
    },
  );

  server.tool(
    {
      name: "summarize_note_with_host_model",
      title: "Summarize with host model",
      description:
        "通过 MRTR sampling 让宿主模型总结一条笔记。客户端不支持 sampling 时返回错误。",
      inputSchema: z.object({
        noteId: z.string().describe("要总结的笔记 id"),
      }),
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: true,
      },
    },
    async ({ noteId }, ctx) => {
      if (!ctx.client.can("sampling")) {
        return {
          isError: true,
          content: [
            {
              type: "text",
              text: "当前客户端未声明 sampling 能力；请在支持 MRTR sampling 的客户端中调用。",
            },
          ],
        };
      }

      const note = notePayload(noteId);
      if (!note) {
        return {
          isError: true,
          content: [{ type: "text", text: `笔记不存在：${noteId}` }],
        };
      }

      const key = "summary";
      const response = inputResponse(ctx.inputResponses, key);
      if (response.kind === "missing") {
        return inputRequired({
          inputRequests: {
            [key]: inputRequired.createMessage({
              messages: [
                {
                  role: "user",
                  content: {
                    type: "text",
                    text: `请用一句中文总结这条笔记：\n\n${note.title}\n${note.body}`,
                  },
                },
              ],
              maxTokens: 120,
            }),
          },
        });
      }
      if (response.kind !== "sampling") {
        return {
          isError: true,
          content: [{ type: "text", text: "未收到 sampling 响应。" }],
        };
      }

      const blocks = Array.isArray(response.result.content)
        ? response.result.content
        : [response.result.content];
      const summary = blocks
        .map((block) => (block.type === "text" ? block.text : JSON.stringify(block)))
        .join("\n");
      return { content: [{ type: "text", text: summary }] };
    },
  );
}
