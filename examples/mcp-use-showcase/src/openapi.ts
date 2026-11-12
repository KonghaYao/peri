/**
 * OpenAPI 示例：从一个内联 OpenAPI 3.1 文档生成 MCP tools。
 * 同进程启动一个本地 REST mock，避免依赖外网。
 *
 * 启动：bun run openapi
 * 连接：http://127.0.0.1:31338/mcp
 */
import { createServer } from "node:http";
import { MCPServer } from "mcp-use";

const REST_PORT = 31339;

type RestNote = { id: string; title: string; tags: string[] };
const restNotes = new Map<string, RestNote>([
  ["openapi", { id: "openapi", title: "OpenAPI 到 MCP", tags: ["demo", "openapi"] }],
]);

function json(res: import("node:http").ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

const restServer = createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://127.0.0.1:${REST_PORT}`);

  if (req.method === "GET" && url.pathname === "/notes") {
    const tag = url.searchParams.get("tag");
    const items = [...restNotes.values()].filter((note) => !tag || note.tags.includes(tag));
    json(res, 200, { items });
    return;
  }

  if (req.method === "GET" && url.pathname.startsWith("/notes/")) {
    const id = decodeURIComponent(url.pathname.slice("/notes/".length));
    const note = restNotes.get(id);
    if (!note) {
      json(res, 404, { error: `note not found: ${id}` });
      return;
    }
    json(res, 200, note);
    return;
  }

  if (req.method === "POST" && url.pathname === "/notes") {
    let raw = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      raw += chunk;
    });
    req.on("end", () => {
      try {
        const body = JSON.parse(raw) as Partial<RestNote>;
        if (!body.id || !body.title) {
          json(res, 400, { error: "id and title are required" });
          return;
        }
        const note: RestNote = {
          id: body.id,
          title: body.title,
          tags: body.tags ?? [],
        };
        restNotes.set(note.id, note);
        json(res, 201, note);
      } catch {
        json(res, 400, { error: "invalid json" });
      }
    });
    return;
  }

  json(res, 404, { error: "not found" });
});

await new Promise<void>((resolve) => restServer.listen(REST_PORT, "127.0.0.1", resolve));

const spec = {
  openapi: "3.1.0",
  info: { title: "Showcase REST API", version: "0.1.0" },
  servers: [{ url: `http://127.0.0.1:${REST_PORT}` }],
  paths: {
    "/notes": {
      get: {
        operationId: "rest_list_notes",
        summary: "列出笔记，可按 tag 过滤",
        tags: ["notes"],
        parameters: [
          {
            name: "tag",
            in: "query" as const,
            required: false,
            description: "精确匹配标签",
            schema: { type: "string" },
          },
        ],
        responses: { "200": { description: "笔记列表" } },
      },
      post: {
        operationId: "rest_create_note",
        summary: "创建笔记",
        tags: ["notes"],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: {
                type: "object",
                required: ["id", "title"],
                properties: {
                  id: { type: "string" },
                  title: { type: "string" },
                  tags: { type: "array", items: { type: "string" } },
                },
              },
            },
          },
        },
        responses: { "201": { description: "创建成功" } },
      },
    },
    "/notes/{id}": {
      get: {
        operationId: "rest_get_note",
        summary: "按 id 读取笔记",
        tags: ["notes"],
        parameters: [
          {
            name: "id",
            in: "path" as const,
            required: true,
            schema: { type: "string" },
          },
        ],
        responses: { "200": { description: "笔记详情" } },
      },
    },
  },
};

const server = MCPServer.fromOpenAPI({
  spec,
  name: "showcase-openapi",
  version: "0.1.0",
});

export default server;
