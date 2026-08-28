import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import path from "node:path";
import { fileURLToPath } from "node:url";

const RESOURCE_URI = "ui://get-time/mcp-app.html";
const MIME = "text/html;profile=mcp-app";
const projectDir = path.dirname(fileURLToPath(import.meta.url));

function invariant(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const client = new Client(
  { name: "peri-mcp-apps-check", version: "1.0.0" },
  {
    capabilities: {
      extensions: {
        "io.modelcontextprotocol/ui": {
          mimeTypes: [MIME],
        },
      },
    },
  },
);

const transport = new StdioClientTransport({
  command: process.execPath,
  args: [path.join(projectDir, "node_modules", "tsx", "dist", "cli.mjs"), "stdio-server.ts"],
  cwd: projectDir,
  stderr: "pipe",
});

try {
  await client.connect(transport);

  const tools = await client.listTools();
  const getTime = tools.tools.find((tool) => tool.name === "get-time");
  invariant(getTime, "tools/list did not return get-time");

  const meta = getTime._meta as
    | { ui?: { resourceUri?: string; visibility?: string[] } }
    | undefined;
  invariant(
    meta?.ui?.resourceUri === RESOURCE_URI,
    "get-time is missing _meta.ui.resourceUri",
  );
  invariant(
    meta.ui.visibility?.includes("app") === true,
    "get-time is not app-visible",
  );

  const resource = await client.readResource({ uri: RESOURCE_URI });
  invariant(resource.contents.length > 0, "resources/read returned no contents");
  const content = resource.contents[0];
  invariant(content.uri === RESOURCE_URI, "resource URI did not round-trip");
  invariant(content.mimeType === MIME, "resource MIME is not MCP Apps HTML");
  invariant("text" in content && content.text.includes("<!DOCTYPE html>"), "resource is not bundled HTML");

  const result = await client.callTool({ name: "get-time", arguments: {} });
  const structured = result.structuredContent as { iso?: string } | undefined;
  invariant(typeof structured?.iso === "string", "tools/call is missing structuredContent.iso");
  invariant(!Number.isNaN(Date.parse(structured.iso)), "structuredContent.iso is not an ISO date");
  const contentBlocks = Array.isArray(result.content) ? result.content : [];
  invariant(contentBlocks.length > 0, "tools/call is missing text fallback content");
  invariant(result.isError !== true, "tools/call returned isError=true");

  process.stdout.write(
    JSON.stringify(
      {
        ok: true,
        tool: getTime.name,
        resourceUri: content.uri,
        mimeType: content.mimeType,
        structuredContent: structured,
      },
      null,
      2,
    ) + "\n",
  );
} finally {
  await client.close();
}
