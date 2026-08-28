import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  registerAppResource,
  registerAppTool,
  RESOURCE_MIME_TYPE,
} from "@modelcontextprotocol/ext-apps/server";
import fs from "node:fs/promises";
import path from "node:path";
import { z } from "zod";

export const RESOURCE_URI = "ui://get-time/mcp-app.html";

const server = new McpServer({
  name: "Peri MCP Apps Fixture",
  version: "1.0.0",
});

registerAppTool(
  server,
  "get-time",
  {
    title: "Get Time",
    description: "Returns the current server time for the MCP Apps fixture.",
    inputSchema: {},
    outputSchema: { iso: z.string().datetime() },
    _meta: {
      ui: {
        resourceUri: RESOURCE_URI,
        visibility: ["model", "app"],
      },
    },
  },
  async () => {
    const iso = new Date().toISOString();
    return {
      content: [{ type: "text", text: iso }],
      structuredContent: { iso },
      _meta: { fixture: "peri-mcp-apps" },
    };
  },
);

registerAppResource(
  server,
  RESOURCE_URI,
  RESOURCE_URI,
  {
    mimeType: RESOURCE_MIME_TYPE,
    _meta: {
      ui: {
        prefersBorder: true,
      },
    },
  },
  async () => {
    const html = await fs.readFile(
      path.join(import.meta.dirname, "dist", "mcp-app.html"),
      "utf-8",
    );
    return {
      contents: [
        {
          uri: RESOURCE_URI,
          mimeType: RESOURCE_MIME_TYPE,
          text: html,
          _meta: {
            ui: {
              prefersBorder: true,
              csp: {
                connectDomains: [],
                resourceDomains: [],
              },
            },
          },
        },
      ],
    };
  },
);

await server.connect(new StdioServerTransport());
