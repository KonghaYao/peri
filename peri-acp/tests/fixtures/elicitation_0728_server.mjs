#!/usr/bin/env node
// MCP 2026-07-28 stdio fixture for Peri elicitation acceptance.
// - `request_approval`: form elicitation (approve boolean + optional note)
// - `authorize`: URL elicitation with a local browser callback
// Every received JSON-RPC line and every business execution is appended to
// MCP_STATE_LOG as JSONL so the ACP driver can count transport vs business calls.

import fs from "node:fs";
import readline from "node:readline";

const stateLog = process.env.MCP_STATE_LOG ?? "/tmp/mcp-0728-state.jsonl";
const callbackPort = Number(process.env.MCP_CALLBACK_PORT ?? "0");
const toolResult = "receipt-001";
const legacy = process.env.MCP_LEGACY === "1";
const protocolVersion = legacy ? "2025-11-25" : "2026-07-28";

const log = (record) =>
  fs.appendFileSync(
    stateLog,
    `${JSON.stringify({ at: new Date().toISOString(), ...record })}\n`,
  );

let transportCalls = 0;
let executions = 0;
const authFile = process.env.MCP_AUTH_FILE ?? "";
const authUrl = process.env.MCP_AUTH_URL ?? "";

const authorized = () => authFile !== "" && fs.existsSync(authFile);


function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function result(id, payload) {
  send({ jsonrpc: "2.0", id, result: payload });
}

function toolText(text, structured) {
  return {
    content: [{ type: "text", text }],
    ...(structured ? { structuredContent: structured } : {}),
    isError: false,
  };
}

function inputRequired(key, request, requestState) {
  return {
    resultType: "input_required",
    inputRequests: { [key]: request },
    ...(requestState ? { requestState } : {}),
  };
}

function formRequest(message = "Approve deploying to the fixture environment") {
  return {
    method: "elicitation/create",
    params: {
      mode: "form",
      message,
      requestedSchema: {
        type: "object",
        properties: {
          approve: { type: "boolean", title: "Approve" },
          note: { type: "string", title: "Note" },
        },
        required: ["approve"],
      },
    },
  };
}

