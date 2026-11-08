import { readFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { configureAgent, delegate, healthy, text, tools, write } from "../replay-v2/agents-fixture.js";
import { bodyText, CrossIdentities, crossProvider, key, observedRequest, occurrences, rootRows, uniqueResult } from "./cross-lifecycle-fixture.js";
import { assertOsc } from "./async-agent-fixture.js";

const RESULT = "J01_UNCOVERED_CHILD_RESULT_937";

async function compact(context: ReplayScenarioContext, actors: CrossIdentities, tui: any, epoch: string, expectedCalls: number, summary: string) {
  actors.armCompact(epoch);
  await tui.command("compact");
  await tui.waitFor(healthy(context, (screen) => /已压缩|压缩完成|compaction completed|Compacted/u.test(screen)), "manual full compact committed");
  await context.waitForCalls((state) => state.active === 0 && state.calls.length === expectedCalls);
  // A previous compact toast remains visible. Wait for this generation's durable summary.
  await expect.poll(() => rootRows(context).filter((row) => row.content.includes(summary)).length,
    { timeout: 10_000 }).toBe(1);
  await tui.save(epoch);
}

describe("cross-domain mailbox, model coverage and compaction epochs", () => {
  it("J01 uncovered async result survives Stop and Full, then becomes archivable only after a committed model response", { timeout: 180_000 }, async () => {
    let actors: CrossIdentities;
    await runReplayScenario({
      id: "J01", title: "异步结果跨 Stop 与两次 Full Compact，响应提交前保护、提交后归档",
      configure: configureAgent,
      upstream: async () => crossProvider((call) => {
        if (call.purpose === "compact") return text(call.execution === "compact-uncovered"
          ? "J01_SUMMARY_EPOCH_ONE: A delegated worker was launched after the user requested an isolated artifact."
          : "J01_SUMMARY_EPOCH_TWO: The worker artifact was verified and the handling receipt was saved once.");
        if (call.owner === "worker") return call.call === "reason:1"
          ? tools(write("J01-child-write", "j01-child.txt", "J01_REAL_CHILD_ARTIFACT\n")) : text(RESULT);
        if (call.execution === "delegate") {
          if (call.call === "reason:1") return tools(delegate("J01-spawn", "Produce the isolated child artifact and report it.", true));
          if (call.call === "reason:2") return text("J01_PARENT_BEFORE_RESULT " + "eligible previous context ".repeat(300) + "\nJ01_PARENT_SUSPENDED_READY");
          if (call.call === "reason:3") return text("J01_CANCELLED_RESPONSE_MUST_NOT_COMMIT");
        }
        if (call.execution === "explicit-continue") return call.call === "reason:1"
          ? tools(write("J01-handled", "j01-handled.txt", "J01_RESULT_HANDLED_ONCE\n")) : text("J01_RESULT_COVERED_BY_REAL_COMMIT");
        if (call.execution === "after-second-compact") return text("J01_SECOND_EPOCH_CONTINUED");
        throw new Error(`unexpected J01 model call ${JSON.stringify(call)}`);
      }),
      runOptions(context) {
        actors = new CrossIdentities(context, [{ tool: "J01-spawn", alias: "worker", action: "delegate" }]);
        return { resolveContext: actors.resolve, onRequest: actors.onRequest, gates: [
          { identity: key("worker", "created", "reason:2"), afterBytes: 0 },
          { identity: key("root", "delegate", "reason:3"), afterBytes: 0 },
        ] };
      },
      async run(context) {
        const tui = await context.startTui("root", { rows: 46 });
        actors.arm("delegate"); await tui.prompt("J01_START_DELEGATED_WORK");
        await context.run.waitReached(key("worker", "created", "reason:2"));
        await context.waitForCalls((state) => state.calls.some((call: any) => call.identity.owner === "root" && call.identity.call === "reason:2" && call.terminal === "complete"));
        await tui.waitFor(healthy(context, (screen) => screen.includes("J01_PARENT_SUSPENDED_READY") && screen.includes("Brewed for")), "parent suspension is actually rendered before the child resumes it");
        context.run.release(key("worker", "created", "reason:2"));
        await context.run.waitReached(key("root", "delegate", "reason:3"));
        const pending = observedRequest(context, "delegate", "reason:3");
        expect(occurrences(bodyText(pending), RESULT)).toBe(1);
        expect(await readFile(path.join(context.world.work, "j01-child.txt"), "utf8")).toBe("J01_REAL_CHILD_ARTIFACT\n");
        const before = uniqueResult(context, RESULT); expect(before.excluded).toBe(0);
        await tui.save("result-in-request-before-response-commit");
        await tui.waitFor(healthy(context, (screen) => !screen.includes("Brewed for")
          && /[✳✴✵✶✷✸✹✺✻✼❃❊].*\(\d+s(?: · [^)]*)?\)/u.test(screen)),
        "resumed main request is visibly running and cancellable before its first response byte", 10_000);
        await tui.key("C-c");
        await context.waitForCalls((state) => state.calls.find((call: any) => call.identity.owner === "root" && call.identity.call === "reason:3")?.terminal === "client_abort");
        // TurnInterrupted restores idle; the product does not add a cancellation transcript note.
        await tui.waitFor(healthy(context, (screen) => screen.includes("Brewed for")
          && !screen.includes("J01_CANCELLED_RESPONSE_MUST_NOT_COMMIT")), "real Stop ends visible main loading");
        await assertOsc(tui, "idle", "stopped-before-response-commit");
        expect(rootRows(context).some((row) => row.content.includes("J01_CANCELLED_RESPONSE_MUST_NOT_COMMIT"))).toBe(false);
        await compact(context, actors, tui, "compact-uncovered", 6, "J01_SUMMARY_EPOCH_ONE");
        const firstSummary = observedRequest(context, "compact-uncovered", "compact-command:0", "compact");
        expect(bodyText(firstSummary)).not.toContain(RESULT);
        expect(bodyText(firstSummary)).toContain("J01_PARENT_BEFORE_RESULT");
        const protectedRow = uniqueResult(context, RESULT);
        expect(protectedRow.message_id).toBe(before.message_id); expect(protectedRow.excluded).toBe(0);
        actors.arm("explicit-continue"); await tui.prompt("J01_EXPLICIT_CONTINUE_AFTER_STOP");
        await tui.waitFor(healthy(context, (screen) => screen.includes("J01_RESULT_COVERED_BY_REAL_COMMIT") && screen.includes("Brewed for")), "explicit continuation commits result handling");
        await context.waitForCalls((state) => state.active === 0 && state.calls.length === 8);
        const resumed = bodyText(observedRequest(context, "explicit-continue", "reason:1"));
        expect(resumed).toContain("J01_SUMMARY_EPOCH_ONE"); expect(occurrences(resumed, RESULT)).toBe(1);
        expect(await readFile(path.join(context.world.work, "j01-handled.txt"), "utf8")).toBe("J01_RESULT_HANDLED_ONCE\n");
        expect(uniqueResult(context, RESULT).message_id).toBe(before.message_id);
        await compact(context, actors, tui, "compact-covered", 9, "J01_SUMMARY_EPOCH_TWO");
        const secondSummary = bodyText(observedRequest(context, "compact-covered", "compact-command:0", "compact"));
        expect(occurrences(secondSummary, RESULT)).toBe(1);
        await expect.poll(() => uniqueResult(context, RESULT).excluded, { timeout: 10_000 }).toBe(1);
        const covered = uniqueResult(context, RESULT);
        expect(covered.message_id).toBe(before.message_id); expect(covered.excluded).toBe(1);
        actors.arm("after-second-compact"); await tui.prompt("J01_CONTINUE_NEXT_COMPACTION_EPOCH");
        await tui.waitFor(healthy(context, (screen) => screen.includes("J01_SECOND_EPOCH_CONTINUED") && screen.includes("Brewed for")), "second compact epoch continues");
        const finalRequest = bodyText(observedRequest(context, "after-second-compact", "reason:1"));
        expect(finalRequest).toContain("J01_SUMMARY_EPOCH_TWO"); expect(finalRequest).not.toContain(RESULT);
        context.observe("mailbox-compact-authority", { canonicalMessageId: before.message_id, beforeCoverageExcluded: 0,
          afterCoverageExcluded: 1, compactEpochs: 2, originalResultRows: 1, actualChildFile: true, actualHandledFile: true });
        await tui.save("two-epochs-canonical-result-retained");
      },
    });
  });

  it("J02 a new failed child result reactivates a failed parent without repeating the old failed input", { timeout: 180_000 }, async () => {
    let actors: CrossIdentities;
    await runReplayScenario({
      id: "J02", title: "父请求失败后新的后台错误自动唤醒，旧请求不自旋重试",
      configure: configureAgent,
      upstream: async () => crossProvider((call) => {
        if (call.owner === "worker") return call.call === "reason:1"
          ? tools(write("J02-started", "j02-child-started.txt", "J02_CHILD_EXECUTED_ONCE\n")) : { error: "J02_CHILD_BAD_REQUEST" };
        if (call.execution === "delegate") return call.call === "reason:1"
          ? tools(delegate("J02-spawn", "Write the child start evidence, then report.", true)) : { error: "J02_PARENT_BAD_REQUEST" };
        if (call.execution === "error-result-continuation") return call.call === "reason:1"
          ? tools(write("J02-error-handled", "j02-parent-handled.txt", "J02_NEW_ERROR_HANDLED_ONCE\n")) : text("J02_PARENT_REACTIVATED_BY_CHILD_ERROR");
        throw new Error(`unexpected J02 model call ${JSON.stringify(call)}`);
      }),
      runOptions(context) {
        actors = new CrossIdentities(context, [{ tool: "J02-spawn", alias: "worker", action: "delegate" }]);
        return { resolveContext: actors.resolve, onRequest: actors.onRequest,
          gates: [{ identity: key("worker", "created", "reason:1"), afterBytes: 0 }] };
      },
      async run(context) {
        const tui = await context.startTui("root"); actors.arm("delegate"); await tui.prompt("J02_START_BACKGROUND_AND_WAIT");
        await context.run.waitReached(key("worker", "created", "reason:1"));
        await tui.waitFor(healthy(context, (screen) => screen.includes("J02_PARENT_BAD_REQUEST")), "parent request visibly failed before child result");
        await context.waitForCalls((state) => state.calls.find((call: any) => call.identity.owner === "root" && call.identity.call === "reason:2")?.terminal === "complete");
        expect(rootRows(context).some((row) => row.content.includes("J02_CHILD_BAD_REQUEST"))).toBe(false);
        await tui.save("parent-failed-child-still-held");
        actors.arm("error-result-continuation"); context.run.release(key("worker", "created", "reason:1"));
        await tui.waitFor(healthy(context, (screen) => screen.includes("J02_PARENT_REACTIVATED_BY_CHILD_ERROR") && screen.includes("Brewed for")), "new task error activates a parent attempt without new input", 60_000);
        await context.waitForCalls((state) => state.active === 0 && state.calls.length === 6);
        const continued = observedRequest(context, "error-result-continuation", "reason:1");
        const content = bodyText(continued);
        expect(content).toContain("J02_CHILD_BAD_REQUEST"); expect(content).toContain("state: failed");
        const initial = observedRequest(context, "delegate", "reason:1");
        expect(continued.context.execution).not.toBe(initial.context.execution);
        const parentCalls = context.requests.filter((entry) => entry.context.owner === "root" && entry.context.execution === "delegate");
        expect(parentCalls.map((entry) => [entry.context.call, entry.context.attempt])).toEqual([["reason:1", 1], ["reason:2", 1]]);
        expect(await readFile(path.join(context.world.work, "j02-child-started.txt"), "utf8")).toBe("J02_CHILD_EXECUTED_ONCE\n");
        expect(await readFile(path.join(context.world.work, "j02-parent-handled.txt"), "utf8")).toBe("J02_NEW_ERROR_HANDLED_ONCE\n");
        const failedResult = uniqueResult(context, "J02_CHILD_BAD_REQUEST");
        const canonical = JSON.parse(failedResult.content);
        expect(canonical.type).toBe("system_reminder");
        // Configurable controls display filtering; EnsureProcessing controls queue activation.
        expect(canonical.reminder).toMatchObject({ category: "task", source: "subagent", delivery: "configurable",
          metadata: { success: false, timed_out: false } });
        expect(canonical.reminder.body).toContain("state: failed");
        expect(canonical.reminder.metadata.subagent_failure).toMatchObject({
          child_thread_id: actors.agents.children.get("worker"),
        });
        const rows = rootRows(context);
        expect(rows.filter((row) => row.role === "user" && row.content.includes("J02_START_BACKGROUND_AND_WAIT"))).toHaveLength(1);
        context.observe("failure-reactivation-authority", { originalUserInputs: 1, originalParentCalls: 2, oldRequestRetries: 0,
          newRootExecution: true, childResultRows: 1, childExecutedBeforeFailure: true, parentHandledRealError: true });
        await tui.save("new-error-processed-without-user-input");
      },
    });
  });
});
