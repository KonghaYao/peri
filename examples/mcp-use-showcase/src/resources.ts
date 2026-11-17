import type { MCPServer } from "mcp-use";
import { listNotes, noteCount, notePayload } from "./notes.ts";
import {
  NOTES_INDEX_URI,
  REPORT_URI,
  STATE_URI,
  revision,
  statePayload,
  theme,
} from "./state.ts";

/** Resources：静态、模板、多内容、二进制、可订阅。注册顺序即列表顺序。 */
export function registerResources(server: MCPServer): void {
  server.resource(
    {
      name: "showcase-readme",
      title: "Showcase README",
      uri: "demo://readme",
      description: "demo 的能力清单与试用方式",
      mimeType: "text/markdown",
      annotations: { audience: ["user", "assistant"], priority: 0.8 },
    },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: "text/markdown",
          text: [
            "# mcp-use showcase",
            "",
            "- tools: echo / search_notes / create_note / set_theme / slow_task / request_approval / summarize_note_with_host_model / client_info / fail_demo / reset_demo_state / get_testing_guide",
            "- resources: demo://readme / demo://state / demo://notes / demo://notes/{id} / demo://report / demo://banner",
            "- prompts: review-note / project-brief",
            "- skills: showcase-test-guide（skill://showcase-test-guide/SKILL.md）",
            "- advanced: middleware / progress / custom notification / resource subscription / skills",
            "",
            "验收先调用 get_testing_guide；完整步骤和失败排查在 skill 资源里。",
            "调用 set_theme 后重新读取 demo://state 观察变化。",
          ].join("\n"),
        },
      ],
    }),
  );

  server.resource(
    {
      name: "demo-state",
      title: "Demo state",
      uri: STATE_URI,
      description: "可变状态；变更时发送资源更新通知",
      mimeType: "application/json",
      annotations: { audience: ["assistant"], priority: 1 },
    },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: "application/json",
          text: JSON.stringify(statePayload(), null, 2),
        },
      ],
    }),
  );

  server.resource(
    {
      name: "notes-index",
      title: "Notes index",
      uri: NOTES_INDEX_URI,
      description: "全部笔记的轻量索引",
      mimeType: "application/json",
      annotations: { audience: ["assistant"], priority: 0.9 },
    },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: "application/json",
          text: JSON.stringify(
            listNotes().map(({ id, title, tags, updatedAt }) => ({
              id,
              title,
              tags,
              updatedAt,
            })),
            null,
            2,
          ),
        },
      ],
    }),
  );

  server.resourceTemplate(
    {
      name: "note-by-id",
      title: "Note by id",
      uriTemplate: "demo://notes/{id}",
      description: "按 id 读取单条笔记",
      mimeType: "application/json",
      complete: {
        id: async (value) =>
          listNotes()
            .map((note) => note.id)
            .filter((id) => id.toLowerCase().startsWith(value.toLowerCase())),
      },
      annotations: { audience: ["assistant"], priority: 0.9 },
    },
    async (uri, { id }) => {
      const noteId = Array.isArray(id) ? id[0] : id;
      const note = notePayload(noteId ?? "");
      if (!note) {
        throw new Error(`笔记不存在：${noteId}`);
      }
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/json",
            text: JSON.stringify(note, null, 2),
          },
        ],
      };
    },
  );

  server.resource(
    {
      name: "report",
      title: "Report",
      uri: REPORT_URI,
      description: "一次读取返回多个内容块（Markdown + JSON）",
      mimeType: "text/markdown",
      annotations: { audience: ["assistant"], priority: 0.7 },
    },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: "text/markdown",
          text: `# 状态报告\n\n- 笔记数：${noteCount()}\n- 主题：${theme}\n- 版本：${revision}`,
        },
        {
          uri: `${uri.href}#json`,
          mimeType: "application/json",
          text: JSON.stringify(statePayload(), null, 2),
        },
      ],
    }),
  );

  server.resource(
    {
      name: "banner",
      title: "Banner",
      uri: "demo://banner",
      description: "二进制内容示例（1x1 PNG）",
      mimeType: "image/png",
      annotations: { audience: ["user"], priority: 0.3 },
    },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: "image/png",
          blob: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        },
      ],
    }),
  );
}
