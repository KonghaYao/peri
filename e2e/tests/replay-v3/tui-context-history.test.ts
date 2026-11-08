import { writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import type { ReplayTui } from "../../helpers/llm-replay-tui.js";
import { ALT_UP, CTRL_END, bodyOf, clipboardPayloads, contextModel, inspectWindow, latestUser, modelText, resultBlocks, resultText, seekOldest, stored, turn } from "./tui-context-fixture.js";

async function compact(ctx: ReplayScenarioContext, tui: ReplayTui, epoch: number): Promise<void> {
  const total = ctx.requests.length + 1;
  ctx.identity.arm(ctx.sessionIds.root!, `compact-${epoch}`, { purpose: "compact", call: "compact-command:0" });
  await tui.command("compact");
  await tui.waitFor(ctx.checked((s: string) => /已压缩|压缩完成|compaction completed|Compacted/.test(s)), `epoch ${epoch} compact committed`);
  await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === total);
  expect(ctx.requests.at(-1)!.context.purpose).toBe("compact");
  ctx.identity.finish(ctx.sessionIds.root!);
  await tui.save(`compact-${epoch}`);
}

describe("replay v3 · long context and persisted history", () => {
  it("L01 pages beyond fifty persisted entries, preserving order and an unsent draft", { timeout: 180_000 }, async () => {
    await runReplayScenario({ id: "L01", title: "32 轮真实对话重启后跨 50 条窗口分页，历史顺序与草稿保留", upstream: async () => contextModel((body) => {
      const input = latestUser(body);
      return { text: input === "L01_DRAFT_未提交" ? "L01_DRAFT_ACCEPTED" : `L01_REPLY_${input.match(/L01_INPUT_(\d+)/)?.[1]}_STORED` };
    }), async run(ctx) {
      let tui = await ctx.startTui("root", { label: "writer", cols: 100, rows: 32 });
      for (let i = 1; i <= 32; i++) {
        const n = String(i).padStart(2, "0");
        await turn(ctx, tui, `turn-${n}`, `L01_INPUT_${n}`, `L01_REPLY_${n}_STORED`);
      }
      const messages = stored(ctx.world, ctx.sessionIds.root!);
      expect(messages.filter((m) => m.role === "user")).toHaveLength(32);
      expect(messages.filter((m) => m.role === "assistant")).toHaveLength(32);
      await tui.save("thirty-two-turns"); await tui.stop();
      tui = await ctx.startTui("root", { label: "reader", cols: 64, rows: 24 });
      const tail = await tui.waitFor((s) => s.includes("L01_REPLY_32_STORED"), "latest persisted page");
      expect(tail).not.toContain("L01_REPLY_01_STORED");
      await tui.type("L01_DRAFT_未提交");
      const head = await seekOldest(tui, "L01_REPLY_01_STORED");
      expect(head).toContain("L01_INPUT_01"); expect(head).toContain("L01_INPUT_02");
      expect(head.indexOf("L01_INPUT_01")).toBeLessThan(head.indexOf("L01_REPLY_01_STORED"));
      expect(head.indexOf("L01_REPLY_01_STORED")).toBeLessThan(head.indexOf("L01_INPUT_02"));
      expect(head).not.toContain("L01_REPLY_32_STORED"); expect(head).toContain("L01_DRAFT_未提交");
      expect(head.split("\n")).toHaveLength(24); expect(ctx.requests).toHaveLength(32);
      await tui.save("oldest-page-narrow-draft");
      await tui.resize(140, 44);
      await tui.waitFor((s) => s.split("\n").length === 44 && s.includes("L01_REPLY_01_STORED") && s.includes("L01_DRAFT_未提交")
        && s.split("\n").some((line) => /^ {10,}─{80,}/u.test(line)), "wide application layout before single End key");
      await tui.type(CTRL_END);
      await tui.waitFor((s) => s.includes("L01_REPLY_32_STORED") && s.includes("L01_DRAFT_未提交"), "back to latest with draft");
      const pages = await inspectWindow(ctx, ctx.sessionIds.root!);
      expect(pages.length).toBeGreaterThanOrEqual(2);
      const items = pages.flatMap((p) => p.items);
      expect(items).toHaveLength(messages.length);
      expect(new Set(items.map((item: any) => item.entryId)).size).toBe(messages.length);
      expect(JSON.stringify(pages[0])).toContain("L01_REPLY_32_STORED");
      expect(JSON.stringify(pages.at(-1))).toContain("L01_REPLY_01_STORED");
      expect(pages[0].beforeCursor).toBeTruthy(); expect(pages.at(-1).beforeCursor).toBeFalsy();
      ctx.identity.arm(ctx.sessionIds.root!, "submit-preserved-draft"); await tui.key("Enter");
      await tui.waitFor(ctx.checked((s: string) => s.includes("L01_DRAFT_ACCEPTED") && s.includes("Brewed for")), "draft submitted after paging");
      await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === 33);
      expect(modelText(bodyOf(ctx.requests.at(-1)))).toContain("L01_REPLY_01_STORED");
      expect(modelText(bodyOf(ctx.requests.at(-1))).match(/L01_DRAFT_未提交/g)).toHaveLength(1);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("continued");
      ctx.observe("history-pages", { count: pages.length, entries: items.length, first: "01", last: "32" });
    } });
  });

  it("L03 three compact epochs survive separate processes while display history stays complete", { timeout: 180_000 }, async () => {
    const detail = (epoch: number) => `L03_ORIGINAL_${epoch}_` + `epoch-${epoch}-private-detail `.repeat(180) + `\nL03_END_${epoch}`;
    await runReplayScenario({ id: "L03", title: "三代 compact 与每代独立进程重启，当前上下文和完整显示历史分离", upstream: async () => contextModel((body) => {
      if (body.stream === false || !body.tools?.length) {
        const epochs = [...modelText(body).matchAll(/L03_EPOCH_INPUT_(\d+)/g)].map((m) => Number(m[1]));
        return { text: `L03_SUMMARY_GENERATION_${Math.max(...epochs)}: Preserve ledger 731 and the completed generation.` };
      }
      const input = latestUser(body);
      return { text: input === "L03_FINAL_CONTINUATION" ? "L03_FINAL_SUMMARY_CONSUMED" : detail(Number(input.match(/L03_EPOCH_INPUT_(\d+)/)?.[1])) };
    }), async run(ctx) {
      let tui = await ctx.startTui("root", { label: "epoch-1" });
      for (let epoch = 1; epoch <= 3; epoch++) {
        await turn(ctx, tui, `epoch-${epoch}-input`, `L03_EPOCH_INPUT_${epoch}`, `L03_END_${epoch}`);
        const current = modelText(bodyOf(ctx.requests.at(-1)));
        if (epoch > 1) {
          expect(current).toContain(`L03_SUMMARY_GENERATION_${epoch - 1}`);
          expect(current).not.toContain(`epoch-${epoch - 1}-private-detail `.repeat(50));
        }
        await compact(ctx, tui, epoch);
        const compactInput = modelText(bodyOf(ctx.requests.at(-1)));
        expect(compactInput).toContain(`L03_EPOCH_INPUT_${epoch}`); expect(compactInput).toContain(`L03_ORIGINAL_${epoch}_`);
        await tui.stop();
        tui = await ctx.startTui("root", { label: `epoch-${epoch + 1}` });
        expect(ctx.requests).toHaveLength(epoch * 2);
      }
      await turn(ctx, tui, "final-continuation", "L03_FINAL_CONTINUATION", "L03_FINAL_SUMMARY_CONSUMED");
      const final = modelText(bodyOf(ctx.requests.at(-1)));
      expect(final).toContain("L03_SUMMARY_GENERATION_3");
      for (let epoch = 1; epoch <= 3; epoch++) expect(final).not.toContain(`L03_ORIGINAL_${epoch}_`);
      const persisted = stored(ctx.world, ctx.sessionIds.root!);
      for (let epoch = 1; epoch <= 3; epoch++) expect(persisted.filter((m) => m.role === "assistant" && m.content.includes(`L03_ORIGINAL_${epoch}_`))).toHaveLength(1);
      const oldest = await seekOldest(tui, "L03_ORIGINAL_1_");
      expect(oldest).toContain("L03_EPOCH_INPUT_1"); expect(oldest).not.toContain("L03_FINAL_SUMMARY_CONSUMED");
      await tui.save("history-survives-three-compactions"); await tui.type(CTRL_END);
      await tui.waitFor((s) => s.includes("L03_FINAL_SUMMARY_CONSUMED"), "latest after archived history traversal");
      const pages = await inspectWindow(ctx, ctx.sessionIds.root!);
      for (let epoch = 1; epoch <= 3; epoch++) expect(JSON.stringify(pages)).toContain(`L03_ORIGINAL_${epoch}_`);
      ctx.observe("compact-generations", { epochs: 3, processStarts: 4, compactCalls: ctx.requests.filter((r) => r.context.purpose === "compact").length });
    } });
  });

  it("L11 a multilingual oversized history entry loads real body chunks and exports losslessly", { timeout: 180_000 }, async () => {
    const enormous = "L11_BODY_BEGIN\n" + "中文跨块🚀UTF8边界 abcdefghijklmnopqrstuvwxyz\n".repeat(12_000) + "L11_BODY_END";
    expect(Buffer.byteLength(enormous)).toBeGreaterThan(512 * 1024);
    await runReplayScenario({ id: "L11", title: "超过 512 KiB 中文历史详情按协商大小分块加载，OSC52 导出与 SQLite 原文一致", upstream: async () => contextModel(() => ({ text: enormous })), async run(ctx) {
      let tui = await ctx.startTui("root", { label: "large-writer", rows: 32, env: { PERI_CLIPBOARD: "osc52" } });
      await turn(ctx, tui, "large-body", "L11_GENERATE_LARGE_HISTORY", "L11_BODY_END");
      const record = stored(ctx.world, ctx.sessionIds.root!).find((m) => m.role === "assistant" && m.content.includes("L11_BODY_BEGIN"));
      expect(record).toBeDefined(); expect(Buffer.byteLength(record!.content)).toBeGreaterThan(512 * 1024);
      await tui.stop();
      const pages = await inspectWindow(ctx, ctx.sessionIds.root!);
      const bodyDescriptor = pages.flatMap((p) => p.items).find((item: any) => item.body);
      expect(bodyDescriptor).toBeDefined(); expect(bodyDescriptor.body.totalBytes).toBe(Buffer.byteLength(record!.content));
      const chunkSize = bodyDescriptor.body.chunkSize;
      expect(chunkSize).toBeGreaterThan(0); expect(chunkSize).toBeLessThanOrEqual(256 * 1024);
      tui = await ctx.startTui("root", { label: "detail-reader", cols: 100, rows: 32, env: { PERI_CLIPBOARD: "osc52" } });
      const descriptor = await tui.waitFor((s) => s.includes("超大内容") && s.includes("按需加载详情"), "oversized body descriptor in fresh history");
      expect(descriptor).not.toContain("L11_BODY_END"); await tui.save("descriptor");
      await tui.type(ALT_UP); await tui.key("Enter");
      await tui.waitFor((s) => s.includes("History Detail") && s.includes(record!.content.slice(0, 24)) && !s.includes("加载中…"), "first actual history body chunk");
      await tui.save("chunk-one");
      const bytes = Buffer.from(record!.content), chunks = Math.ceil(bytes.length / chunkSize);
      for (let chunk = 1; chunk <= chunks; chunk++) {
        const prefix = bytes.subarray(0, Math.min(bytes.length, chunk * chunkSize));
        let validPrefix: string | undefined;
        try { validPrefix = new TextDecoder("utf-8", { fatal: true }).decode(prefix); } catch { /* a real UTF-8 boundary fragment must remain unexportable */ }
        const deadline = Date.now() + 8_000;
        let confirmed = false;
        while (Date.now() < deadline) {
          await tui.type("e");
          const screen = await tui.screen(), copies = await clipboardPayloads(tui, "chunk-progress");
          confirmed = validPrefix !== undefined ? copies.at(-1) === validPrefix
            : !screen.includes("加载中…") && screen.includes("正文不是完整 UTF-8，无法复制") && screen.includes("字节为未完成字符");
          if (confirmed) break;
          await new Promise((resolve) => setTimeout(resolve, 30));
        }
        expect(confirmed, `chunk ${chunk} must expose exactly ${prefix.length} loaded bytes or its UTF-8 boundary error`).toBe(true);
        ctx.observe("detail-chunk-loaded", { chunk, loadedBytes: prefix.length, validUtf8Prefix: validPrefix !== undefined });
        if (chunk < chunks) {
          const revision = tui.outputRevision();
          await tui.key("Down");
          await tui.waitFor((s) => tui.outputRevision() > revision && s.includes("History Detail") && !s.includes("加载中…"), `new frame after body chunk ${chunk + 1}`);
        }
      }
      await tui.type("e");
      let copies: string[] = [];
      const deadline = Date.now() + 10_000;
      while (Date.now() < deadline) {
        copies = await clipboardPayloads(tui, "export");
        if (copies.at(-1) === record!.content) break;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      expect(copies.at(-1)).toBe(record!.content); expect(copies.at(-1)).not.toContain("�");
      await tui.key("Escape"); await tui.waitFor((s) => !s.includes("History Detail") && s.includes("超大内容"), "detail closes without losing descriptor");
      expect(ctx.requests).toHaveLength(1); await tui.save("detail-closed");
      ctx.observe("oversized-history", { bodyBytes: Buffer.byteLength(record!.content), chunkSize, chunks, copiedBytes: Buffer.byteLength(copies.at(-1)!) });
    } });
  });

  it("L12 real tool exchanges remain auditable over two compact and restart epochs", { timeout: 180_000 }, async () => {
    await runReplayScenario({ id: "L12", title: "两代真实 Read 工具往返经 compact 与重启后，工具结果归档和摘要上下文正确", configure: async (world) => {
      await writeFile(path.join(world.work, "ledger-one.txt"), "L12_LEDGER_ONE=731\n" + "ledger one proof ".repeat(200));
      await writeFile(path.join(world.work, "ledger-two.txt"), "L12_LEDGER_TWO=947\n" + "ledger two proof ".repeat(200));
    }, upstream: async () => contextModel((body) => {
      if (body.stream === false || !body.tools?.length) return { text: modelText(body).includes("L12_READ_TWO") ? "L12_SUMMARY_2 ledger one=731 two=947" : "L12_SUMMARY_1 ledger one=731" };
      const input = latestUser(body), two = input.includes("L12_READ_TWO");
      if (input.includes("L12_CONTINUE")) return { text: "L12_BOTH_LEDGERS_CONFIRMED" };
      const id = two ? "l12-read-two" : "l12-read-one";
      if (resultBlocks(body).some((r) => r.tool_use_id === id)) return { text: two ? "L12_TOOL_TWO_COMPLETE" : "L12_TOOL_ONE_COMPLETE" };
      return { tool: { id, name: "Read", input: { file_path: two ? "ledger-two.txt" : "ledger-one.txt" } } };
    }), async run(ctx) {
      let tui = await ctx.startTui("root", { label: "tools-epoch-one", rows: 44 });
      await turn(ctx, tui, "read-one", "L12_READ_ONE", "L12_TOOL_ONE_COMPLETE", 2);
      expect(resultText(resultBlocks(bodyOf(ctx.requests.at(-1)))[0])).toContain("L12_LEDGER_ONE=731");
      await compact(ctx, tui, 1); await tui.stop();
      tui = await ctx.startTui("root", { label: "tools-epoch-two", rows: 44 });
      await turn(ctx, tui, "read-two", "L12_READ_TWO", "L12_TOOL_TWO_COMPLETE", 2);
      const second = modelText(bodyOf(ctx.requests[3])); expect(second).toContain("L12_SUMMARY_1"); expect(second).not.toContain("ledger one proof ".repeat(50));
      expect(resultText(resultBlocks(bodyOf(ctx.requests.at(-1))).at(-1))).toContain("L12_LEDGER_TWO=947");
      await compact(ctx, tui, 2); await tui.stop();
      tui = await ctx.startTui("root", { label: "tools-final", rows: 44 });
      await turn(ctx, tui, "continue", "L12_CONTINUE", "L12_BOTH_LEDGERS_CONFIRMED");
      const final = bodyOf(ctx.requests.at(-1)); expect(modelText(final)).toContain("L12_SUMMARY_2"); expect(resultBlocks(final)).toHaveLength(0);
      const history = stored(ctx.world, ctx.sessionIds.root!).map((m) => m.content).join("\n");
      for (const marker of ["L12_LEDGER_ONE=731", "L12_LEDGER_TWO=947", "l12-read-one", "l12-read-two"]) expect(history).toContain(marker);
      const head = await seekOldest(tui, "L12_READ_ONE"); expect(head).toContain("Read"); expect(head).toContain("ledger-one.txt");
      await tui.save("old-tool-history"); await tui.type(CTRL_END);
      await tui.waitFor((s) => s.includes("L12_BOTH_LEDGERS_CONFIRMED"), "current generation after tool history browsing");
      const pages = await inspectWindow(ctx, ctx.sessionIds.root!);
      expect(JSON.stringify(pages)).toContain("L12_LEDGER_ONE=731"); expect(JSON.stringify(pages)).toContain("L12_LEDGER_TWO=947");
      ctx.observe("tool-context-generations", { compactCalls: 2, freshProcesses: 3, reasoningCalls: 5 });
    } });
  });
});
