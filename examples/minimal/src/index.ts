import { McpServer } from "@modelcontextprotocol/server";
import { createCacheVersion, createMcppServerFactory, startServer } from "@peri-code/mcpp";
import { z } from "zod";

const cacheVersion = await createCacheVersion({
  schemaVersion: "1",
  tools: ["echo"],
});

const createServer = createMcppServerFactory(
  { cacheVersion },
  (_request, mcpp) => {
    const server = new McpServer(
      {
        name: "minimal-mcpp-server",
        version: "0.1.0",
      },
      { capabilities: mcpp.capabilities },
    );

    server.registerTool(
      "echo",
      {
        description: "返回输入的文本，用于验证 MCP server 连接。",
        inputSchema: z.object({
          text: z.string().describe("要返回的文本"),
        }),
      },
      ({ text }) => ({
        content: [{ type: "text", text }],
      }),
    );

    return server;
  },
);

await startServer(createServer, { mode: "stdio" });
