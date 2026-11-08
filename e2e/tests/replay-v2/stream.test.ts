import { createServer, type ServerResponse } from "node:http";
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";

function event(type: string, data: object): string {
  return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
}

function startMessage(id: string): string {
  return event("message_start", { type: "message_start", message: {
    id, type: "message", role: "assistant", model: "fixture-model", content: [],
    stop_reason: null, stop_sequence: null, usage: { input_tokens: 20, output_tokens: 0 },
  } });
}

function textBlock(index: number, fragments: string[], complete = true): string {
  return event("content_block_start", { type: "content_block_start", index, content_block: { type: "text", text: "" } })
    + fragments.map((text) => event("content_block_delta", { type: "content_block_delta", index, delta: { type: "text_delta", text } })).join("")
    + (complete ? event("content_block_stop", { type: "content_block_stop", index }) : "");
}

function endMessage(stopReason = "end_turn"): string {
  return event("message_delta", { type: "message_delta", delta: { stop_reason: stopReason, stop_sequence: null }, usage: { output_tokens: 12 } })
    + event("message_stop", { type: "message_stop" });
}

function textReply(text: string): string {
  return startMessage("stream-text") + textBlock(0, [text]) + endMessage();
}

function callIdentity(execution = "turn-1", call = "reason:1", attempt = 1) {
  return { owner: "root", execution, call, attempt, purpose: "reason" };
}

async function fixture(handler: (body: any, response: ServerResponse, requestPath: string) => void | Promise<void>) {
  const requests: Array<{ body: any; path: string }> = [];
  const errors: string[] = [];
  const server = createServer((request, response) => { void (async () => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    requests.push({ body, path: request.url ?? "" });
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("upstream received replay metadata");
    await handler(body, response, request.url ?? "");
  })().catch((error) => {
    errors.push(String(error));
    if (!response.headersSent) response.writeHead(500, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: String(error) }));
  }); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture failed to listen");
  return {
    url: `http://127.0.0.1:${address.port}`, requests, errors,
    close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }),
  };
}

function sseResponse(response: ServerResponse, body: string): void {
  response.writeHead(200, { "content-type": "text/event-stream" });
  response.end(body);
}

function healthy(ctx: any, predicate: (screen: string) => boolean) {
  return (screen: string) => {
    if (ctx.upstream?.errors.length) throw new Error(`fixture rejected request: ${JSON.stringify(ctx.upstream.errors)}`);
    const errors = ctx.proxy.snapshot(ctx.runId).errors;
    if (errors.length) throw new Error(`proxy rejected stream scenario: ${JSON.stringify(errors)}`);
    return predicate(screen);
  };
}

async function finishTurn(ctx: any, count: number): Promise<void> {
  await ctx.proxy.waitFor(ctx.runId, (state: any) => {
    if (ctx.upstream?.errors.length) throw new Error(JSON.stringify(ctx.upstream.errors));
    if (state.errors.length) throw new Error(JSON.stringify(state.errors));
    return state.active === 0 && state.calls.length === count;
  }, { timeout: 30_000 });
  ctx.identity.finish(ctx.sessionIds.root);
}

async function beginTurn(ctx: any, tui: any, turn: string, prompt: string): Promise<void> {
  ctx.identity.arm(ctx.sessionIds.root, turn);
  await tui.prompt(prompt);
}

async function finalScreen(ctx: any, tui: any, marker: string): Promise<string> {
  return tui.waitFor(healthy(ctx, (screen) => screen.includes(marker) && screen.includes("Brewed for")), `${marker} and completed footer`, 60_000);
}

const S01_FIRST = startMessage("s01-first") + textBlock(0, ["S01_FIRST_BLOCK\n"]);
const S01_REPLY = S01_FIRST + textBlock(1, ["S01_SECOND_", "BLOCK\n"]) + endMessage();
const S05_PREFIX = startMessage("s05-interrupted") + textBlock(0, ["S05_PREFIX_VISIBLE"], false);

