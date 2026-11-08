import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import path from "node:path";
import type { ReplayWorld } from "../../helpers/llm-replay-tui.js";

export type ToolCaseId = "T01" | "T02" | "T03" | "T04" | "T05" | "T06";
type Body = Record<string, any>;
export interface ToolResult { type: string; tool_use_id: string; is_error?: boolean; content: unknown }
interface Step { id: string; name: string; input: Record<string, unknown>; result: { error?: boolean; includes?: string } }

export function resultBlocks(body: Body): ToolResult[] {
  return (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : [])
    .filter((block: any) => block.type === "tool_result");
}

export function assertToolResult(body: Body, id: string, expected: Step["result"]): ToolResult {
  const results = resultBlocks(body).filter((block) => block.tool_use_id === id);
  if (results.length !== 1) throw new Error(`expected exactly one result for ${id}, got ${results.length}`);
  const result = results[0]!;
  if (Boolean(result.is_error) !== Boolean(expected.error)) throw new Error(`${id}: unexpected error state ${JSON.stringify(result)}`);
  if (expected.includes && !JSON.stringify(result.content).includes(expected.includes)) {
    throw new Error(`${id}: missing actual tool output ${expected.includes}: ${JSON.stringify(result)}`);
  }
  return result;
}

export function toolsPlan(id: ToolCaseId, world: ReplayWorld): Step[] {
  const file = (name: string) => path.join(world.work, name);
  switch (id) {
    case "T01": return [
      { id: "t01-write", name: "Write", input: { file_path: file("note.txt"), content: "T01_WRITTEN_AND_READ\n" }, result: {} },
      { id: "t01-read", name: "Read", input: { file_path: file("note.txt") }, result: { includes: "T01_WRITTEN_AND_READ" } },
    ];
    case "T02": return [
      { id: "t02-read-before", name: "Read", input: { file_path: file("config.txt") }, result: { includes: "mode=old" } },
      { id: "t02-edit", name: "Edit", input: { file_path: file("config.txt"), old_string: "mode=old", new_string: "mode=new" }, result: {} },
      { id: "t02-read-after", name: "Read", input: { file_path: file("config.txt") }, result: { includes: "mode=new" } },
    ];
    case "T03": return [
      { id: "t03-bash", name: "Bash", input: { command: "printf 'T03_FILE_EFFECT\\n' > command-result.txt && printf 'T03_STDOUT_CONFIRMED\\n'" }, result: { includes: "T03_STDOUT_CONFIRMED" } },
    ];
    case "T04": return [
      { id: "t04-missing", name: "Read", input: { file_path: file("missing.txt") }, result: { error: true, includes: "Error: File not found at " } },
      { id: "t04-repair", name: "Write", input: { file_path: file("missing.txt"), content: "T04_REPAIRED_CONTENT\n" }, result: {} },
      { id: "t04-read-fixed", name: "Read", input: { file_path: file("missing.txt") }, result: { includes: "T04_REPAIRED_CONTENT" } },
    ];
    case "T05": return [
      { id: "t05-discover", name: "SearchExtraTools", input: { query: "select:mcp_read_resource" }, result: { includes: "mcp_read_resource" } },
      { id: "t05-resource", name: "ExecuteExtraTool", input: { tool_name: "mcp_read_resource", params: { server_name: "workspace", uri: "skill://builtin/goal/SKILL.md" } }, result: { includes: "# Goal Mode" } },
    ];
    case "T06": return [
      { id: "t06-hooked-bash", name: "Bash", input: { command: "printf 'T06_UNCHECKED\\n' > hook-result.txt" }, result: { includes: "T06_HOOK_APPROVED_STDOUT" } },
    ];
  }
}

function sse(event: string, data: unknown): string { return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`; }
function responseBody(id: string, content: { text: string } | { step: Step }): string {
  let result = sse("message_start", { type: "message_start", message: { id, type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 12, output_tokens: 0 } } });
  if ("text" in content) {
    result += sse("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
    result += sse("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: content.text } });
  } else {
    result += sse("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: content.step.id, name: content.step.name, input: {} } });
    result += sse("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(content.step.input) } });
  }
  return result + sse("content_block_stop", { type: "content_block_stop", index: 0 })
    + sse("message_delta", { type: "message_delta", delta: { stop_reason: "text" in content ? "end_turn" : "tool_use", stop_sequence: null }, usage: { output_tokens: 8 } })
    + sse("message_stop", { type: "message_stop" });
}

/** The local model emits a script; every tool is executed by the real Peri process. */
export async function startToolsFixture(id: ToolCaseId, world: ReplayWorld) {
  const steps = toolsPlan(id, world);
  const requests: Body[] = [], errors: string[] = [];
  let main = 0, hooks = 0;
  const server = createServer((request, response) => { void handle(request, response).catch((error) => {
    errors.push(String(error)); response.writeHead(500); response.end(String(error));
  }); });
  async function handle(request: IncomingMessage, response: ServerResponse) {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body: Body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    requests.push(body);
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("private replay metadata leaked upstream");
    let reply: string;
    if (!Array.isArray(body.tools) || body.tools.length === 0) {
      if (id !== "T06" || !JSON.stringify(body).includes("T06_HOOK_EVALUATION")) throw new Error("unexpected auxiliary model request");
      hooks += 1;
      if (hooks !== 1) throw new Error("prompt hook executed more than once");
      reply = responseBody("t06-hook-response", { text: JSON.stringify({ hookSpecificOutput: {
        hookEventName: "PreToolUse", permissionDecision: "allow", updatedInput: {
          command: "printf 'T06_HOOK_APPROVED\\n' > hook-result.txt && printf 'T06_HOOK_APPROVED_STDOUT\\n'",
        },
      } }) });
    } else {
      if (!JSON.stringify(body.messages).includes(`${id}_BUSINESS_CASE`)) throw new Error(`missing ${id} user action`);
      if (main > steps.length) throw new Error("unexpected extra main model call");
      for (const previous of steps.slice(0, main)) assertToolResult(body, previous.id, previous.result);
      reply = responseBody(`${id.toLowerCase()}-message-${main + 1}`, main === steps.length ? { text: `${id}_BUSINESS_DONE` } : { step: steps[main]! });
      main += 1;
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(reply);
  }
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture model did not listen");
  return {
    url: `http://127.0.0.1:${address.port}`, requests, errors,
    close: () => new Promise<void>((resolve, reject) => { server.closeAllConnections(); server.close((error) => error ? reject(error) : resolve()); }),
  };
}
