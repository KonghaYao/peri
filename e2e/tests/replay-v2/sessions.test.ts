import { createServer } from "node:http";
import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";

function event(type: string, data: object): string { return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`; }
function prefix(text: string): string {
  return event("message_start", { type: "message_start", message: { id: "session-response", type: "message", role: "assistant", model: "fixture-model", content: [], stop_reason: null, usage: { input_tokens: 30, output_tokens: 0 } } })
    + event("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } })
    + event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
}
function answer(text: string, tail = ""): string {
  return prefix(text) + (tail ? event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: tail } }) : "")
    + event("content_block_stop", { type: "content_block_stop", index: 0 })
    + event("message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 12 } })
    + event("message_stop", { type: "message_stop" });
}
async function model(responseFor: (body: any) => { text: string; tail?: string }) {
  const server = createServer(async (req, res) => {
    try {
      const chunks: Buffer[] = []; for await (const chunk of req) chunks.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(chunks).toString());
      const response = responseFor(body);
      if (body.stream === false) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ id: "session-complete", type: "message", role: "assistant", model: "fixture-model", content: [{ type: "text", text: response.text }], stop_reason: "end_turn", stop_sequence: null, usage: { input_tokens: 30, output_tokens: 12 } }));
      } else { res.writeHead(200, { "content-type": "text/event-stream" }); res.end(answer(response.text, response.tail)); }
    } catch (error) { res.destroy(error as Error); }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("no fixture address");
  return { url: `http://127.0.0.1:${address.port}`, close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }) };
}
function key(execution: string, owner = "root") { return { owner, execution, call: "reason:1", attempt: 1, purpose: "reason" }; }
function bodyOf(entry: any): any { return JSON.parse(entry.request.body.toString()); }
async function chooseThread(tui: any, title: string): Promise<void> {
  await tui.waitFor((s: string) => s.includes("Threads") && s.includes(title), "target thread listed");
  await tui.key("Home");
  const selected = (screen: string) => screen.split("\n").some((line: string) => line.includes(">") && line.includes(title));
  // The two-row picker retains its selection across reopen; choose by the visible row identity.
  await tui.waitFor((s: string) => s.split("\n").some((line: string) => /[>]/.test(line) && /A_STREAM_IN_BACKGROUND|B_SEED_HISTORY/.test(line)), "thread selection visible");
  if (!selected(await tui.screen())) {
    await tui.key("Down"); await tui.waitFor(selected, "target thread selected");
  }
  await tui.save(`select-${title}`); await tui.key("Enter");
  await tui.waitFor((s: string) => !s.includes("Enter resume"), "thread picker closes after selecting the target");
}
function persisted(world: { home: string }): any[] {
  const db = new DatabaseSync(path.join(world.home, ".peri", "threads", "threads.db"), { readOnly: true });
  try { return db.prepare("SELECT thread_id, content FROM messages").all(); } finally { db.close(); }
}

