// 微信风格聊天 MCP server —— 2026-07-28 协议（手写 JSON-RPC）
//
// 场景：用户在聊天 UI 里 @agent → server 通过 subscriptions/listen 长流
// 通知已订阅的 MCP client（如 peri）→ client 读资源、调 chat/send 回复。
//
// 运行：bun run chat/server.ts（默认 http://localhost:3100）
// 端点：
//   GET  /                → 聊天 UI（web/chat.html）
//   GET  /api/messages    → UI 轮询读取消息
//   POST /api/messages    → UI 发送消息（含 @agent 时触发订阅通知）
//   POST /mcp             → MCP JSON-RPC（Streamable HTTP，stateless）

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 3100;
const ROOM = "general";
const ROOM_URI = `chat://room/${ROOM}/messages`;

// ─── 状态 ────────────────────────────────────────────────────────────────────

interface ChatMessage {
  id: number;
  room: string;
  sender: "user" | "agent";
  text: string;
  ts: number;
  mentionsAgent: boolean;
}

/** 每个 room 的消息历史（按时间序） */
const rooms = new Map<string, ChatMessage[]>();
let nextId = 1;

/** 活跃的 subscriptions/listen 流：subscriptionId → { send, uris } */
interface SubscriptionEntry {
  send: (payload: unknown) => void;
  uris: Set<string>;
}
const subscriptions = new Map<number, SubscriptionEntry>();

function getMessages(room: string): ChatMessage[] {
  return rooms.get(room) ?? [];
}

function addMessage(room: string, sender: "user" | "agent", text: string): ChatMessage {
  const mentionsAgent = sender === "user" && /@agent\b/.test(text);
  const msg: ChatMessage = { id: nextId++, room, sender, text, ts: Date.now(), mentionsAgent };
  const list = rooms.get(room) ?? [];
  list.push(msg);
  rooms.set(room, list);
  return msg;
}

/** 向订阅了该 URI 的 listen 流推送 resources/updated 通知 */
function notifySubscribers(msg: ChatMessage) {
  const uri = `chat://room/${msg.room}/messages`;
  let notified = 0;
  for (const [sid, entry] of subscriptions) {
    if (!entry.uris.has(uri)) continue;
    entry.send({
      jsonrpc: "2.0",
      method: "notifications/resources/updated",
      params: {
        uri,
        _meta: { "io.modelcontextprotocol/subscriptionId": sid },
      },
    });
    notified++;
  }
  console.log(`[chat] @agent 消息 #${msg.id} → 通知 ${notified} 个订阅流`);
}

// ─── MCP 工具 / 资源定义 ─────────────────────────────────────────────────────

const TOOLS = [
  {
    name: "chat/send",
    description:
      "在聊天房间里以 agent 身份发送一条消息（回复用户）。用户 @agent 后调用本工具回复。",
    inputSchema: {
      type: "object",
      properties: {
        room_id: { type: "string", description: "房间 ID（默认 general）" },
        text: { type: "string", description: "要发送的消息内容" },
      },
      required: ["text"],
    },
  },
  {
    name: "chat/history",
    description: "读取聊天房间的消息历史（含用户消息与 agent 回复），用于了解对话上下文。",
    inputSchema: {
      type: "object",
      properties: {
        room_id: { type: "string", description: "房间 ID（默认 general）" },
        limit: { type: "number", description: "返回最近 N 条（默认 50）" },
      },
    },
  },
];

const RESOURCES = [
  {
    uri: ROOM_URI,
    name: "聊天房间消息",
    description: "聊天房间的消息历史（JSON 数组），@agent 后内容会更新",
    mimeType: "application/json",
  },
];

function chatSend(params: Record<string, unknown>) {
  const room = String(params.room_id ?? ROOM);
  const text = String(params.text ?? "");
  if (!text.trim()) {
    return { isError: true, content: [{ type: "text", text: "text 不能为空" }] };
  }
  const msg = addMessage(room, "agent", text.trim());
  return {
    content: [{ type: "text", text: JSON.stringify({ ok: true, id: msg.id, ts: msg.ts }) }],
  };
}

function chatHistory(params: Record<string, unknown>) {
  const room = String(params.room_id ?? ROOM);
  const limit = Number(params.limit ?? 50);
  const messages = getMessages(room).slice(-limit);
  return {
    content: [{ type: "text", text: JSON.stringify({ room, messages }) }],
  };
}

