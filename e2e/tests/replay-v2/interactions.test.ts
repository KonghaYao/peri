import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";

function event(type: string, data: object): string { return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`; }
function answer(value: string | { id: string; name: string; input: object }): string {
  const tool = typeof value !== "string";
  return event("message_start", { type: "message_start", message: { id: "interaction-response", type: "message", role: "assistant", model: "fixture-model", content: [], stop_reason: null, usage: { input_tokens: 10, output_tokens: 0 } } })
    + event("content_block_start", { type: "content_block_start", index: 0, content_block: tool ? { type: "tool_use", id: value.id, name: value.name, input: {} } : { type: "text", text: "" } })
    + event("content_block_delta", { type: "content_block_delta", index: 0, delta: tool ? { type: "input_json_delta", partial_json: JSON.stringify(value.input) } : { type: "text_delta", text: value } })
    + event("content_block_stop", { type: "content_block_stop", index: 0 })
    + event("message_delta", { type: "message_delta", delta: { stop_reason: tool ? "tool_use" : "end_turn" }, usage: { output_tokens: 10 } })
    + event("message_stop", { type: "message_stop" });
}
function results(body: any): any[] { return (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content.filter((block: any) => block.type === "tool_result") : []); }
function contentText(content: any): string { return typeof content === "string" ? content : content.map((block: any) => block.text).join("\n"); }
async function model(responseFor: (body: any) => string) {
  const server = createServer(async (req, res) => {
    try {
      const chunks: Buffer[] = []; for await (const chunk of req) chunks.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      res.writeHead(200, { "content-type": "text/event-stream" }); res.end(responseFor(body));
    } catch (error) { res.destroy(error as Error); }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("no fixture address");
  return { url: `http://127.0.0.1:${address.port}`, close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }) };
}
const question = (header: string, multiSelect = false) => ({ header, question: `Choose ${header}`, multiSelect,
  options: [{ label: "Alpha", description: "First option" }, { label: "Beta", description: "Second option" }, { label: "Gamma", description: "Third option" }] });

