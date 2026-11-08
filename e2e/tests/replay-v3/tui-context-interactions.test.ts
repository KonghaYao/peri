import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { CTRL_END, CTRL_HOME, bodyOf, contextModel, gateKey, latestUser, modelText, seekOldest, stored, textPrefix, turn, userTexts } from "./tui-context-fixture.js";

describe("replay v3 · streaming, input and layout", () => {
  it("L02 browsing during streaming shows new-output state and preserves an unsubmitted draft", { timeout: 120_000 }, async () => {
    const history = "L02_HISTORY_HEAD\n\n" + Array.from({ length: 75 }, (_, i) => `L02_SCROLL_LINE_${String(i).padStart(3, "0")}`).join("\n\n") + "\n\nL02_HISTORY_END";
    await runReplayScenario({ id: "L02", title: "流式期间浏览旧历史，新输出提示与回到底部及输入草稿正确", upstream: async () => contextModel((body) => {
      const input = latestUser(body);
      return input === "L02_START_STREAM" ? { text: "L02_STREAM_PREFIX", tail: "\nL02_STREAM_FINAL" }
        : { text: input === "L02_HELD_DRAFT" ? "L02_DRAFT_COMPLETED" : history };
    }), runOptions: () => ({ gates: [{ identity: gateKey("stream"), afterBytes: Buffer.byteLength(textPrefix("L02_STREAM_PREFIX")) }] }), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 100, rows: 32 });
      await turn(ctx, tui, "seed", "L02_SEED_HISTORY", "L02_HISTORY_END");
      ctx.identity.arm(ctx.sessionIds.root!, "stream"); await tui.prompt("L02_START_STREAM");
      await ctx.run.waitReached(gateKey("stream"));
      await tui.waitFor((s) => s.includes("L02_STREAM_PREFIX"), "live stream prefix");
      await tui.type("L02_HELD_DRAFT");
      const old = await seekOldest(tui, "L02_HISTORY_HEAD");
      expect(old).not.toContain("L02_STREAM_PREFIX"); expect(old).toContain("L02_HELD_DRAFT");
      await tui.save("browsing-active-stream"); ctx.run.release(gateKey("stream"));
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 2);
      const held = await tui.waitFor((s) => s.includes("New output") && s.includes("L02_HISTORY_HEAD"), "new output without stealing history viewport");
      expect(held).not.toContain("L02_STREAM_FINAL"); expect(held).toContain("L02_HELD_DRAFT");
      await tui.save("new-output-indicator"); await tui.type(CTRL_END);
      const bottom = await tui.waitFor((s) => s.includes("L02_STREAM_FINAL") && s.includes("L02_HELD_DRAFT") && !s.includes("New output"), "follow restored");
      expect(bottom).not.toContain("L02_HISTORY_HEAD"); ctx.identity.finish(ctx.sessionIds.root!);
      ctx.identity.arm(ctx.sessionIds.root!, "submit-draft"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L02_DRAFT_COMPLETED") && s.includes("Brewed for")), "held draft reaches model");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 3);
      expect(userTexts(bodyOf(ctx.requests.at(-1))).filter((s) => s === "L02_HELD_DRAFT")).toHaveLength(1);
      expect(modelText(bodyOf(ctx.requests.at(-1)))).toContain("L02_STREAM_FINAL");
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("draft-submitted");
    } });
  });

  it("L05 multiline Unicode paste is preserved across resize and submitted as one message", { timeout: 120_000 }, async () => {
    const input = "L05_FIRST 中文🚀 mixed width\nL05_SECOND é emoji 👩‍💻\nL05_LAST shell $VALUE and `literal`";
    await runReplayScenario({ id: "L05", title: "多行 Unicode 括号粘贴、窄宽屏切换与单次提交的真实编辑器链路", upstream: async () => contextModel(() => ({ text: "L05_MULTILINE_ACCEPTED" })), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 120, rows: 40 });
      await tui.paste(input);
      await tui.waitFor((s) => s.includes("L05_FIRST") && s.includes("L05_LAST"), "multiline draft is visible");
      expect(ctx.requests).toHaveLength(0); await tui.save("wide-paste");
      await tui.resize(64, 24);
      const narrow = await tui.waitFor((s) => s.includes("L05_FIRST") && s.includes("L05_LAST"), "multiline draft survives narrow layout");
      expect(narrow.split("\n")).toHaveLength(24); expect(narrow).toContain("中文🚀"); expect(narrow).not.toContain("�");
      await tui.save("narrow-paste"); await tui.resize(140, 48);
      await tui.waitFor((s) => s.includes("L05_SECOND") && s.includes("L05_LAST"), "wide multiline draft restored");
      expect(ctx.requests).toHaveLength(0);
      ctx.identity.arm(ctx.sessionIds.root!, "multiline-submit"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L05_MULTILINE_ACCEPTED") && s.includes("Brewed for")), "multiline submission complete");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 1);
      expect(userTexts(bodyOf(ctx.requests[0]))).toEqual([input]);
      const persisted = stored(ctx.world, ctx.sessionIds.root!).filter((m) => m.role === "user");
      expect(persisted).toHaveLength(1); expect(persisted[0]!.content).toContain("L05_FIRST"); expect(persisted[0]!.content).toContain("L05_LAST");
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("one-message-accepted");
    } });
  });

  it("L06 long code and table history reflows at narrow width without losing top or tail", { timeout: 120_000 }, async () => {
    const markdown = "# L06_MARKDOWN_BEGIN\n\n```typescript\nconst 中文 = 'L06_CODE_ANCHOR';\n" + Array.from({ length: 80 }, (_, i) => `const row${i} = 'value-${i}';`).join("\n")
      + "\n```\n\n| Key | Value |\n| --- | --- |\n" + Array.from({ length: 35 }, (_, i) => `| L06_TABLE_${i} | 中文🚀 value ${i} |`).join("\n") + "\n\nL06_MARKDOWN_END";
    await runReplayScenario({ id: "L06", title: "百行代码与表格真实渲染，窄屏滚动及宽屏回流保留首尾和完整模型历史", upstream: async () => contextModel((body) => ({ text: latestUser(body) === "L06_CONTINUE" ? "L06_CONTEXT_COMPLETE" : markdown })), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 120, rows: 40 });
      await turn(ctx, tui, "render", "L06_RENDER_LONG_MARKDOWN", "L06_MARKDOWN_END");
      const tail = await tui.screen(); expect(tail).toContain("L06_TABLE_34"); expect(tail).not.toContain("L06_MARKDOWN_BEGIN");
      await tui.save("long-tail");
      const head = await seekOldest(tui, "L06_CODE_ANCHOR"); expect(head).toContain("L06_MARKDOWN_BEGIN"); expect(head).toContain("const row0");
      expect(head.indexOf("L06_MARKDOWN_BEGIN")).toBeLessThan(head.indexOf("L06_CODE_ANCHOR")); await tui.save("long-head");
      await tui.resize(64, 24);
      // xterm can reflow the old wide frame before Peri applies SIGWINCH; fence on the actual narrow grid before End.
      const narrow = await tui.waitFor((s) => s.split("\n").length === 24 && s.includes("L06_MARKDOWN_BEGIN") && s.includes("L06_CODE_ANCHOR")
        && /^ │  │ const row0 = 'value-0';/mu.test(s) && /^─{20,}/mu.test(s), "narrow application grid preserves code viewport");
      expect(narrow.split("\n")).toHaveLength(24); expect(narrow).not.toContain("�");
      await tui.save("narrow-code"); await tui.type(CTRL_END);
      const narrowTail = await tui.waitFor((s) => s.includes("L06_TABLE_34") && s.includes("L06_MARKDOWN_END"), "narrow final table row");
      expect(narrowTail).toContain("│ L06_TABLE_34 │ 中文🚀 value 34 │");
      await tui.save("narrow-table"); await tui.resize(140, 48);
      await tui.waitFor((s) => s.split("\n").length === 48 && s.includes("L06_MARKDOWN_END")
        && s.split("\n").some((line) => /^ {10,}─{80,}/u.test(line)), "wide application grid keeps tail after second reflow");
      await turn(ctx, tui, "continue", "L06_CONTINUE", "L06_CONTEXT_COMPLETE");
      const messages = modelText(bodyOf(ctx.requests.at(-1)));
      for (const text of ["L06_CODE_ANCHOR", "const row79", "L06_TABLE_0", "L06_TABLE_34"]) expect(messages).toContain(text);
      expect(stored(ctx.world, ctx.sessionIds.root!).filter((m) => m.role === "assistant" && m.content.includes("L06_MARKDOWN_BEGIN"))).toHaveLength(1);
      await tui.save("complete-context");
    } });
  });

  it("L10 canceling a stream retains the editor draft and isolates the next execution", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "L10", title: "流式取消保留未提交草稿，下一次执行不含未接收尾部且身份隔离", upstream: async () => contextModel((body) => latestUser(body) === "L10_RECOVERY_DRAFT"
      ? { text: "L10_RECOVERED" } : { text: "L10_CANCEL_PREFIX", tail: " L10_FORBIDDEN_UNRECEIVED_TAIL" }),
    runOptions: () => ({ gates: [{ identity: gateKey("cancel-stream"), afterBytes: Buffer.byteLength(textPrefix("L10_CANCEL_PREFIX")) }] }), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 80, rows: 30 });
      ctx.identity.arm(ctx.sessionIds.root!, "cancel-stream"); await tui.prompt("L10_BEGIN_STREAM");
      await ctx.run.waitReached(gateKey("cancel-stream")); await tui.waitFor((s) => s.includes("L10_CANCEL_PREFIX"), "partial response shown");
      await tui.type("L10_RECOVERY_DRAFT"); await tui.waitFor((s) => s.includes("L10_RECOVERY_DRAFT"), "draft while active");
      await tui.key("C-c");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.some((c: any) => c.terminal === "client_abort"));
      const canceled = await tui.waitFor((s) => s.includes("L10_RECOVERY_DRAFT") && /[Cc]ancel|已取消/.test(s), "canceled turn and draft");
      expect(canceled).not.toContain("L10_FORBIDDEN_UNRECEIVED_TAIL"); await tui.save("canceled-draft");
      ctx.identity.finish(ctx.sessionIds.root!); ctx.identity.arm(ctx.sessionIds.root!, "recovery-draft"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L10_RECOVERED") && s.includes("Brewed for")), "new execution consumes retained draft");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 2);
      expect(ctx.requests[0]!.context.execution).not.toBe(ctx.requests[1]!.context.execution);
      const next = bodyOf(ctx.requests[1]); expect(userTexts(next).filter((t) => t === "L10_RECOVERY_DRAFT")).toHaveLength(1);
      expect(modelText(next)).not.toContain("L10_FORBIDDEN_UNRECEIVED_TAIL");
      expect(stored(ctx.world, ctx.sessionIds.root!).map((m) => m.content).join("\n")).not.toContain("L10_FORBIDDEN_UNRECEIVED_TAIL");
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("recovered-draft");
    } });
  });
});