function readRoomResource(uri: string) {
  const room = uri.replace(/^chat:\/\/room\//, "").replace(/\/messages$/, "");
  const messages = getMessages(room || ROOM);
  return {
    contents: [
      { uri, mimeType: "application/json", text: JSON.stringify({ room, messages }, null, 2) },
    ],
  };
}

// ─── MCP JSON-RPC 处理（2026-07-28，stateless）────────────────────────────────

interface JsonRpcRequest {
  jsonrpc?: string;
  id?: unknown;
  method: string;
  params?: Record<string, unknown>;
}

function rpcResult(id: unknown, result: unknown) {
  return Response.json({ jsonrpc: "2.0", id, result });
}

function rpcError(id: unknown, code: number, message: string) {
  return Response.json({ jsonrpc: "2.0", id, error: { code, message } }, { status: 200 });
}

function handleMcpRequest(body: JsonRpcRequest): Response {
  const { id, method, params } = body;

  switch (method) {
    case "initialize":
      return rpcResult(id, {
        protocolVersion: "2026-07-28",
        capabilities: {
          tools: { listChanged: false },
          resources: { subscribe: true, listChanged: false },
        },
        serverInfo: { name: "peri-chat-demo", version: "1.0.0" },
        instructions:
          "聊天场景 demo：用户在聊天 UI 里 @agent 时，你会收到 chat:// 资源更新通知；" +
          "收到后先调 chat/history 了解上下文，再用 chat/send 回复。",
      });

    case "ping":
      return rpcResult(id, {});

    case "tools/list":
      return rpcResult(id, { tools: TOOLS });

    case "tools/call": {
      const { name, arguments: args } = (params ?? {}) as {
        name?: string;
        arguments?: Record<string, unknown>;
      };
      if (name === "chat/send") return rpcResult(id, chatSend(args ?? {}));
      if (name === "chat/history") return rpcResult(id, chatHistory(args ?? {}));
      return rpcError(id, -32602, `未知工具: ${name}`);
    }

    case "resources/list":
      return rpcResult(id, { resources: RESOURCES });

    case "resources/read": {
      const uri = String((params ?? {}).uri ?? "");
      if (!uri.startsWith("chat://room/")) {
        return rpcError(id, -32602, `不支持的资源 URI: ${uri}`);
      }
      return rpcResult(id, readRoomResource(uri));
    }

    // subscriptions/listen：唯一需要 SSE 长流的请求（见 listen()）
    case "subscriptions/listen":
      return listen(id, params);

    case "notifications/initialized":
    case "notifications/cancelled":
      // 通知类请求无需响应（2026-07-28 已移除 initialized，这里兼容旧 client）
      return new Response(null, { status: 202 });

    default:
      return rpcError(id, -32601, `method not found: ${method}`);
  }
}

/** 处理 subscriptions/listen：返回 SSE 长流；先 ack，再在资源变化时推送通知 */
function listen(id: unknown, params?: Record<string, unknown>): Response {
  const requested = ((params ?? {}).notifications ?? {}) as Record<string, unknown>;

  // 只接受本 server 支持的子集：chat:// 前缀的资源订阅
  const requestedUris: string[] = Array.isArray(requested.resourceSubscriptions)
    ? (requested.resourceSubscriptions as string[]).filter((u) => u.startsWith("chat://"))
    : [];
  const accepted: Record<string, unknown> = {};
  if (requestedUris.length > 0) accepted.resourceSubscriptions = requestedUris;

  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      const send = (payload: unknown) => {
        const data = JSON.stringify(payload);
        // MCP SSE 传输格式：event: message + data: <json>
        controller.enqueue(encoder.encode(`event: message\ndata: ${data}\n\n`));
      };

      // 原则：先确认（acknowledged）、后推送
      send({
        jsonrpc: "2.0",
        method: "notifications/subscriptions/acknowledged",
        params: {
          notifications: accepted,
          _meta: { "io.modelcontextprotocol/subscriptionId": id },
        },
      });

      subscriptions.set(
        Number(id) ?? id as number,
        { send, uris: new Set(requestedUris) },
      );
      console.log(`[mcp] subscriptions/listen 建立 (id=${id}, uris=${JSON.stringify(requestedUris)})`);
    },
    cancel() {
      subscriptions.delete(Number(id));
      console.log(`[mcp] subscriptions/listen 关闭 (id=${id})`);
    },
  });

  return new Response(stream, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    },
  });
}

// ─── HTTP 服务 ───────────────────────────────────────────────────────────────

const chatHtml = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "web", "chat.html"), "utf-8");

Bun.serve({
  port: PORT,
  // SSE 订阅流（subscriptions/listen）需长连接：默认 idleTimeout=10s 会在
  // 10 秒无数据时强制断开，置 0 禁用空闲超时（MCP 2026-07-28 长流要求）。
  idleTimeout: 0,
  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    const { pathname } = url;

    // 聊天 UI
    if (req.method === "GET" && pathname === "/") {
      return new Response(chatHtml, {
        headers: { "Content-Type": "text/html; charset=utf-8" },
      });
    }

    // UI：读取消息（轮询，after=时间戳取增量）
    if (req.method === "GET" && pathname === "/api/messages") {
      const room = url.searchParams.get("room") ?? ROOM;
      const after = Number(url.searchParams.get("after") ?? 0);
      const messages = getMessages(room).filter((m) => m.ts > after);
      return Response.json({ room, messages, serverTs: Date.now() });
    }

    // UI：发送消息（@agent 时触发订阅通知）
    if (req.method === "POST" && pathname === "/api/messages") {
      const { room, text } = (await req.json()) as { room?: string; text?: string };
      if (!text?.trim()) return Response.json({ error: "text 不能为空" }, { status: 400 });
      const msg = addMessage(room ?? ROOM, "user", text.trim());
      if (msg.mentionsAgent) notifySubscribers(msg);
      return Response.json({ message: msg });
    }

    // MCP 端点
    if (pathname === "/mcp") {
      const body = (await req.json()) as JsonRpcRequest;
      return handleMcpRequest(body);
    }

    return new Response("not found", { status: 404 });
  },
});

console.log(`[chat] 聊天 MCP server 已启动:`);
console.log(`[chat]   MCP 端点   : http://localhost:${PORT}/mcp  (2026-07-28, Streamable HTTP)`);
console.log(`[chat]   聊天 UI    : http://localhost:${PORT}/`);
console.log(`[chat]   资源       : ${ROOM_URI}`);
