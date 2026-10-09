import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { buildPeriForE2e } from "../../helpers/build.js";
import { sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { captureBgStageBoundary, completedBgStage } from "../../helpers/bg-task-boundary.js";
import {
  launchReplayTui,
  makeReplayHome,
  replayDebug,
  startReplayModelServer,
  type ReplayServer,
  type ReplayStep,
} from "../../helpers/replay-model.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: background task display and automatic result processing", () => {
  let tester: TmuxTester | undefined;
  let replay: ReplayServer | undefined;
  let home: string | undefined;
  let work: string | undefined;

  beforeAll(async () => {
    await buildPeriForE2e();
  }, 610_000);

  afterEach(async (context) => {
    if (replay && context.task.result?.state === "fail") {
      console.log(replayDebug(replay));
      console.log(JSON.stringify(replay.requests.filter((request) => request.isMain).map((request) => ({
        matched: request.matched,
        tools: request.body.tools.map((tool: { name: string }) => tool.name),
        messages: request.body.messages.slice(-2),
      }))));
    }
    if (tester?.isRunning()) await tester.stop();
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
    if (work) await rm(work, { recursive: true, force: true });
    tester = undefined;
    replay = undefined;
    home = undefined;
    work = undefined;
  });

  for (const mode of ["agent", "fork", "shell"] as const) {
    it(`${mode} result is consumed without another user input`, { timeout: 180_000 }, async () => {
      const rootPrompt = `BG_ROOT_${mode.toUpperCase()}`;
      const childPrompt = `BG_CHILD_${mode.toUpperCase()}`;
      const output = `BG_OUTPUT_${mode.toUpperCase()}`;
      const parentReply = `BG_PARENT_PROCESSED_${mode.toUpperCase()}`;
      const shellInput = { command: `sleep 3 && printf ${output}`, run_in_background: mode === "shell" };
      const steps: ReplayStep[] = mode === "shell" ? [
        { prompt: rootPrompt, toolUses: [{ name: "Bash", input: shellInput }] },
        { prompt: 'kind="completed"', text: parentReply },
      ] : [
        {
          prompt: rootPrompt,
          toolUses: [{ name: "Agent", input: mode === "fork"
            ? { fork: true, run_in_background: true, prompt: childPrompt }
            : { subagent_type: "general-purpose", run_in_background: true, prompt: childPrompt } }],
        },
        { prompt: childPrompt, toolUses: [{ name: "Bash", input: shellInput }] },
        { text: output },
        { prompt: output, text: parentReply },
      ];
      replay = await startReplayModelServer(steps);
      home = await makeReplayHome(replay.port);
      work = await mkdtemp(path.join(os.tmpdir(), "peri-bg-work-"));
      tester = await launchReplayTui({ home, cwd: work });
      const boundary = captureBgStageBoundary(home);
      await sendPrompt(tester, rootPrompt);
      await tester.waitFor(
        (screen) => new RegExp(`◎ ${mode === "shell" ? "shell" : "agent"}`).test(screen),
        { timeout: 60_000, interval: 100, message: `${mode} must publish running activity` },
      );
      await takePeriSnapshot(tester, `bg-${mode}-running`);
      await tester.waitFor(
        () => completedBgStage(home!, boundary, mode === "shell" ? "shell" : "subagent", output, parentReply) !== undefined,
        { timeout: 60_000, interval: 100, message: `automatic ${mode} processing: ${replayDebug(replay)}` },
      );
      const evidence = completedBgStage(home, boundary, mode === "shell" ? "shell" : "subagent", output, parentReply);
      expect(evidence?.deliveryId).toBeDefined();
      expect(evidence?.responseMessageId).toBeDefined();
      if (mode !== "shell") expect(evidence?.childSessionId).toBeDefined();
      const processing = replay.requests.filter((request) => request.matched && request.userText.includes(
        mode === "shell" ? 'kind="completed"' : output,
      ));
      expect(replay.requests.filter((request) => request.matched), replayDebug(replay))
        .toHaveLength(mode === "shell" ? 2 : 4);
      expect(replay.misses(), replayDebug(replay)).toBe(1);
      expect(processing, replayDebug(replay)).toHaveLength(1);
      expect(processing[0].body.messages.filter((message: { role: string; content: unknown }) =>
        message.role === "user" && JSON.stringify(message.content).includes('kind=\\"completed\\"'),
      )).toHaveLength(1);
      await takePeriSnapshot(tester, `bg-${mode}-processed`);
    });
  }
});
