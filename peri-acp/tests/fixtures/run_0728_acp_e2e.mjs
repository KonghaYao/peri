#!/usr/bin/env node
// Real `peri acp` acceptance driver for MCP 2026-07-28 elicitation.
// Usage: node run_0728_acp_e2e.mjs <scenario> [--json]
// Scenarios: form_accept_true | form_accept_false | form_decline | form_cancel |
//   url_accept | url_cancel | mrtr_two_rounds | mrtr_parallel | plain_tool |
//   no_interaction_host | legacy_handshake
//
// The driver spawns the production `peri acp` binary, connects the repository's
// 0728 MCP fixture through the project `.mcp.json`, and answers ACP
// `elicitation/create` requests exactly like a client would.

import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";

const scenario = process.argv[2] ?? "form_accept_true";
const repoRoot = process.env.PERI_REPO_ROOT ?? process.cwd();
const periBin = process.env.PERI_BIN ?? path.join(repoRoot, "target/debug/peri");
const toolCalls =
  scenario === "mrtr_two_rounds"
    ? [{ name: "double_round", arguments: { action: "deploy" } }]
    : scenario === "mrtr_parallel"
      ? [
          { name: "parallel_probe", arguments: { action: "a" } },
          { name: "parallel_probe", arguments: { action: "b" } },
        ]
      : scenario === "plain_tool" || scenario === "legacy_handshake"
        ? [{ name: "plain_echo", arguments: { action: "deploy" } }]
        : scenario === "no_interaction_host"
          ? [
              { name: "plain_echo", arguments: { action: "deploy" } },
              { name: "request_approval", arguments: { action: "deploy" } },
            ]
          : scenario === "ask_user_question"
            ? [
                {
                  name: "AskUserQuestion",
                  arguments: {
                    questions: [
                      {
                        question: "Deploy where?",
                        header: "Target",
                        multiSelect: false,
                        options: [{ label: "Edge" }, { label: "Cloud" }],
                      },
                    ],
                  },
                },
              ]
            : [
              {
                name:
                  scenario === "url_accept" || scenario === "url_cancel"
                    ? "authorize"
                    : "request_approval",
                arguments: { action: "deploy" },
              },
            ];
const fixtureServer = path.join(
  repoRoot,
  "peri-acp/tests/fixtures/elicitation_0728_server.mjs",
);
const mockModel = path.join(
  repoRoot,
  "peri-acp/tests/fixtures/mock_openai_model.mjs",
);

const temp = fs.mkdtempSync(path.join(os.tmpdir(), "peri-0728-e2e-"));
const home = path.join(temp, "home");
const workspace = path.join(temp, "workspace");
fs.mkdirSync(path.join(home, ".peri"), { recursive: true });
fs.mkdirSync(workspace, { recursive: true });
const mcpState = path.join(temp, "mcp-state.jsonl");
const modelLog = path.join(temp, "model.jsonl");
const acpLog = path.join(temp, "acp.log");
const authFile = path.join(temp, "authorized.flag");

const report = {
  scenario,
  temp,
  timeline: [],
  elicitations: [],
  acpRequests: [],
  acpErrors: [],
  mcp: { transport_calls: 0, executions: 0, calls: [] },
  model: [],
  prompt: null,
  pass: false,
  reasons: [],
};
const note = (event, detail = {}) => {
  report.timeline.push({ at: new Date().toISOString(), event, ...detail });
};

function redactUrl(url) {
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return "<invalid-url>";
  }
}

async function startMockModel() {
  const child = spawn(process.execPath, [mockModel], {
    env: {
      ...process.env,
      MOCK_MODEL_PORT: "0",
      MOCK_MODEL_LOG: modelLog,
      MOCK_TOOL_CALLS: JSON.stringify(toolCalls),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const port = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("mock model did not start")),
      10_000,
    );
    child.stdout.on("data", (chunk) => {
      const match = /MOCK_MODEL_PORT=(\d+)/.exec(String(chunk));
      if (match) {
        clearTimeout(timer);
        resolve(Number(match[1]));
      }
    });
    child.on("error", reject);
  });
  return { child, port };
}

function startAuthServer() {
  const server = http.createServer((req, res) => {
    if (req.url?.startsWith("/authorize")) {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(`<!doctype html><meta charset="utf-8">
<title>Peri authorization fixture</title>
<h1>External authorization</h1>
<p>This local page simulates the browser callback.</p>
<script>setTimeout(() => { location.href = "/callback"; }, 150);</script>`);
      return;
    }
    if (req.url?.startsWith("/callback")) {
      fs.writeFileSync(authFile, "authorized\n");
      note("browser_callback", { path: "/callback" });
      res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
      res.end("authorized\n");
      return;
    }
    res.writeHead(404);
    res.end("not found\n");
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve({ server, port: address.port });
    });
  });
}