describe("replay v2 · user interaction ownership", () => {
  it("U07 single-choice answer reaches the owning tool and model", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U07", title: "AskUser 单选回答返回原工具与模型", upstream: async () => model((body) => {
      const done = results(body).find((block) => block.tool_use_id === "single-question");
      return done ? answer("SINGLE_CHOICE_CONSUMED") : answer({ id: "single-question", name: "AskUserQuestion", input: { questions: [question("SINGLE_TARGET")] } });
    }), async run(ctx) {
      ctx.identity.arm(ctx.sessionIds.root!, "single");
      const tui = await ctx.startTui("root");
      await tui.prompt("RUN_SINGLE_CHOICE");
      const panel = await tui.waitFor((s) => s.includes("Ask User") && s.includes("SINGLE_TARGET") && s.includes("Alpha"), "single-choice panel");
      expect(panel).not.toContain("SINGLE_CHOICE_CONSUMED");
      await tui.key("Down"); await tui.key("Space");
      await tui.waitFor((s) => /●.*Beta/.test(s), "Beta selected");
      await tui.save("selected"); await tui.key("Enter");
      await tui.waitFor((s) => s.includes("SINGLE_CHOICE_CONSUMED") && s.includes("Brewed for"), "single-choice complete");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      const requests = ctx.requests.map((r: any) => JSON.parse(r.request.body.toString()));
      const result = requests.flatMap(results).find((r: any) => r.tool_use_id === "single-question");
      expect(result).toBeDefined(); expect(contentText(result.content)).toBe("[问: SINGLE_TARGET]\n回答: Beta");
      expect(result.is_error ?? false).toBe(false);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("completed");
    } });
  });

  it("U08 multiple questions preserve multi-select answers and order", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U08", title: "AskUser 多题多选保持答案顺序", upstream: async () => model((body) => {
      const done = results(body).find((block) => block.tool_use_id === "multi-question");
      return done ? answer("MULTI_CHOICE_CONSUMED") : answer({ id: "multi-question", name: "AskUserQuestion", input: { questions: [question("MULTIPLE_TARGET", true), question("SECOND_TARGET")] } });
    }), async run(ctx) {
      ctx.identity.arm(ctx.sessionIds.root!, "multiple");
      const tui = await ctx.startTui("root"); await tui.prompt("RUN_MULTIPLE_CHOICE");
      await tui.waitFor((s) => s.includes("Ask User") && s.includes("MULTIPLE_TARGET"), "multiple-choice panel");
      await tui.key("Space"); await tui.waitFor((s) => /☑.*Alpha/.test(s), "Alpha checked");
      await tui.key("Down"); await tui.key("Space"); await tui.waitFor((s) => /☑.*Beta/.test(s), "Beta checked");
      await tui.save("multiple-selected"); await tui.key("Tab");
      await tui.waitFor((s) => s.includes("Choose SECOND_TARGET"), "second question");
      await tui.key("Down"); await tui.key("Down"); await tui.key("Space");
      await tui.waitFor((s) => /●.*Gamma/.test(s), "Gamma selected");
      await tui.key("Enter");
      await tui.waitFor((s) => s.includes("MULTI_CHOICE_CONSUMED") && s.includes("Brewed for"), "multi-choice complete");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      const result = ctx.requests.map((r: any) => JSON.parse(r.request.body.toString())).flatMap(results).find((r: any) => r.tool_use_id === "multi-question");
      expect(result).toBeDefined();
      expect(contentText(result.content)).toBe("[问: MULTIPLE_TARGET]\n回答: Alpha, Beta\n\n[问: SECOND_TARGET]\n回答: Gamma");
      expect(result.is_error ?? false).toBe(false);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("completed");
    } });
  });

  it("U09 deny prevents writes and a later explicit approval executes once", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U09", title: "审批拒绝无副作用，下一轮允许只执行一次", upstream: async (world) => model((body) => {
      const messages = JSON.stringify(body.messages);
      const allow = messages.includes("RUN_ALLOW_TOOL");
      const id = allow ? "allowed-shell" : "denied-shell";
      const done = results(body).find((block) => block.tool_use_id === id);
      if (done) return answer(allow ? "APPROVED_ACTION_COMPLETE" : "DENIED_ACTION_COMPLETE");
      return answer({ id, name: "Bash", input: { command: `printf '${allow ? "ALLOW" : "DENY"}\\n' >> approval.txt`, description: "Append isolated approval evidence" } });
    }), async run(ctx) {
      const tui = await ctx.startTui("root", { permissionMode: "default" });
      ctx.identity.arm(ctx.sessionIds.root!, "denied"); await tui.prompt("RUN_DENY_TOOL");
      await tui.waitFor((s) => s.includes("Approval Required") || s.includes("审批请求"), "approval before denied tool");
      await tui.save("deny-prompt"); await tui.key("Escape");
      await tui.waitFor((s) => s.includes("DENIED_ACTION_COMPLETE") && s.includes("Brewed for"), "denied action settled");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      await expect(readFile(path.join(ctx.world.work, "approval.txt"), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
      ctx.identity.finish(ctx.sessionIds.root!);
      ctx.identity.arm(ctx.sessionIds.root!, "allowed"); await tui.prompt("RUN_ALLOW_TOOL");
      await tui.waitFor((s) => s.includes("Approval Required") || s.includes("审批请求"), "separate approval for later tool");
      await tui.save("allow-prompt"); await tui.key("Enter");
      await tui.waitFor((s) => s.includes("APPROVED_ACTION_COMPLETE") && s.includes("Brewed for"), "allowed action settled");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 4);
      expect(await readFile(path.join(ctx.world.work, "approval.txt"), "utf8")).toBe("ALLOW\n");
      const all = ctx.requests.map((r: any) => JSON.parse(r.request.body.toString())).flatMap(results);
      const denied = all.find((r: any) => r.tool_use_id === "denied-shell");
      const allowed = all.find((r: any) => r.tool_use_id === "allowed-shell");
      expect(denied).toBeDefined(); expect(denied.is_error).toBe(true);
      expect(contentText(denied.content)).toContain("Cancelled by user");
      expect(allowed).toBeDefined(); expect(allowed.is_error ?? false).toBe(false);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("completed");
    } });
  });
});
