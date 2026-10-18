/**
 * 自主委派评测（真实模型，gated）。
 *
 * 默认跳过；运行：
 *   cd e2e
 *   PERI_E2E_AUTONOMY=1 npm run e2e -- --file tests/subagent/autonomous-delegation.test.ts --serial --retry 0
 *
 * 需要 `.env` 的 OPENAI_API_KEY / OPENAI_BASE_URL / OPENAI_MODEL（与其它真实
 * 模型 e2e 相同）。任务文本**不提及** subagent、Agent 工具或 skill，衡量：
 * - 有效委派：父会话是否对独立分支发起 Agent 调用并产生子会话；
 * - 重复劳动：主 Agent 是否在委派后重写子 Agent 已写的同一文件；
 * - 质量：仓库可见测试 + 隐藏验收脚本是否都通过；
 * - 耗时：从提交任务到验收通过的墙钟时间；
 * - 成本：模型调用数（assistant 消息）与 `thread_goals.tokens_used`（仅 goal
 *   模式持久化；无则 null，不伪造 token/费用）。
 *
 * 指标写入 `e2e/results/autonomy/<timestamp>.json`（gitignored），并输出到
 * 测试日志。默认要求至少 1 次有效委派；可用
 * `PERI_E2E_AUTONOMY_MIN_DELEGATIONS=0` 跑基线对照（只记录、不要求委派）。
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { execFile } from "node:child_process";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { promisify } from "node:util";
import os from "node:os";
import path from "node:path";
import { buildPeriForE2e } from "../../helpers/build.js";
import { readDelegationMetrics } from "../../helpers/delegation-metrics.js";
import {
  createE2EModelHome,
  launchPeri,
  PROJECT_ROOT,
  sendPrompt,
  waitForStableScreen,
} from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

const execFileAsync = promisify(execFile);
const ENABLED = process.env.PERI_E2E_AUTONOMY === "1";
const MIN_DELEGATIONS = Number(process.env.PERI_E2E_AUTONOMY_MIN_DELEGATIONS ?? "1");
const FIXTURE = path.join(PROJECT_ROOT, "e2e", "fixtures", "autonomy-delegation");
const ACCEPTANCE_TIMEOUT_MS = 10 * 60_000;
const describeAutonomy = describe.skipIf(!ENABLED);

async function runCommand(
  cwd: string,
  args: string[],
): Promise<{ ok: boolean; output: string }> {
  try {
    const { stdout, stderr } = await execFileAsync(process.execPath, args, {
      cwd,
      timeout: 120_000,
      maxBuffer: 2 * 1024 * 1024,
    });
    return { ok: true, output: `${stdout}${stderr}` };
  } catch (error) {
    const failed = error as { stdout?: string; stderr?: string; message?: string };
    return {
      ok: false,
      output: `${failed.stdout ?? ""}${failed.stderr ?? ""}${failed.message ?? ""}`,
    };
  }
}

async function acceptance(work: string): Promise<{ ok: boolean; output: string }> {
  const tests = await runCommand(work, ["--test"]);
  const hidden = await runCommand(work, ["acceptance.mjs"]);
  return {
    ok: tests.ok && hidden.ok && hidden.output.includes("AUTONOMY_ACCEPTANCE_OK"),
    output: `--- node --test ---\n${tests.output}\n--- acceptance.mjs ---\n${hidden.output}`,
  };
}

describeAutonomy("subagent: autonomous delegation (real model, gated)", () => {
  let tester: TmuxTester | undefined;
  let home: string | undefined;
  let work: string | undefined;

  beforeAll(async () => {
    await buildPeriForE2e();
  }, 610_000);

  afterAll(async () => {
    if (tester?.isRunning()) await tester.stop();
    if (home) await rm(home, { recursive: true, force: true });
    if (work) await rm(work, { recursive: true, force: true });
  });

  it(
    "独立分支任务在不提示 subagent 的情况下被主动委派，且主 Agent 不重做",
    { timeout: 900_000 },
    async () => {
      work = await mkdtemp(path.join(os.tmpdir(), "peri-autonomy-work-"));
      await cp(FIXTURE, work, { recursive: true });
      home = createE2EModelHome();
      tester = await launchPeri({ cwd: work, env: { HOME: home } });

      const startedAt = Date.now();
      await sendPrompt(
        tester,
        "Three independent packages under `packages/` have failing checks: `reporting`, " +
          "`ledger`, and `scheduler`. Each package is self-contained. Investigate all three, " +
          "fix the defects in the source files, and run `node --test` from the workspace root " +
          "until it passes. Do not weaken the checks in the tests. When done, reply with a " +
          "short summary of the changes.",
      );

      let result = { ok: false, output: "" };
      while (Date.now() - startedAt < ACCEPTANCE_TIMEOUT_MS) {
        result = await acceptance(work);
        if (result.ok) break;
        await new Promise((resolve) => setTimeout(resolve, 5_000));
      }

      try {
        await waitForStableScreen(tester, 180_000);
      } catch (error) {
        console.log(`[autonomy] screen did not stabilize: ${String(error)}`);
      }
      const durationMs = Date.now() - startedAt;
      const metrics = readDelegationMetrics(home);
      const report = {
        scenario: "autonomous-delegation",
        durationMs,
        quality: result.ok,
        ...metrics,
      };

      const resultsDir = path.join(PROJECT_ROOT, "e2e", "results", "autonomy");
      await mkdir(resultsDir, { recursive: true });
      const stamp = new Date().toISOString().replace(/[:.]/g, "-");
      await writeFile(
        path.join(resultsDir, `${stamp}.json`),
        `${JSON.stringify(report, null, 2)}\n`,
      );
      console.log(`[autonomy] ${JSON.stringify(report)}`);
      if (!result.ok) console.log(result.output);

      expect(result.ok, `验收未通过：\n${result.output}`).toBe(true);
      if (MIN_DELEGATIONS > 0) {
        expect(
          metrics.effectiveDelegation,
          `期望有效委派：agentCalls=${metrics.agentCalls}, childThreads=${metrics.childThreadIds.length}`,
        ).toBe(true);
        expect(metrics.agentCalls).toBeGreaterThanOrEqual(MIN_DELEGATIONS);
      }
      expect(
        metrics.duplicateLabor,
        "主 Agent 不得在委派后重写子 Agent 已写的同一文件",
      ).toBe(0);
    },
  );
});