describe("S01-S06 真实 TUI 流式业务录制与独立离线重放", () => {
  it("S01 多轮文本按流式块顺序呈现并保留上轮上下文", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S01", title: "多轮文本与流式块顺序",
      upstream: async () => fixture((body, response) => {
        const history = JSON.stringify(body.messages);
        if (history.includes("S01_SECOND_TURN")) {
          expect(history).toContain("S01_FIRST_BLOCK");
          expect(history).toContain("S01_SECOND_BLOCK");
          sseResponse(response, textReply("S01_CONTEXT_PRESERVED"));
        } else {
          expect(history).toContain("S01_FIRST_TURN");
          sseResponse(response, S01_REPLY);
        }
      }),
      runOptions: () => ({ gates: [
        { identity: callIdentity(), afterBytes: Buffer.byteLength(S01_FIRST) },
        { identity: callIdentity("turn-2"), afterBytes: 0 },
      ] }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        await beginTurn(ctx, tui, "turn-1", "S01_FIRST_TURN");
        await ctx.run.waitReached(callIdentity(), { timeout: 30_000 });
        const prefix = await tui.waitFor(healthy(ctx, (screen) => screen.includes("S01_FIRST_BLOCK")), "first streamed block visible");
        expect(prefix).not.toContain("S01_SECOND_BLOCK");
        expect(prefix).not.toContain("Brewed for");
        await tui.save("first-block-visible");
        ctx.run.release(callIdentity());
        const first = await finalScreen(ctx, tui, "S01_SECOND_BLOCK");
        expect(first).toContain("S01_FIRST_BLOCK");
        expect(first).toContain("S01_SECOND_BLOCK");
        expect(first.indexOf("S01_FIRST_BLOCK")).toBeLessThan(first.indexOf("S01_SECOND_BLOCK"));
        await finishTurn(ctx, 1);
        await beginTurn(ctx, tui, "turn-2", "S01_SECOND_TURN");
        await ctx.run.waitReached(callIdentity("turn-2"), { timeout: 30_000 });
        await tui.waitFor((screen: string) => screen.includes("S01_SECOND_TURN") && !screen.includes("Brewed for"), "second turn enters loading");
        ctx.run.release(callIdentity("turn-2"));
        const second = await finalScreen(ctx, tui, "S01_CONTEXT_PRESERVED");
        expect(second).toContain("S01_FIRST_BLOCK");
        expect(second).toContain("S01_SECOND_BLOCK");
        await tui.save("multi-turn-completed");
        await finishTurn(ctx, 2);
        ctx.observe("text-order-and-context", { ordered: true, turns: 2 });
      },
    });
  });

  it("S02 分段工具参数重组后执行真实 Write 并显示完成卡片", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S02", title: "流式工具参数切分与 Unicode 文件效果",
      upstream: async (world: any) => fixture(async (body, response) => {
        const history = JSON.stringify(body.messages);
        expect(history).toContain("S02_STREAMED_WRITE");
        const result = (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : [])
          .find((block: any) => block.type === "tool_result" && block.tool_use_id === "s02-write");
        if (result) { expect(result.is_error ?? false).toBe(false); sseResponse(response, textReply("S02_WRITE_DONE")); return; }
        const args = JSON.stringify({ file_path: path.join(world.work, "split-参数.txt"), content: "STREAMED_ARGUMENTS_OK\n中文🚀\n" });
        const points = Array.from(args);
        const fragments: string[] = [];
        for (let offset = 0; offset < points.length; offset += 7) fragments.push(points.slice(offset, offset + 7).join(""));
        const wire = startMessage("s02")
          + event("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "s02-write", name: "Write", input: {} } })
          + fragments.map((partial_json) => event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json } })).join("")
          + event("content_block_stop", { type: "content_block_stop", index: 0 }) + endMessage("tool_use");
        response.writeHead(200, { "content-type": "text/event-stream" });
        const bytes = Buffer.from(wire);
        for (let offset = 0; offset < bytes.length; offset += 31) {
          response.write(bytes.subarray(offset, offset + 31));
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
        response.end();
      }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        await beginTurn(ctx, tui, "turn-1", "S02_STREAMED_WRITE");
        const screen = await finalScreen(ctx, tui, "S02_WRITE_DONE");
        expect(screen).toMatch(/✓\s+Write/);
        expect(screen).toContain("split-参数.txt");
        expect(await readFile(path.join(ctx.world.work, "split-参数.txt"), "utf8")).toBe("STREAMED_ARGUMENTS_OK\n中文🚀\n");
        await tui.save("split-arguments-tool-completed");
        await finishTurn(ctx, 2);
        ctx.observe("streamed-tool-result", { card: "Write success", exactFileContent: true });
      },
    });
  });

  it("S03 限流与服务故障重试后只呈现一次成功回复", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S03", title: "HTTP 429 与 503 后正常重试完成",
      upstream: async () => {
        let attempt = 0;
        return fixture((body, response) => {
          expect(JSON.stringify(body.messages)).toContain("S03_RETRY_REQUEST");
          attempt++;
          if (attempt <= 2) {
            response.writeHead(attempt === 1 ? 429 : 503, { "content-type": "application/json", "retry-after": "0" });
            response.end(JSON.stringify({ error: { type: attempt === 1 ? "rate_limit_error" : "overloaded_error", message: "temporary fixture failure" } }));
          } else {
            expect(attempt).toBe(3);
            sseResponse(response, textReply("S03_RECOVERED_ONCE"));
          }
        });
      },
      runOptions: () => ({ gates: [{ identity: callIdentity("turn-1", "reason:1", 3), afterBytes: 0 }] }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        const arrivals = new Map<number, number>();
        const observeAttempt = () => {
          for (const call of ctx.proxy.snapshot(ctx.runId).calls) {
            if (call.identity.owner !== "root" || call.identity.execution !== "turn-1" || call.identity.call !== "reason:1" || call.identity.purpose !== "reason") continue;
            if (!arrivals.has(call.identity.attempt)) arrivals.set(call.identity.attempt, performance.now());
          }
        };
        ctx.run.on("change", observeAttempt);
        const started = performance.now();
        await beginTurn(ctx, tui, "turn-1", "S03_RETRY_REQUEST");
        await ctx.run.waitReached(callIdentity("turn-1", "reason:1", 3), { timeout: 30_000 });
        ctx.run.off("change", observeAttempt);
        ctx.observe("retry-timing", {
          promptToThirdAttemptMs: performance.now() - started,
          requestIntervalsMs: [arrivals.get(2)! - arrivals.get(1)!, arrivals.get(3)! - arrivals.get(2)!],
          policy: "production default retry delay and jitter",
        });
        const waiting = await tui.screen();
        expect(waiting).not.toContain("S03_RECOVERED_ONCE");
        ctx.run.release(callIdentity("turn-1", "reason:1", 3));
        const screen = await finalScreen(ctx, tui, "S03_RECOVERED_ONCE");
        expect(screen.split("S03_RECOVERED_ONCE")).toHaveLength(2);
        await tui.save("retry-recovered");
        await finishTurn(ctx, 3);
        const calls = ctx.proxy.snapshot(ctx.runId).calls;
        expect(calls.map((call: any) => call.identity.attempt)).toEqual([1, 2, 3]);
        expect(new Set(calls.map((call: any) => call.identity.call)).size).toBe(1);
        ctx.observe("retry-recovered-once", { physicalAttempts: 3, visibleReplies: 1 });
      },
    });
  });

  it("S04 模型错误可见后仍能提交并完成下一条用户输入", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S04", title: "终态 HTTP 错误后的继续输入",
      upstream: async () => fixture((body, response) => {
        const history = JSON.stringify(body.messages);
        if (history.includes("S04_CONTINUE")) { sseResponse(response, textReply("S04_CONTINUED_OK")); return; }
        expect(history).toContain("S04_TRIGGER_ERROR");
        response.writeHead(400, { "content-type": "application/json" });
        response.end(JSON.stringify({ error: { type: "invalid_request_error", message: "S04_REJECTED" } }));
      }),
      runOptions: () => ({ gates: [{ identity: callIdentity("turn-2"), afterBytes: 0 }] }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        await beginTurn(ctx, tui, "turn-1", "S04_TRIGGER_ERROR");
        const failed = await tui.waitFor(healthy(ctx, (screen) => screen.includes("400") && screen.includes("anthropic")), "model HTTP 400 is visible", 30_000);
        expect(failed).not.toContain("S04_CONTINUED_OK");
        await tui.save("terminal-error-visible");
        await finishTurn(ctx, 1);
        await beginTurn(ctx, tui, "turn-2", "S04_CONTINUE");
        await ctx.run.waitReached(callIdentity("turn-2"), { timeout: 30_000 });
        await tui.waitFor((screen: string) => screen.includes("S04_CONTINUE") && !screen.includes("Brewed for"), "input after error begins a new turn");
        ctx.run.release(callIdentity("turn-2"));
        const screen = await finalScreen(ctx, tui, "S04_CONTINUED_OK");
        expect(screen.split("S04_CONTINUED_OK")).toHaveLength(2);
        await tui.save("continued-after-error");
        await finishTurn(ctx, 2);
        ctx.observe("error-then-next-input", { errorVisible: true, nextTurnCompleted: true });
      },
    });
  });

  it("S05 断流续请求被取消后，新任务不会混入旧流尾文", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S05", title: "断流、真实取消与新任务隔离",
      upstream: async () => {
        let interrupted: ServerResponse | undefined;
        let oldRequests = 0;
        const server = await fixture((body, response) => {
          const history = JSON.stringify(body.messages);
          if (history.includes("S05_NEW_TASK")) { sseResponse(response, textReply("S05_NEW_TASK_DONE")); return; }
          expect(history).toContain("S05_OLD_TASK");
          oldRequests++;
          if (oldRequests === 1) {
            interrupted = response;
            response.writeHead(200, { "content-type": "text/event-stream" });
            response.write(S05_PREFIX);
          } else {
            expect(oldRequests).toBe(2);
            sseResponse(response, textReply("S05_FORBIDDEN_OLD_TAIL"));
          }
        });
        return { ...server, disconnect() {
          if (!interrupted) throw new Error("initial streaming response not started");
          interrupted.destroy();
        } };
      },
      runOptions: () => ({ gates: [
        { identity: callIdentity(), afterBytes: Buffer.byteLength(S05_PREFIX) },
        { identity: callIdentity("turn-1", "reason:2"), afterBytes: 0 },
        { identity: callIdentity("turn-2"), afterBytes: 0 },
      ] }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        await beginTurn(ctx, tui, "turn-1", "S05_OLD_TASK");
        const partial = await tui.waitFor(healthy(ctx, (screen) => screen.includes("S05_PREFIX_VISIBLE")), "partial old reply is visibly rendered");
        expect(partial).not.toContain("S05_FORBIDDEN_OLD_TAIL");
        expect(partial).not.toContain("Brewed for");
        await tui.save("before-upstream-disconnect");
        ctx.run.release(callIdentity());
        if (ctx.mode === "record") ctx.upstream.disconnect();
        await ctx.run.waitReached(callIdentity("turn-1", "reason:2"), { timeout: 30_000 });
        await tui.key("C-c");
        await ctx.proxy.waitFor(ctx.runId, (state: any) => state.calls.some((call: any) => call.identity.execution === "turn-1" && call.identity.call === "reason:2" && call.terminal === "client_abort"), { timeout: 30_000 });
        await tui.waitFor((screen: string) => screen.includes("Brewed for"), "cancelled continuation leaves loading");
        await finishTurn(ctx, 2);
        await beginTurn(ctx, tui, "turn-2", "S05_NEW_TASK");
        await ctx.run.waitReached(callIdentity("turn-2"), { timeout: 30_000 });
        await tui.waitFor((screen: string) => screen.includes("S05_NEW_TASK") && !screen.includes("Brewed for"), "new task replaces cancelled loading state");
        ctx.run.release(callIdentity("turn-2"));
        const screen = await finalScreen(ctx, tui, "S05_NEW_TASK_DONE");
        expect(screen).not.toContain("S05_FORBIDDEN_OLD_TAIL");
        expect(screen).toContain("S05_PREFIX_VISIBLE");
        expect(screen.split("S05_PREFIX_VISIBLE")).toHaveLength(2);
        expect(screen.indexOf("S05_PREFIX_VISIBLE")).toBeLessThan(screen.indexOf("S05_NEW_TASK"));
        await tui.save("new-task-after-cancel");
        await finishTurn(ctx, 3);
        expect(ctx.proxy.snapshot(ctx.runId).calls.map((call: any) => call.terminal)).toEqual(["upstream_abort", "client_abort", "complete"]);
        ctx.observe("cancelled-old-execution-isolated", { upstreamInterrupted: true, continuationCancelled: true, newTaskCompleted: true, forbiddenTailAbsent: true });
      },
    });
  });

  it("S06 OpenAI Responses 模型驱动真实工具与 TUI 完成结果", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "S06", title: "Responses 协议真实工具与终态",
      configure: async (world: any) => {
        const file = path.join(world.home, ".peri/settings.json");
        const settings = JSON.parse(await readFile(file, "utf8"));
        settings.config.providers[0].type = "openai_responses";
        for (const profile of Object.values(settings.config.profiles) as any[]) profile.contextWindow = 120000;
        await writeFile(file, JSON.stringify(settings), { mode: 0o600 });
      },
      upstream: async (world: any) => fixture((body, response, requestPath) => {
        expect(requestPath).toBe("/responses");
        expect(JSON.stringify(body.input)).toContain("S06_RESPONSES_WRITE");
        const result = (body.input ?? []).find((item: any) => item.type === "function_call_output" && item.call_id === "s06-call");
        if (result) { sseResponse(response, responsesText("S06_RESPONSES_DONE")); return; }
        const args = JSON.stringify({ file_path: path.join(world.work, "responses.txt"), content: "RESPONSES_TOOL_EFFECT\n" });
        const item = { id: "s06-tool", type: "function_call", call_id: "s06-call", name: "Write", arguments: args, status: "completed" };
        sseResponse(response,
          event("response.created", { type: "response.created", response: { id: "resp_s06_tool" } })
          + event("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { ...item, arguments: "", status: "in_progress" } })
          + event("response.function_call_arguments.delta", { type: "response.function_call_arguments.delta", output_index: 0, item_id: item.id, delta: args })
          + event("response.function_call_arguments.done", { type: "response.function_call_arguments.done", output_index: 0, item_id: item.id, arguments: args })
          + event("response.output_item.done", { type: "response.output_item.done", output_index: 0, item })
          + event("response.completed", { type: "response.completed", response: { id: "resp_s06_tool", status: "completed", output: [item], usage: { input_tokens: 20, output_tokens: 10 } } }));
      }),
      run: async (ctx: any) => {
        const tui = await ctx.startTui("root");
        await beginTurn(ctx, tui, "turn-1", "S06_RESPONSES_WRITE");
        const screen = await finalScreen(ctx, tui, "S06_RESPONSES_DONE");
        expect(screen).toMatch(/✓\s+Write/);
        expect(screen).toContain("responses.txt");
        expect(screen.split("S06_RESPONSES_DONE")).toHaveLength(2);
        expect(await readFile(path.join(ctx.world.work, "responses.txt"), "utf8")).toBe("RESPONSES_TOOL_EFFECT\n");
        await tui.save("responses-tool-completed");
        await finishTurn(ctx, 2);
        ctx.observe("responses-real-tool-and-tui", { protocol: "openai_responses", toolCardComplete: true, fileEffectExact: true });
      },
    });
  });
});

function responsesText(text: string): string {
  const item = { id: "s06-message", type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text }] };
  return event("response.created", { type: "response.created", response: { id: "resp_s06_text" } })
    + event("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { ...item, content: [], status: "in_progress" } })
    + event("response.output_text.delta", { type: "response.output_text.delta", output_index: 0, item_id: item.id, content_index: 0, delta: text })
    + event("response.output_text.done", { type: "response.output_text.done", output_index: 0, item_id: item.id, content_index: 0, text })
    + event("response.output_item.done", { type: "response.output_item.done", output_index: 0, item })
    + event("response.completed", { type: "response.completed", response: { id: "resp_s06_text", status: "completed", output: [item], usage: { input_tokens: 20, output_tokens: 10 } } });
}
