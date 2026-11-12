/**
 * 2026-07-28 订阅示例：用官方 SDK 打开 `subscriptions/listen`，
 * 观察 resources/updated、resources/list_changed 与 tools/list_changed。
 *
 * 先启动 server：bun run dev
 * 再运行：bun run subscribe
 */
import { Client, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";

const url = process.env.SHOWCASE_URL ?? "http://127.0.0.1:31337/mcp";

const client = new Client(
  { name: "showcase-subscriber", version: "0" },
  { versionNegotiation: { mode: "auto" } },
);

const seen: string[] = [];
client.fallbackNotificationHandler = async (notification) => {
  seen.push(notification.method);
  console.log(`[notification] ${notification.method}`, JSON.stringify(notification.params));
};

const transport = new StreamableHTTPClientTransport(new URL(url));
await client.connect(transport);

const subscription = await client.listen({
  resourceSubscriptions: ["demo://state"],
  resourcesListChanged: true,
  toolsListChanged: true,
});
console.log("acknowledged:", subscription.honoredFilter);

await client.callTool({ name: "set_theme", arguments: { theme: "dark" } });
await client.callTool({
  name: "create_note",
  arguments: {
    id: "subscription-demo",
    title: "订阅演示",
    body: "这条笔记由 subscribe 示例创建。",
    tags: ["demo"],
  },
});
await new Promise((resolve) => setTimeout(resolve, 800));

console.log("observed:", [...new Set(seen)]);
await subscription.close();
await client.close();
