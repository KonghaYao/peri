/**
 * mcp-use 客户端示例：连接 showcase server，依次验证
 * tools / resources / templates / prompts / subscriptions / progress / MRTR。
 *
 * 先启动 server：bun run dev
 * 再运行：bun run client
 */
import {
  MCPClient,
  type OnElicitationCallback,
  type OnSamplingCallback,
} from "@mcp-use/client";

const url = process.env.SHOWCASE_URL ?? "http://127.0.0.1:31337/mcp";

const client = new MCPClient({
  mcpServers: {
    showcase: {
      url,
      // 2026 协议的 MRTR 与旧版 push 请求共用这两个回调。
      onSampling: (async (params) => {
        const content = params.messages.at(-1)?.content;
        const blocks = Array.isArray(content) ? content : content ? [content] : [];
        const text = blocks.find((block) => block.type === "text");
        console.log(`[sampling] host model 收到 ${params.messages.length} 条消息`);
        return {
          model: "demo-host-model",
          role: "assistant",
          content: {
            type: "text",
            text: `宿主模型摘要：${text?.type === "text" ? text.text.slice(0, 40) : ""}…`,
          },
        };
      }) satisfies OnSamplingCallback,
      onElicitation: (async (params) => {
        console.log("[elicitation] 收到审批请求", params.message);
        return { action: "accept", content: { approve: true, note: "client demo" } };
      }) satisfies OnElicitationCallback,
    },
  },
});

const session = await client.connect("showcase");
const notifications: string[] = [];
session.on("notification", async (notification) => {
  const method = (notification as { method?: string }).method ?? "unknown";
  notifications.push(method);
  console.log(`[notification] ${method}`, JSON.stringify(notification).slice(0, 200));
});

function section(title: string): void {
  console.log(`\n=== ${title} ===`);
}

try {
  const serverInfo = session.serverInfo;
  section("server");
  console.log(serverInfo?.name, serverInfo?.version);

  section("tools");
  const tools = await session.listTools();
  console.log(tools.map((tool) => tool.name).join(", "));

  section("call echo");
  const echoed = await session.callTool("echo", { text: "hello", uppercase: true });
  console.log(echoed.content);

  section("resources");
  const resources = await session.listAllResources();
  console.log(resources.resources.map((resource) => resource.uri).join("\n"));

  section("resource templates");
  const templates = await session.listResourceTemplates();
  console.log(templates.resourceTemplates.map((template) => template.uriTemplate).join("\n"));

  section("read demo://readme");
  const readme = await session.readResource("demo://readme");
  console.log(readme.contents[0]?.mimeType);

  section("read template demo://notes/peri");
  const note = await session.readResource("demo://notes/peri");
  console.log(note.contents[0] && "text" in note.contents[0] ? note.contents[0].text : "");

  section("prompts");
  const prompts = await session.listPrompts();
  console.log(prompts.prompts.map((prompt) => prompt.name).join(", "));
  const prompt = await session.getPrompt("review-note", {
    noteId: "peri",
    tone: "concise",
  });
  console.log(prompt.messages.map((message) => JSON.stringify(message)).join("\n"));

  section("state");
  await session.callTool("set_theme", { theme: "dark" });
  const state = await session.readResource("demo://state");
  console.log(state.contents[0] && "text" in state.contents[0] ? state.contents[0].text : "");

  section("progress + custom notification");
  await session.callTool("slow_task", { steps: 3, stepMs: 50 });

  section("MRTR elicitation");
  const approval = await session.callTool("request_approval", { action: "publish demo" });
  console.log(approval.content);

  section("MRTR sampling");
  const summary = await session.callTool("summarize_note_with_host_model", {
    noteId: "mcp-basics",
  });
  console.log(summary.content);

  section("notifications observed");
  console.log([...new Set(notifications)].join(", ") || "(none)");
} finally {
  await session.disconnect();
  await client.closeAllSessions();
}