function urlRequest() {
  // 2026-07-28 URL mode omits `elicitationId` on the MCP wire.
  return {
    method: "elicitation/create",
    params: {
      mode: "url",
      message: "Authorize the deployment in the browser",
      url: authUrl,
    },
  };
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  log({ event: "recv", line });
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id === undefined) return;
  const { id, method, params } = message;

  switch (method) {
    case "server/discover":
      if (legacy) {
        send({
          jsonrpc: "2.0",
          id,
          error: { code: -32601, message: "method not found: server/discover" },
        });
        return;
      }
      result(id, {
        resultType: "complete",
        supportedVersions: ["2026-07-28"],
        capabilities: { tools: {} },
        ttlMs: 0,
        cacheScope: "private",
        _meta: {
          "io.modelcontextprotocol/serverInfo": {
            name: "peri-elicitation-fixture",
            version: "1.0.0",
          },
        },
      });
      return;
    case "initialize":
      log({
        event: "initialize",
        protocolVersion: params?.protocolVersion ?? null,
        capabilities: params?.capabilities ?? null,
      });
      result(id, {
        protocolVersion,
        capabilities: { tools: {} },
        serverInfo: { name: "peri-elicitation-fixture", version: "1.0.0" },
      });
      return;
    case "tools/list":
      result(id, {
        tools: [
          {
            name: "request_approval",
            description: "Ask the user to approve a deployment",
            inputSchema: {
              type: "object",
              properties: { action: { type: "string" } },
            },
          },
          {
            name: "authorize",
            description: "Ask the user to authorize in a browser",
            inputSchema: {
              type: "object",
              properties: { action: { type: "string" } },
            },
          },
          {
            name: "double_round",
            description: "Ask two MRTR rounds before returning",
            inputSchema: {
              type: "object",
              properties: { action: { type: "string" } },
            },
          },
          {
            name: "parallel_probe",
            description: "Ask with a shared input key for parallel isolation",
            inputSchema: {
              type: "object",
              properties: { action: { type: "string" } },
            },
          },
          {
            name: "plain_echo",
            description: "Return a plain tool result without elicitation",
            inputSchema: {
              type: "object",
              properties: { action: { type: "string" } },
            },
          },
        ],
      });
      return;
    case "tools/call": {
      transportCalls += 1;
      const callIndex = transportCalls;
      const name = params?.name ?? "";
      log({
        event: "tools_call",
        id,
        index: callIndex,
        name,
        arguments: params?.arguments ?? null,
        inputResponses: params?.inputResponses ?? null,
        requestState: params?.requestState ?? null,
        clientCapabilities:
          params?._meta?.["io.modelcontextprotocol/clientCapabilities"] ?? null,
      });
      const inputResponses = params?.inputResponses ?? null;
      if (name === "request_approval") {
        if (!inputResponses) {
          result(id, inputRequired("approval", formRequest(), "approval-state-1"));
          return;
        }
        const response = inputResponses.approval ?? {};
        const approved =
          response.action === "accept" && response.content?.approve === true;
        const note =
          response.action === "accept" && typeof response.content?.note === "string"
            ? response.content.note
            : "";
        executions += 1;
        const structured = { approved, note, execution: executions };
        log({ event: "execution", name, structured });
        result(id, toolText(JSON.stringify(structured), structured));
        return;
      }
      if (name === "authorize") {
        const response = inputResponses?.authorize ?? null;
        if (response && response.action !== "accept") {
          const structured = { authorized: false, reason: response.action };
          log({ event: "denied", name, structured });
          result(id, toolText(JSON.stringify(structured), structured));
          return;
        }
        if (!authorized()) {
          log({ event: "url_required", index: callIndex });
          result(
            id,
            inputRequired("authorize", urlRequest(), `url-state-${callIndex}`),
          );
          return;
        }
        executions += 1;
        const structured = { receipt: toolResult, execution: executions };
        log({ event: "execution", name, structured });
        result(id, toolText(toolResult, structured));
        return;
      }
      if (name === "double_round") {
        const responses = inputResponses ?? {};
        if (!responses.first) {
          result(
            id,
            inputRequired("first", formRequest("Approve the first round"), "state-first"),
          );
          return;
        }
        if (!responses.second) {
          // 第二轮故意省略 requestState：下一轮续调也必须省略。
          result(id, inputRequired("second", formRequest("Approve the second round")));
          return;
        }
        executions += 1;
        const structured = {
          first: responses.first,
          second: responses.second,
          requestState: params?.requestState ?? null,
          execution: executions,
        };
        log({ event: "execution", name, structured });
        result(id, toolText(JSON.stringify(structured), structured));
        return;
      }
      if (name === "parallel_probe") {
        const action = params?.arguments?.action ?? "";
        const response = inputResponses?.approval ?? null;
        if (!response) {
          result(
            id,
            inputRequired(
              "approval",
              formRequest(`Approve ${action}`),
              `parallel-state-${action}`,
            ),
          );
          return;
        }
        const cancelled = response.action === "cancel" || response.action === "decline";
        const structured = {
          action,
          note: response.content?.note ?? null,
          result: cancelled ? "cancelled" : "accepted",
          requestState: params?.requestState ?? null,
        };
        log({ event: "execution", name, structured });
        result(id, toolText(JSON.stringify(structured), structured));
        return;
      }
      if (name === "plain_echo") {
        executions += 1;
        const structured = { result: "plain-ok", execution: executions };
        log({ event: "execution", name, structured });
        result(id, toolText("plain-ok", structured));
        return;
      }
      result(id, toolText(`unknown tool: ${name}`));
      return;
    }
    default:
      send({
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `method not found: ${method}` },
      });
  }
});

const shutdown = () => {
  process.exit(0);
};
process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
