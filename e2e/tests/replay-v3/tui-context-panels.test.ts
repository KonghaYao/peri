import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import type { ReplayTui } from "../../helpers/llm-replay-tui.js";
import { ALT_UP, bodyOf, clipboardPayloads, contextModel, latestUser, resultBlocks, resultText, stored, turn, userTexts } from "./tui-context-fixture.js";

async function clickText(tui: ReplayTui, needle: string): Promise<void> {
  const lines = (await tui.waitFor((s) => s.includes(needle), `click target ${needle} rendered`)).split("\n"), row = lines.findIndex((line) => line.includes(needle));
  expect(row, `visible click target ${needle}`).toBeGreaterThanOrEqual(0);
  const x = lines[row]!.indexOf(needle) + 1, y = row + 1;
  await tui.type(`\x1b[<0;${x};${y}M\x1b[<0;${x};${y}m`);
}

describe("replay v3 · tool cards, panels and clipboard", () => {
  it("L04 a large real Read result has bounded expandable summaries at both breakpoints", { timeout: 120_000 }, async () => {
    const text = Array.from({ length: 250 }, (_, i) => `L04_TOOL_ROW_${String(i).padStart(3, "0")} actual source evidence`).join("\n") + "\n";
    await runReplayScenario({ id: "L04", title: "250 行真实 Read 截断提示与折叠展开，标准屏 4 行及窄屏 2 行摘要边界", configure: async (world) => { await writeFile(path.join(world.work, "long-tool.txt"), text); },
      upstream: async () => contextModel((body) => resultBlocks(body).some((r) => r.tool_use_id === "l04-read") ? { text: "L04_READ_DONE" }
        : { tool: { id: "l04-read", name: "Read", input: { file_path: "long-tool.txt" } } }), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 100, rows: 36 });
      await turn(ctx, tui, "read-large", "L04_READ_LONG_FILE", "L04_READ_DONE", 2);
      const result = resultBlocks(bodyOf(ctx.requests[1])).find((r) => r.tool_use_id === "l04-read");
      expect(resultText(result)).toContain("L04_TOOL_ROW_000"); expect(resultText(result)).toContain("[Output truncated:");
      expect(resultText(result)).toContain("continue reading with offset="); expect(resultText(result)).not.toContain("L04_TOOL_ROW_249");
      expect(await readFile(path.join(ctx.world.work, "long-tool.txt"), "utf8")).toBe(text);
      expect(result.is_error ?? false).toBe(false);
      await tui.waitFor((s) => s.includes("Read") && s.includes("long-tool.txt"), "completed Read card header");
      const original = await tui.screen();
      await tui.type(ALT_UP); await tui.type(ALT_UP);
      if (original.includes("L04_TOOL_ROW_000")) { await tui.key("Enter"); await tui.waitFor((s) => !s.includes("L04_TOOL_ROW_000"), "card collapses"); }
      expect(await tui.screen()).not.toContain("L04_TOOL_ROW_000"); await tui.save("collapsed");
      await tui.key("Enter");
      const expanded = await tui.waitFor((s) => s.includes("L04_TOOL_ROW_000"), "expanded real output summary");
      expect(expanded).toContain("L04_TOOL_ROW_003"); expect(expanded).not.toContain("L04_TOOL_ROW_004"); expect(expanded).not.toContain("L04_TOOL_ROW_249");
      await tui.save("standard-four-lines"); await tui.resize(50, 24);
      const narrow = await tui.waitFor((s) => s.includes("L04_TOOL_ROW_000") && s.includes("L04_READ_DONE"), "compact tool summary");
      expect(narrow).toContain("L04_TOOL_ROW_001"); expect(narrow).not.toContain("L04_TOOL_ROW_002"); expect(narrow.split("\n")).toHaveLength(24);
      await tui.save("compact-two-lines"); await tui.resize(120, 40);
      await tui.waitFor((s) => s.includes("L04_TOOL_ROW_003"), "expanded preference survives resize");
      await tui.key("Enter"); await tui.waitFor((s) => !s.includes("L04_TOOL_ROW_000"), "refold without extra tool invocation");
      expect(ctx.requests).toHaveLength(2);
      expect(stored(ctx.world, ctx.sessionIds.root!).filter((m) => m.content.includes("[Output truncated:") && m.content.includes("L04_TOOL_ROW_000"))).toHaveLength(1);
      await tui.save("refolded"); await tui.key("Escape");
    } });
  });

  it("L07 model popup navigation and resizing preserve the editor draft and do not submit it", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "L07", title: "状态栏模型弹窗键盘焦点、窄屏布局和关闭后草稿恢复，无误提交或误配置写入", upstream: async () => contextModel(() => ({ text: "L07_DRAFT_ACCEPTED" })), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 120, rows: 40 });
      const config = await readFile(path.join(ctx.world.home, ".peri/settings.json"), "utf8");
      await tui.type("L07_PRESERVED_DRAFT"); await tui.waitFor((s) => s.includes("L07_PRESERVED_DRAFT"), "draft before panel");
      await clickText(tui, "fixture-model");
      await tui.waitFor((s) => s.includes("fable") && s.includes("opus") && s.includes("haiku"), "statusbar opens actual model popup");
      await tui.key("Down"); await tui.waitFor((s) => /❯\s+haiku/.test(s), "popup keyboard selection moves");
      await tui.type("L07_SWALLOWED_PANEL_KEYS");
      expect(ctx.requests).toHaveLength(0); await tui.save("panel-focused");
      await tui.resize(64, 24);
      const narrow = await tui.waitFor((s) => s.includes("fable") && s.includes("haiku") && /❯\s+haiku/.test(s), "model popup at 64 by 24");
      expect(narrow.split("\n")).toHaveLength(24); expect(narrow).not.toContain("L07_SWALLOWED_PANEL_KEYS");
      await tui.save("panel-narrow"); await tui.key("Escape");
      const restored = await tui.waitFor((s) => s.includes("L07_PRESERVED_DRAFT") && !s.includes("haiku"), "popup closes and editor regains focus");
      expect(restored).not.toContain("L07_SWALLOWED_PANEL_KEYS"); expect(ctx.requests).toHaveLength(0);
      expect(await readFile(path.join(ctx.world.home, ".peri/settings.json"), "utf8")).toBe(config);
      ctx.identity.arm(ctx.sessionIds.root!, "after-panel"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L07_DRAFT_ACCEPTED") && s.includes("Brewed for")), "preserved draft submits");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 1);
      expect(userTexts(bodyOf(ctx.requests[0]))).toEqual(["L07_PRESERVED_DRAFT"]);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("draft-submitted");
    } });
  });

  it("L08 dismissing rejection confirmation keeps the form, and a later question has fresh ownership", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "L08", title: "AskUser 拒绝确认可取消且保留选项，再确认拒绝与下一题状态隔离", upstream: async () => contextModel((body) => {
      const second = latestUser(body) === "L08_NEW_QUESTION", id = second ? "l08-new-question" : "l08-old-question";
      if (resultBlocks(body).some((r) => r.tool_use_id === id)) return { text: second ? "L08_NEW_ANSWER_CONSUMED" : "L08_REJECTION_CONSUMED" };
      return { tool: { id, name: "AskUserQuestion", input: { questions: [{ header: second ? "L08_FRESH" : "L08_ORIGINAL", question: "Choose a stable answer after inspecting the confirmation popup", multiSelect: false,
        options: [{ label: "Alpha", description: "First option" }, { label: "Beta", description: "Second option" }] }] } } };
    }), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 80, rows: 36 });
      ctx.identity.arm(ctx.sessionIds.root!, "reject-original"); await tui.prompt("L08_FIRST_QUESTION");
      await tui.waitFor((s) => s.includes("Ask User") && s.includes("L08_ORIGINAL") && s.includes("Alpha"), "original question");
      await tui.key("Down"); await tui.key("Space"); await tui.waitFor((s) => /●.*Beta/.test(s), "original Beta selected");
      await tui.key("Escape");
      await tui.waitFor((s) => s.includes("Reject Answer") && s.includes("Enter: confirm"), "rejection confirmation");
      expect(ctx.requests).toHaveLength(1); await tui.save("reject-confirmation");
      await tui.resize(64, 24);
      const narrow = await tui.waitFor((s) => s.includes("Reject Answer") && s.includes("Esc: cancel"), "confirmation is usable at narrow width");
      expect(narrow.split("\n")).toHaveLength(24); await tui.save("reject-confirmation-narrow");
      await tui.key("Escape");
      await tui.waitFor((s) => !s.includes("Reject Answer") && /●.*Beta/.test(s), "canceling confirmation preserves original selection");
      expect(ctx.requests).toHaveLength(1); await tui.key("Escape");
      await tui.waitFor((s) => s.includes("Reject Answer"), "second confirmation"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L08_REJECTION_CONSUMED") && s.includes("Brewed for")), "explicit rejection reaches model");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 2);
      const denied = resultBlocks(bodyOf(ctx.requests[1])).find((r) => r.tool_use_id === "l08-old-question");
      expect(denied).toBeDefined(); expect(denied.is_error).toBe(true); expect(resultText(denied)).toContain("用户拒绝回答");
      ctx.identity.finish(ctx.sessionIds.root!); await tui.resize(100, 40);
      ctx.identity.arm(ctx.sessionIds.root!, "answer-new"); await tui.prompt("L08_NEW_QUESTION");
      const fresh = await tui.waitFor((s) => s.includes("Ask User") && s.includes("L08_FRESH") && s.includes("Alpha"), "new form owner");
      expect(fresh).not.toMatch(/●.*Beta/); await tui.key("Space"); await tui.waitFor((s) => /●.*Alpha/.test(s), "fresh Alpha selected");
      await tui.key("Enter"); await tui.waitFor(ctx.checked((s: string) => s.includes("L08_NEW_ANSWER_CONSUMED") && s.includes("Brewed for")), "new owner consumes its answer");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 4);
      const answer = resultBlocks(bodyOf(ctx.requests[3])).find((r) => r.tool_use_id === "l08-new-question");
      expect(resultText(answer)).toBe("[问: L08_FRESH]\n回答: Alpha"); expect(answer.is_error ?? false).toBe(false);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("new-owner-answer");
    } });
  });

  it("L09 mouse selection copies exact rendered text through OSC52 without submitting or folding", { timeout: 120_000 }, async () => {
    const copy = "L09_COPY_EXACT_731";
    await runReplayScenario({ id: "L09", title: "真实鼠标拖选与 OSC52 剪贴板精确文本，缩放后坐标重算且无误提交", upstream: async () => contextModel(() => ({ text: `L09_ASSISTANT_LINE ${copy} suffix\n\nL09_COPY_END` })), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 120, rows: 40, env: { PERI_CLIPBOARD: "osc52" } });
      await turn(ctx, tui, "selection", "L09_RENDER_SELECTABLE_RESPONSE", "L09_COPY_END");
      for (const size of [{ cols: 120, rows: 40, name: "wide" }, { cols: 64, rows: 24, name: "narrow" }]) {
        if (size.name === "narrow") await tui.resize(size.cols, size.rows);
        const screen = await tui.waitFor((s) => s.includes(copy), `${size.name} selection target`);
        const lines = screen.split("\n"), row = lines.findIndex((line) => line.includes(copy));
        expect(row).toBeGreaterThanOrEqual(0);
        const from = lines[row]!.indexOf(copy) + 1, to = from + copy.length, y = row + 1;
        expect(to).toBeLessThan(size.cols);
        await tui.type(`\x1b[<0;${from};${y}M`);
        await tui.type(`\x1b[<32;${to};${y}M`);
        await tui.type(`\x1b[<0;${to};${y}m`);
        const deadline = Date.now() + 5_000;
        let payloads: string[] = [];
        while (Date.now() < deadline) {
          payloads = await clipboardPayloads(tui, `${size.name}-selection`);
          if (payloads.length >= (size.name === "wide" ? 1 : 2)) break;
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
        expect(payloads.at(-1)).toBe(copy); expect(await tui.screen()).toContain("L09_COPY_END");
      }
      expect(ctx.requests).toHaveLength(1);
      expect(stored(ctx.world, ctx.sessionIds.root!).filter((m) => m.role === "user")).toHaveLength(1);
      await tui.save("selected-without-new-turn");
    } });
  });

  it("L13 clicking the visible tool header toggles its body after a process-zone transition", { timeout: 120_000 }, async () => {
    await runReplayScenario({ id: "L13", title: "过程区过渡线后的真实工具标题鼠标命中，展开与折叠内容无额外执行", configure: async (world) => {
      await writeFile(path.join(world.work, "l13-tool.txt"), "L13_READ_BODY_EXACT\nL13_SECOND_LINE\n");
    }, upstream: async () => contextModel((body) => resultBlocks(body).some((r) => r.tool_use_id === "l13-read") ? { text: "L13_READ_FINISHED" }
      : { tool: { id: "l13-read", name: "Read", input: { file_path: "l13-tool.txt" } } }), async run(ctx) {
      const tui = await ctx.startTui("root", { cols: 100, rows: 32 });
      await turn(ctx, tui, "read", "L13_SHOW_TOOL_CARD", "L13_READ_FINISHED", 2);
      expect(resultText(resultBlocks(bodyOf(ctx.requests[1]))[0])).toContain("L13_READ_BODY_EXACT");
      const collapsed = await tui.waitFor((s) => s.includes("Read l13-tool.txt"), "visible tool header");
      expect(collapsed).not.toContain("L13_READ_BODY_EXACT"); await tui.save("before-header-click");
      await clickText(tui, "l13-tool.txt");
      await tui.waitFor((s) => s.includes("L13_READ_BODY_EXACT") && s.includes("L13_SECOND_LINE"), "clicking actual header expands tool body", 5_000);
      await tui.save("header-click-expanded"); await clickText(tui, "l13-tool.txt");
      await tui.waitFor((s) => !s.includes("L13_READ_BODY_EXACT") && s.includes("L13_READ_FINISHED"), "same header collapses tool body");
      expect(ctx.requests).toHaveLength(2); expect(stored(ctx.world, ctx.sessionIds.root!).filter((m) => m.content.includes("L13_READ_BODY_EXACT"))).toHaveLength(1);
      await tui.save("header-click-collapsed");
    } });
  });
});
