#!/usr/bin/env node
// Minimal OpenAI-compatible streaming model used by the 0728 acceptance driver.
// First request: call MOCK_TOOL. Later requests (after a tool result): finish with text.

import fs from "node:fs";
import http from "node:http";

const port = Number(process.env.MOCK_MODEL_PORT ?? "0");
const tool = process.env.MOCK_TOOL ?? "request_approval";
const finalText = process.env.MOCK_FINAL_TEXT ?? "tool finished";
const logPath = process.env.MOCK_MODEL_LOG ?? "/tmp/mock-model.jsonl";
const calls = process.env.MOCK_TOOL_CALLS
  ? JSON.parse(process.env.MOCK_TOOL_CALLS)
  : [
      {
        name: tool,
        arguments: process.env.MOCK_ARGUMENTS ?? JSON.stringify({ action: "deploy" }),
      },
    ];

const log = (record) =>
  fs.appendFileSync(logPath, `${JSON.stringify(record)}\n`);

function sse(res, frames) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
  for (const frame of frames) {
    res.write(`data: ${JSON.stringify(frame)}\n\n`);
  }
  res.write("data: [DONE]\n\n");
  res.end();
}

const server = http.createServer((req, res) => {
  if (req.method !== "POST" || !req.url?.endsWith("/chat/completions")) {
    res.writeHead(404);
    res.end();
    return;
  }
  let body = "";
  req.on("data", (chunk) => {
    body += chunk;
  });
  req.on("end", () => {
    let request;
    try {
      request = JSON.parse(body);
    } catch {
      res.writeHead(400);
      res.end();
      return;
    }
    const messages = Array.isArray(request.messages) ? request.messages : [];
    const last = messages.at(-1);
    const sawToolResult = last?.role === "tool";
    log({
      event: "request",
      model: request.model,
      stream: request.stream,
      toolNames: Array.isArray(request.tools)
        ? request.tools.map((entry) => entry?.function?.name).filter(Boolean)
        : [],
      lastRole: last?.role ?? null,
      lastContent:
        typeof last?.content === "string"
          ? last.content.slice(0, 1000)
          : JSON.stringify(last?.content ?? null).slice(0, 1000),
      sawToolResult,
    });
    if (sawToolResult) {
      sse(res, [
        {
          id: "chatcmpl-final",
          object: "chat.completion.chunk",
          created: Math.floor(Date.now() / 1000),
          model: request.model ?? "mock",
          choices: [
            {
              index: 0,
              delta: { role: "assistant", content: finalText },
              finish_reason: null,
            },
          ],
        },
        {
          id: "chatcmpl-final",
          object: "chat.completion.chunk",
          created: Math.floor(Date.now() / 1000),
          model: request.model ?? "mock",
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
        },
      ]);
      return;
    }
    sse(res, [
      {
        id: "chatcmpl-tool",
        object: "chat.completion.chunk",
        created: Math.floor(Date.now() / 1000),
        model: request.model ?? "mock",
        choices: [
          {
            index: 0,
            delta: {
              role: "assistant",
              tool_calls: calls.map((call, index) => ({
                index,
                id: `call_fixture_${index + 1}`,
                type: "function",
                function: {
                  name: call.name,
                  arguments:
                    typeof call.arguments === "string"
                      ? call.arguments
                      : JSON.stringify(call.arguments ?? {}),
                },
              })),
            },
            finish_reason: null,
          },
        ],
      },
      {
        id: "chatcmpl-tool",
        object: "chat.completion.chunk",
        created: Math.floor(Date.now() / 1000),
        model: request.model ?? "mock",
        choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }],
      },
    ]);
  });
});

server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  process.stdout.write(`MOCK_MODEL_PORT=${address.port}\n`);
  log({ event: "listen", port: address.port });
});
