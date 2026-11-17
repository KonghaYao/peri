/**
 * Proxy 示例：gateway 把上游 MCP server 的能力组合进自己的工具面。
 *
 * 启动：bun run gateway
 * 连接：http://127.0.0.1:31341/mcp
 */
import { MCPServer } from "mcp-use";
import { z } from "zod";

const UPSTREAM_PORT = 31340;
const GATEWAY_PORT = 31341;

const upstream = new MCPServer({
  name: "notes-upstream",
  version: "0.1.0",
});

let counter = 0;
upstream.tool(
  {
    name: "upstream_ping",
    description: "上游服务示例工具；返回调用次数。",
    inputSchema: z.object({ label: z.string().default("ping") }),
    annotations: { readOnlyHint: true, destructiveHint: false },
  },
  async ({ label }) => {
    counter += 1;
    return {
      content: [{ type: "text", text: `${label}#${counter} from notes-upstream` }],
    };
  },
);

upstream.resource(
  {
    name: "upstream-readme",
    uri: "upstream://readme",
    mimeType: "text/markdown",
  },
  async (uri) => ({
    contents: [{ uri: uri.href, mimeType: "text/markdown", text: "# notes-upstream" }],
  }),
);

await upstream.listen(UPSTREAM_PORT, { host: "127.0.0.1" });

const gateway = new MCPServer({
  name: "showcase-gateway",
  version: "0.1.0",
  instructions: "通过 gateway 访问组合后的上游能力，工具名带 notes 命名空间。",
});

await gateway.proxy({
  notes: { url: `http://127.0.0.1:${UPSTREAM_PORT}/mcp` },
});
await gateway.listen(GATEWAY_PORT, { host: "127.0.0.1" });

console.log(`gateway ready: http://127.0.0.1:${GATEWAY_PORT}/mcp`);
console.log(`upstream ready: http://127.0.0.1:${UPSTREAM_PORT}/mcp`);

const shutdown = async () => {
  await gateway.close();
  await upstream.close();
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