function writeConfig(port, authPort) {
  fs.writeFileSync(
    path.join(home, ".peri", "settings.json"),
    JSON.stringify(
      {
        config: {
          active_alias: "opus",
          providers: [
            {
              id: "fixture",
              type: "openai",
              apiKey: "fixture-key",
              baseUrl: `http://127.0.0.1:${port}/v1`,
              models: { opus: "fixture-model" },
            },
          ],
          profiles: {
            opus: { provider: "fixture", model: "fixture-model", max_tokens: 2048 },
          },
        },
      },
      null,
      2,
    ),
  );
  fs.writeFileSync(
    path.join(workspace, ".mcp.json"),
    JSON.stringify(
      {
        mcpServers: {
          elicit_fixture: {
            command: process.execPath,
            args: [fixtureServer],
            env: {
              MCP_STATE_LOG: mcpState,
              MCP_AUTH_FILE: authFile,
              MCP_AUTH_URL: `http://127.0.0.1:${authPort}/authorize`,
              ...(scenario === "legacy_handshake" ? { MCP_LEGACY: "1" } : {}),
            },
            system_mcp: true,
            system_mcp_tools: [
              "request_approval",
              "authorize",
              "double_round",
              "parallel_probe",
              "plain_echo",
            ],
            system_mcp_timeout: 10000,
          },
        },
      },
      null,
      2,
    ),
  );
}

