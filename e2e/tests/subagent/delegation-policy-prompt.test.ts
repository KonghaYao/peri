/**
 * 默认委派策略的 prompt 接线（L0，无真实模型/凭据/外网）。
 *
 * 用本地假 model server 捕获真实二进制的首个主请求，断言：
 * - `11_subagent` 默认策略包含主动委派与「按独立性/风险/验证成本判断」；
 * - 不再出现「2-3 个文件禁止委派」的硬门槛；
 * - 目录行携带 explorer 的有界描述与 `[readonly]`；
 * - 说明如何经 `agent://.../agent.md` 读取完整职责。
 *
 * 这不证明模型会主动选择委派（见 `autonomous-delegation.test.ts` 的 gated
 * 真实模型评测）；它只证明策略与角色目录确实进入运行时 system prompt。
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { buildPeriForE2e } from "../../helpers/build.js";
import { sendPrompt } from "../../helpers/peri.js";
import {
  launchReplayTui,
  makeReplayHome,
  startReplayModelServer,
  type ReplayServer,
} from "../../helpers/replay-model.js";
import { rm } from "node:fs/promises";
import type { TmuxTester } from "tui-tester";

function systemText(request: { body?: any }): string {
  const system = request.body?.system;
  if (typeof system === "string") return system;
  return (system ?? []).map((block: any) => block?.text ?? "").join("\n");
}

async function waitFor(
  predicate: () => boolean,
  timeoutMs: number,
  message: string,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(message);
}

describe("subagent: default delegation policy prompt wiring", () => {
  let tester: TmuxTester | undefined;
  let replay: ReplayServer | undefined;
  let home: string | undefined;

  beforeAll(async () => {
    await buildPeriForE2e();
  }, 610_000);

  afterAll(async () => {
    if (tester?.isRunning()) await tester.stop();
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
  });

  it(
    "system prompt 默认包含主动委派策略、角色描述与完整职责读取路径",
    { timeout: 120_000 },
    async () => {
      const sentinel = "PROMPT_WIRING_SENTINEL";
      replay = await startReplayModelServer([{ prompt: sentinel, text: "ok" }]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home });
      await sendPrompt(tester, sentinel);

      await waitFor(
        () => replay!.requests.some((request) => request.isMain),
        60_000,
        "未捕获到首个主模型请求",
      );
      const request = replay.requests.find((entry) => entry.isMain)!;
      const system = systemText(request);

      expect(system).toContain("## Default delegation policy");
      expect(system).toContain("Judge by independence, risk, and verification cost");
      expect(system).toContain("not by file count");
      expect(system).not.toContain("tasks involving only 2-3 files");
      expect(system).toContain("do not redo its investigation or edits in the foreground");
      expect(system).toContain("This default policy does not weaken or replace that mode");

      expect(system).toContain("- explorer [haiku] [readonly]");
      expect(system).toMatch(
        /- explorer \[haiku\] \[readonly\] \| description \(untrusted metadata\): Fast agent specialized for exploring codebases\./,
      );
      expect(system).toContain("agent://builtin/<id>/agent.md");
      expect(system).toContain("mcp_read_resource");
    },
  );
});