describe("replay v2 · sessions and terminal lifecycle", () => {
  it("U01 a new process restores completed history and continues the same session", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U01", title: "进程重启后恢复历史并在原会话续聊", upstream: async () => model((body) => ({ text: JSON.stringify(body.messages).includes("AFTER_PROCESS_RESTART") ? "RESTART_CONTINUATION_DONE" : "BEFORE_RESTART_STORED" })), async run(ctx) {
      ctx.identity.arm(ctx.sessionIds.root!, "before-restart");
      let tui = await ctx.startTui("root", { label: "first-process" }); await tui.prompt("BEFORE_PROCESS_RESTART");
      await tui.waitFor((s) => s.includes("BEFORE_RESTART_STORED") && s.includes("Brewed for"), "first turn persisted");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 1);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("before-exit"); await tui.stop();
      expect(persisted(ctx.world).filter((r) => r.thread_id === ctx.sessionIds.root && r.content.includes("BEFORE_RESTART_STORED"))).toHaveLength(1);
      tui = await ctx.startTui("root", { label: "second-process" });
      await tui.waitFor((s) => s.includes("BEFORE_RESTART_STORED"), "history restored in a fresh process");
      expect(ctx.requests).toHaveLength(1);
      ctx.identity.arm(ctx.sessionIds.root!, "after-restart"); await tui.prompt("AFTER_PROCESS_RESTART");
      await tui.waitFor((s) => s.includes("RESTART_CONTINUATION_DONE") && s.includes("Brewed for"), "continued same session");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      expect(JSON.stringify(bodyOf(ctx.requests[1]).messages)).toContain("BEFORE_RESTART_STORED");
      expect(ctx.requests[1].context.owner).toBe(ctx.requests[0].context.owner);
      expect(ctx.requests[1].context.execution).not.toBe(ctx.requests[0].context.execution);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("continued");
    } });
  });

  it("U02 switching the real TUI settles the old stream and routes later inputs correctly", { timeout: 180_000 }, async () => {
    await runReplayScenario({ id: "U02", title: "同一 TUI 流式期间切换会话，旧流关闭且返回可续聊", sessionNames: ["A", "B"],
      upstream: async () => model((body) => { const text = JSON.stringify(body.messages);
        if (text.includes("A_CONTINUE_AFTER_SWITCH")) return { text: "A_RETURNED_CONTINUATION" };
        if (text.includes("B_CONTINUE_IN_FOCUS")) return { text: "B_FOCUSED_REPLY" };
        if (text.includes("A_STREAM_IN_BACKGROUND")) return { text: "A_STREAM_PREFIX", tail: " A_STREAM_FINISHED" };
        return { text: "B_HISTORY_ANCHOR" };
      }), runOptions: () => ({ gates: [{ identity: key("active-a", "A"), afterBytes: Buffer.byteLength(prefix("A_STREAM_PREFIX")) }] }), async run(ctx) {
        ctx.identity.arm(ctx.sessionIds.B!, "seed-b");
        let tui = await ctx.startTui("B", { label: "seed" }); await tui.prompt("B_SEED_HISTORY");
        await tui.waitFor((s) => s.includes("B_HISTORY_ANCHOR") && s.includes("Brewed for"), "B initial history");
        await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 1);
        ctx.identity.finish(ctx.sessionIds.B!); await tui.stop();
        tui = await ctx.startTui("A", { label: "switching" });
        ctx.identity.arm(ctx.sessionIds.A!, "active-a"); await tui.prompt("A_STREAM_IN_BACKGROUND");
        await ctx.run.waitReached(key("active-a", "A"));
        await tui.waitFor((s) => s.includes("A_STREAM_PREFIX"), "A prefix visible");
        await tui.command("threads"); await chooseThread(tui, "B_SEED_HISTORY");
        await tui.waitFor((s) => s.includes("B_HISTORY_ANCHOR") && !s.includes("A_STREAM_PREFIX"), "focused B history");
        // Peri load_session explicitly closes the old session; switching must cancel its transport.
        await ctx.proxy.waitFor(ctx.runId, (s: any) => s.calls.some((c: any) => c.identity.owner === "A" && c.terminal === "client_abort"));
        ctx.identity.finish(ctx.sessionIds.A!);
        await tui.save("b-focused-after-a-closed");
        ctx.identity.arm(ctx.sessionIds.B!, "focused-b"); await tui.prompt("B_CONTINUE_IN_FOCUS");
        await tui.waitFor((s) => s.includes("B_FOCUSED_REPLY") && s.includes("Brewed for"), "B completes after the route switch");
        ctx.identity.finish(ctx.sessionIds.B!);
        await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 3);
        expect(await tui.screen()).not.toContain("A_STREAM_FINISHED");
        await tui.command("threads"); await chooseThread(tui, "A_STREAM_IN_BACKGROUND");
        const screen = await tui.waitFor((s) => s.split("\n").some((line) => /│\s+A_STREAM_IN_BACKGROUND\s*$/.test(line)) && !s.includes("B_FOCUSED_REPLY"), "return to A user history after its stream was closed");
        expect(screen).not.toContain("B_FOCUSED_REPLY"); expect(screen).not.toContain("A_STREAM_FINISHED");
        ctx.identity.arm(ctx.sessionIds.A!, "returned-a"); await tui.prompt("A_CONTINUE_AFTER_SWITCH");
        await tui.waitFor((s) => s.includes("A_RETURNED_CONTINUATION") && s.includes("Brewed for"), "new execution in A");
        await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 4);
        expect(JSON.stringify(bodyOf(ctx.requests[3]).messages)).toContain("A_STREAM_IN_BACKGROUND");
        expect(JSON.stringify(bodyOf(ctx.requests[3]).messages)).not.toContain("A_STREAM_FINISHED");
        expect(JSON.stringify(bodyOf(ctx.requests[3]).messages)).not.toContain("B_FOCUSED_REPLY");
        ctx.identity.finish(ctx.sessionIds.A!); await tui.save("returned-a");
        const a = persisted(ctx.world).filter((r) => r.thread_id === ctx.sessionIds.A).map((r) => r.content).join("\n");
        const b = persisted(ctx.world).filter((r) => r.thread_id === ctx.sessionIds.B).map((r) => r.content).join("\n");
        expect(a).toContain("A_RETURNED_CONTINUATION"); expect(a).not.toContain("A_STREAM_FINISHED"); expect(a).not.toContain("B_FOCUSED_REPLY");
        expect(b).toContain("B_FOCUSED_REPLY"); expect(b).not.toContain("A_STREAM_FINISHED");
      } });
  });

  it("U03 queued inputs execute exactly once in user order", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U03", title: "流式期间连续提交输入，按用户顺序各执行一次", upstream: async () => model((body) => {
      const text = JSON.stringify(body.messages);
      return { text: text.includes("QUEUE_THIRD_INPUT") ? "THIRD_QUEUE_REPLY" : text.includes("QUEUE_SECOND_INPUT") ? "SECOND_QUEUE_REPLY" : "FIRST_QUEUE_PREFIX", tail: text.includes("QUEUE_SECOND_INPUT") ? "" : " FIRST_QUEUE_DONE" };
    }), runOptions: () => ({ gates: [{ identity: key("first"), afterBytes: Buffer.byteLength(prefix("FIRST_QUEUE_PREFIX")) }] }), async run(ctx) {
      const tui = await ctx.startTui("root"); ctx.identity.arm(ctx.sessionIds.root!, "first");
      await tui.prompt("QUEUE_FIRST_INPUT"); await ctx.run.waitReached(key("first"));
      await tui.waitFor((s) => s.includes("FIRST_QUEUE_PREFIX"), "first active stream");
      await tui.prompt("QUEUE_SECOND_INPUT"); await tui.prompt("QUEUE_THIRD_INPUT");
      expect(ctx.requests).toHaveLength(1); await tui.save("two-inputs-queued");
      ctx.identity.queue(ctx.sessionIds.root!, "second"); ctx.identity.queue(ctx.sessionIds.root!, "third");
      ctx.run.release(key("first"));
      const screen = await tui.waitFor((s) => s.includes("THIRD_QUEUE_REPLY") && s.includes("Brewed for"), "queued turns completed");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 3);
      const bodies = ctx.requests.map(bodyOf).map((b: any) => JSON.stringify(b.messages));
      expect(bodies[0]).not.toContain("QUEUE_SECOND_INPUT"); expect(bodies[1]).toContain("QUEUE_SECOND_INPUT");
      expect(bodies[1]).not.toContain("QUEUE_THIRD_INPUT"); expect(bodies[2]).toContain("SECOND_QUEUE_REPLY");
      expect(screen).toContain("SECOND_QUEUE_REPLY");
      expect(screen.indexOf("SECOND_QUEUE_REPLY")).toBeLessThan(screen.indexOf("THIRD_QUEUE_REPLY"));
      expect(new Set(ctx.requests.map((r: any) => r.context.execution)).size).toBe(3);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("queue-completed");
    } });
  });

  it("U04 clear creates a fresh conversation without leaking old messages", { timeout: 120_000 }, async () => {
    let awaitingFresh = false;
    let freshId = "";
    await runReplayScenario({ id: "U04", title: "/clear 创建新会话且不把旧上下文发给模型", upstream: async () => model((body) => ({ text: JSON.stringify(body.messages).includes("AFTER_CLEAR_INPUT") ? "FRESH_SESSION_REPLY" : "PRIVATE_OLD_SESSION_REPLY" })),
      runOptions: (ctx) => ({ resolveContext: async (raw: any, run: any) => {
        if (awaitingFresh && raw.owner !== ctx.sessionIds.root && !freshId) {
          const db = new DatabaseSync(path.join(ctx.world.home, ".peri", "threads", "threads.db"), { readOnly: true });
          let roots: Array<{ id: string }>;
          try { roots = db.prepare("SELECT id FROM threads WHERE id <> ? AND parent_thread_id IS NULL").all(ctx.sessionIds.root) as Array<{ id: string }>; } finally { db.close(); }
          expect(roots).toHaveLength(1); expect(roots[0]!.id).toBe(raw.owner);
          freshId = roots[0]!.id;
          ctx.identity.register(freshId, "fresh", "only new persisted root after explicit /clear and user submission");
          ctx.identity.arm(freshId, "fresh-input");
        }
        await ctx.identity.resolve(raw, run);
      } }), async run(ctx) {
      const tui = await ctx.startTui("root"); ctx.identity.arm(ctx.sessionIds.root!, "old");
      await tui.prompt("BEFORE_CLEAR_INPUT");
      await tui.waitFor((s) => s.includes("PRIVATE_OLD_SESSION_REPLY") && s.includes("Brewed for"), "old session complete");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 1);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.command("clear");
      await tui.waitFor((s) => !s.includes("PRIVATE_OLD_SESSION_REPLY") && !s.includes("BEFORE_CLEAR_INPUT"), "clear hides previous transcript");
      awaitingFresh = true; await tui.prompt("AFTER_CLEAR_INPUT");
      await tui.waitFor(ctx.checked((s: string) => s.includes("FRESH_SESSION_REPLY") && s.includes("Brewed for")), "fresh session completes");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      expect(freshId).not.toBe(""); expect(freshId).not.toBe(ctx.sessionIds.root);
      const body = JSON.stringify(bodyOf(ctx.requests[1]).messages);
      expect(body).not.toContain("BEFORE_CLEAR_INPUT"); expect(body).not.toContain("PRIVATE_OLD_SESSION_REPLY");
      expect(ctx.requests[1].context.owner).not.toBe(ctx.requests[0].context.owner);
      ctx.identity.finish(freshId); await tui.save("fresh-session");
    } });
  });

  it("U05 resizing long Markdown preserves an unsent draft and conversation", { timeout: 120_000 }, async () => {
    const markdown = "# Layout case\n\n| Name | Value |\n|---|---|\n| Unicode | 中文 🚀 |\n\n```ts\nconst stable = true;\n```\n\n" + Array.from({ length: 35 }, (_, i) => `Line ${i + 1}: preserved history`).join("\n") + "\nLAYOUT_REPLY_END";
    await runReplayScenario({ id: "U05", title: "长 Markdown 窄屏缩放后草稿与上下文保持", upstream: async () => model((body) => ({ text: JSON.stringify(body.messages).includes("DRAFT_SURVIVES_RESIZE") ? "RESIZED_INPUT_CONSUMED" : markdown })), async run(ctx) {
      const tui = await ctx.startTui("root", { rows: 60 }); ctx.identity.arm(ctx.sessionIds.root!, "layout"); await tui.prompt("RENDER_LONG_MARKDOWN");
      const rendered = await tui.waitFor((s) => s.includes("LAYOUT_REPLY_END") && s.includes("Brewed for"), "long markdown completed");
      expect(rendered).toContain("Name"); expect(rendered).toContain("Value");
      expect(rendered).toContain("中文 🚀"); expect(rendered).toContain("const stable = true;");
      await tui.save("markdown-before-resize");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 1); ctx.identity.finish(ctx.sessionIds.root!);
      await tui.type("DRAFT_SURVIVES_RESIZE"); await tui.waitFor((s) => s.includes("DRAFT_SURVIVES_RESIZE"), "draft visible");
      await tui.resize(64, 24);
      const narrow = await tui.waitFor((s) => s.includes("DRAFT_SURVIVES_RESIZE") && s.includes("LAYOUT_REPLY_END") && s.split("\n").some((line) => /^\s*─{35,45} RENDER_LONG_MARKDOWN\s*$/.test(line)), "draft and reflowed composer at narrow width");
      expect(narrow.split("\n")).toHaveLength(24);
      expect(ctx.requests).toHaveLength(1); await tui.save("narrow-draft");
      await tui.resize(120, 40); await tui.waitFor((s) => s.includes("DRAFT_SURVIVES_RESIZE"), "draft restored at wide width");
      ctx.identity.arm(ctx.sessionIds.root!, "resized-input"); await tui.key("Enter");
      await tui.waitFor((s) => s.includes("RESIZED_INPUT_CONSUMED") && s.includes("Brewed for"), "draft submits once");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      const body = JSON.stringify(bodyOf(ctx.requests[1]).messages); expect(body).toContain("中文 🚀"); expect(body).toContain("const stable = true;");
      expect((body.match(/DRAFT_SURVIVES_RESIZE/g) ?? []).length).toBe(1);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("resized-completed");
    } });
  });

  it("U06 manual compaction records its auxiliary call and continuation consumes summary", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "U06", title: "手动 compact 辅助调用被录制，续聊使用已提交摘要", upstream: async () => model((body) => {
      if (body.stream === false || !body.tools?.length) return { text: "COMPACT_SUMMARY_KEPT: The prior completed task recorded the seed value 731." };
      return { text: JSON.stringify(body.messages).includes("CONTINUE_AFTER_COMPACT") ? "COMPACT_CONTINUATION_DONE" : "ORIGINAL_LONG_REPLY_731 " + "history detail ".repeat(300) };
    }), async run(ctx) {
      let tui = await ctx.startTui("root"); ctx.identity.arm(ctx.sessionIds.root!, "seed-history"); await tui.prompt("COMPACT_SEED_INPUT_731");
      await tui.waitFor((s) => s.includes("Brewed for"), "history seed completed");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 1); ctx.identity.finish(ctx.sessionIds.root!);
      ctx.identity.arm(ctx.sessionIds.root!, "manual-compact", { purpose: "compact", call: "compact-command:0" });
      await tui.command("compact");
      await tui.waitFor(ctx.checked((s: string) => /已压缩|压缩完成|compaction completed|Compacted/.test(s)), "manual compaction committed");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 2);
      expect(ctx.requests[1].context.purpose).toBe("compact"); ctx.identity.finish(ctx.sessionIds.root!); await tui.save("compacted");
      await tui.stop();
      tui = await ctx.startTui("root", { label: "after-compact-restart" });
      expect(ctx.requests).toHaveLength(2);
      ctx.identity.arm(ctx.sessionIds.root!, "after-compact"); await tui.prompt("CONTINUE_AFTER_COMPACT");
      await tui.waitFor((s) => s.includes("COMPACT_CONTINUATION_DONE") && s.includes("Brewed for"), "compaction continuation complete");
      await ctx.proxy.waitFor(ctx.runId, (s: any) => s.active === 0 && s.calls.length === 3);
      const body = JSON.stringify(bodyOf(ctx.requests[2]).messages);
      expect(body).toContain("COMPACT_SUMMARY_KEPT"); expect(body).not.toContain("history detail ".repeat(100));
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("continued-after-compact");
    } });
  });
});