function spawnPeri() {
  const child = spawn(periBin, ["acp"], {
    cwd: workspace,
    env: {
      ...process.env,
      HOME: home,
      PERI_ASK_USER_TIMEOUT_SECS: "30",
      RUST_LOG: "info",
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const logStream = fs.createWriteStream(acpLog);
  child.stderr.pipe(logStream);
  return child;
}

function createRpc(child) {
  let nextId = 1;
  const pending = new Map();
  const rl = readline.createInterface({ input: child.stdout });

  rl.on("line", async (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      report.acpErrors.push({ kind: "non_json", line: line.slice(0, 500) });
      return;
    }
    if (message.id !== undefined && message.method === undefined) {
      const waiter = pending.get(message.id);
      if (waiter) {
        pending.delete(message.id);
        if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
        else waiter.resolve(message.result);
      }
      return;
    }
    if (message.id !== undefined && message.method) {
      await handleServerRequest(message);
      return;
    }
    if (message.method) {
      note("acp_notification", {
        method: message.method,
        update: message.params?.update?.sessionUpdate ?? null,
      });
      if (message.method === "session/update") {
        const update = message.params?.update ?? {};
        if (update.sessionUpdate === "tool_call_update") {
          report.timeline.push({
            at: new Date().toISOString(),
            event: "tool_call_update",
            toolCallId: update.toolCallId,
            status: update.status,
            content: update.content ?? null,
          });
        }
      }
    }
  });

  let authState = "idle";
  let authPromise = null;

  async function handleServerRequest(message) {
    report.acpRequests.push({
      method: message.method,
      params: message.params,
    });
    if (message.method === "session/request_permission") {
      rpc.respond(message.id, {
        outcome: { outcome: "selected", optionId: "allow_once" },
      });
      return;
    }
    if (message.method === "elicitation/create") {
      if (scenario === "no_interaction_host") {
        report.acpErrors.push({
          kind: "non_interactive_client",
          method: message.method,
        });
        rpc.respondError(message.id, -32601, "elicitation is not supported");
        return;
      }
      const params = message.params ?? {};
      const isUrl = params.mode === "url";
      const entry = {
        at: new Date().toISOString(),
        mode: params.mode,
        sessionId: params.sessionId,
        elicitationId: params.elicitationId ?? null,
        server: params._meta?.["io.peri/mcpElicitation"]?.server ?? null,
        tool: params._meta?.["io.peri/mcpElicitation"]?.tool ?? null,
        url: isUrl ? redactUrl(params.url) : null,
      };
      report.elicitations.push(entry);
      note("elicitation_create", entry);

      if (isUrl) {
        if (scenario === "url_cancel") {
          rpc.respond(message.id, { action: "cancel" });
          return;
        }
        if (authState === "pending") {
          // The external browser flow is still running: hold the repeated
          // requirement instead of accepting it blindly.
          await authPromise;
          rpc.respond(message.id, { action: "accept" });
          return;
        }
        rpc.respond(message.id, { action: "accept" });
        if (authState === "idle") {
          authState = "pending";
          authPromise = new Promise((resolve) => {
            const callback = new URL("/callback", params.url);
            const request = http.get(
              {
                hostname: callback.hostname,
                port: callback.port,
                path: callback.pathname,
              },
              (response) => {
                response.resume();
                note("browser_callback", { status: response.statusCode });
                authState = "done";
                resolve();
              },
            );
            request.on("error", (error) => {
              note("browser_callback_error", { error: String(error) });
              authState = "done";
              resolve();
            });
          });
        }
        return;
      }

      if (scenario === "ask_user_question") {
        rpc.respond(message.id, {
          action: "accept",
          content: { ask_user_question_0: "Edge" },
        });
        return;
      }
      const content = { approve: true, note: "fixture note" };
      const formMessage = params.message ?? "";
      if (scenario === "mrtr_parallel") {
        if (formMessage.includes("Approve a")) {
          rpc.respond(message.id, {
            action: "accept",
            content: { approve: true, note: "a" },
          });
        } else {
          rpc.respond(message.id, { action: "cancel" });
        }
        return;
      }
      if (scenario === "mrtr_two_rounds") {
        rpc.respond(message.id, { action: "accept", content });
        return;
      }
      switch (scenario) {
        case "form_accept_true":
          rpc.respond(message.id, { action: "accept", content });
          break;
        case "form_accept_false":
          rpc.respond(message.id, {
            action: "accept",
            content: { approve: false, note: "fixture note" },
          });
          break;
        case "form_decline":
          rpc.respond(message.id, { action: "decline" });
          break;
        case "form_cancel":
          rpc.respond(message.id, { action: "cancel" });
          break;
        default:
          rpc.respond(message.id, { action: "decline" });
      }
      return;
    }
    report.acpErrors.push({
      kind: "unhandled_request",
      method: message.method,
      params: message.params,
    });
    rpc.respondError(message.id, -32601, `unsupported: ${message.method}`);
  }

  const rpc = {
    request(method, params, timeoutMs = 60_000) {
      const id = nextId++;
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`timeout waiting for ${method}`));
        }, timeoutMs);
        pending.set(id, {
          resolve: (value) => {
            clearTimeout(timer);
            resolve(value);
          },
          reject: (error) => {
            clearTimeout(timer);
            reject(error);
          },
        });
      });
    },
    respond(id, result) {
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
    },
    respondError(id, code, message) {
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } })}\n`,
      );
    },
  };
  return rpc;
}

function mcpRecords() {
  if (!fs.existsSync(mcpState)) return [];
  return fs
    .readFileSync(mcpState, "utf8")
    .split("\n")
    .filter(Boolean)
    .flatMap((line) => {
      try {
        return [JSON.parse(line)];
      } catch {
        return [];
      }
    });
}

async function main() {
  const model = await startMockModel();
  const auth = await startAuthServer();
  writeConfig(model.port, auth.port);
  note("fixture_ready", { modelPort: model.port, authPort: auth.port });
  const child = spawnPeri();
  const rpc = createRpc(child);

  try {
    const init = await rpc.request("initialize", {
      protocolVersion: 1,
      clientCapabilities: {
        fs: { readTextFile: false, writeTextFile: false },
        terminal: false,
        ...(scenario === "no_interaction_host"
          ? {}
          : { elicitation: { form: {}, url: {} } }),
      },
      clientInfo: { name: "peri-0728-e2e", version: "1.0.0" },
    });
    note("acp_initialize", { protocolVersion: init?.protocolVersion ?? null });

    const session = await rpc.request("session/new", {
      cwd: workspace,
      mcpServers: [],
    });
    const sessionId = session.sessionId;
    note("session_new", { sessionId });

    // Wait until the fixture server is connected and tools are listed.
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      const records = mcpRecords();
      if (records.some((record) => record.event === "recv" && record.line.includes('"tools/list"'))) {
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }

    const prompt = await rpc.request(
      "session/prompt",
      {
        sessionId,
        prompt: [{ type: "text", text: "run the fixture tool" }],
      },
      60_000,
    );
    report.prompt = prompt;
    note("prompt_response", { stopReason: prompt?.stopReason ?? null });
  } catch (error) {
    report.acpErrors.push({ kind: "driver_error", error: String(error) });
  } finally {
    child.stdin.end();
    await new Promise((resolve) => setTimeout(resolve, 300));
    child.kill("SIGTERM");
    model.child.kill("SIGTERM");
    auth.server.close();
  }

  const records = mcpRecords();
  const calls = records.filter((record) => record.event === "tools_call");
  const executions = records.filter((record) => record.event === "execution");
  report.mcp = {
    transport_calls: calls.length,
    executions: executions.length,
    calls,
    executions_detail: executions,
  };
  report.model = fs.existsSync(modelLog)
    ? fs
        .readFileSync(modelLog, "utf8")
        .split("\n")
        .filter(Boolean)
        .flatMap((line) => {
          try {
            return [JSON.parse(line)];
          } catch {
            return [];
          }
        })
    : [];

  const scenarioChecks = {
    form_accept_true: () =>
      report.mcp.transport_calls === 2 &&
      report.mcp.executions === 1 &&
      calls[1]?.inputResponses?.approval?.action === "accept" &&
      calls[1]?.inputResponses?.approval?.content?.approve === true &&
      calls[1]?.inputResponses?.approval?.content?.note === "fixture note",
    form_accept_false: () =>
      report.mcp.transport_calls === 2 &&
      report.mcp.executions === 1 &&
      calls[1]?.inputResponses?.approval?.action === "accept" &&
      calls[1]?.inputResponses?.approval?.content?.approve === false,
    form_decline: () =>
      report.mcp.transport_calls === 2 &&
      report.mcp.executions === 1 &&
      calls[1]?.inputResponses?.approval?.action === "decline",
    form_cancel: () =>
      report.mcp.transport_calls === 2 &&
      report.mcp.executions === 1 &&
      calls[1]?.inputResponses?.approval?.action === "cancel",
    url_accept: () =>
      report.mcp.transport_calls >= 2 &&
      report.mcp.executions === 1 &&
      executions.some((record) => record.structured?.receipt === "receipt-001") &&
      calls.every(
        (record) =>
          !record.inputResponses ||
          !("content" in record.inputResponses.authorize),
      ) &&
      report.elicitations.some((entry) => entry.mode === "url"),
    url_cancel: () =>
      report.mcp.transport_calls === 2 &&
      report.mcp.executions === 0 &&
      report.elicitations.some((entry) => entry.mode === "url"),
    mrtr_two_rounds: () =>
      report.mcp.transport_calls === 3 &&
      report.mcp.executions === 1 &&
      calls[0].inputResponses == null &&
      calls[0].requestState == null &&
      calls[1].inputResponses?.first?.action === "accept" &&
      calls[1].requestState === "state-first" &&
      calls[2].inputResponses?.first?.action === "accept" &&
      calls[2].inputResponses?.second?.action === "accept" &&
      calls[2].requestState == null &&
      new Set(calls.map((record) => record.id)).size === calls.length &&
      calls.every((record) => record.name === "double_round") &&
      executions[0]?.structured?.requestState == null,
    mrtr_parallel: () => {
      if (report.mcp.transport_calls !== 4 || report.mcp.executions !== 2) {
        return false;
      }
      const retries = calls.filter((record) => record.inputResponses);
      const accepted = retries.find(
        (record) =>
          record.arguments?.action === "a" &&
          record.inputResponses.approval?.action === "accept" &&
          record.inputResponses.approval?.content?.note === "a",
      );
      const cancelled = retries.find(
        (record) =>
          record.arguments?.action === "b" &&
          record.inputResponses.approval?.action === "cancel",
      );
      const results = new Map(
        executions.map((record) => [record.structured?.action, record.structured]),
      );
      return (
        Boolean(accepted) &&
        Boolean(cancelled) &&
        results.get("a")?.result === "accepted" &&
        results.get("a")?.note === "a" &&
        results.get("b")?.result === "cancelled" &&
        report.elicitations.length === 2
      );
    },
    plain_tool: () =>
      report.mcp.transport_calls === 1 &&
      report.mcp.executions === 1 &&
      executions[0]?.structured?.result === "plain-ok" &&
      report.elicitations.length === 0,
    no_interaction_host: () =>
      report.mcp.executions === 1 &&
      executions[0]?.name === "plain_echo" &&
      report.elicitations.length === 0 &&
      report.acpErrors.some((entry) => entry.kind === "non_interactive_client") &&
      report.acpErrors.every((entry) => entry.kind !== "driver_error"),
    legacy_handshake: () => {
      const initialize = records.filter((record) => record.event === "initialize");
      return (
        report.mcp.transport_calls === 1 &&
        report.mcp.executions === 1 &&
        executions[0]?.structured?.result === "plain-ok" &&
        initialize.length >= 1 &&
        initialize.every((record) => !record.capabilities?.elicitation) &&
        calls.every((record) => record.clientCapabilities == null)
      );
    },
    ask_user_question: () =>
      report.mcp.transport_calls === 0 &&
      report.elicitations.length === 1 &&
      report.elicitations[0].mode === "form" &&
      report.elicitations[0].server === null &&
      report.model.some(
        (record) =>
          record.sawToolResult && String(record.lastContent).includes("Edge"),
      ),
  };
  const check = scenarioChecks[scenario];
  report.reasons = [];
  if (!check) {
    report.reasons.push(`unknown scenario: ${scenario}`);
  } else if (!check()) {
    report.reasons.push("scenario assertions failed");
  }
  report.pass = report.reasons.length === 0;

  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  process.exit(report.pass ? 0 : 1);
}

main().catch((error) => {
  report.acpErrors.push({ kind: "fatal", error: String(error) });
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  process.exit(1);
});
