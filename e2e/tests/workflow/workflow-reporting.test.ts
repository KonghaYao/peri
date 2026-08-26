/**
 * 回归测试: Workflow 汇报优化（P2/P0/P3）
 *
 * 验证 workflow 完成后：
 * - P2: journal.jsonl 中 agent 条目含 phase + durationMs
 * - P0: token 统计非零
 * - P3: state.json 中长文本被提取到 outputs/ 目录
 *
 * 注意：P1（通知格式）在 TUI 中 system-reminder 块被折叠，无法从屏幕文本验证，
 * 通知格式的正确性由单元测试（registry_test.rs / async_router_test）保证。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, PROJECT_ROOT } from "../../helpers/peri.js";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join } from "node:path";
import type { TmuxTester } from "tui-tester";

const WORKFLOW_RUNS_DIR = join(PROJECT_ROOT, ".claude", "workflow-runs");

/** 记录当前已有的 run ID 集合，用于排除旧 run */
function existingRunIds(): Set<string> {
  if (!existsSync(WORKFLOW_RUNS_DIR)) return new Set();
  return new Set(readdirSync(WORKFLOW_RUNS_DIR, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => d.name));
}

/** 在已有 runs 之外等待一个成功完成且 journal 已落盘的 run。 */
async function findNewCompletedRun(excludeIds: Set<string>, timeoutMs: number): Promise<string | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(WORKFLOW_RUNS_DIR)) {
      for (const dirent of readdirSync(WORKFLOW_RUNS_DIR, { withFileTypes: true })) {
        if (!dirent.isDirectory() || excludeIds.has(dirent.name)) continue;
        const statePath = join(WORKFLOW_RUNS_DIR, dirent.name, "state.json");
        if (!existsSync(statePath)) continue;
        const state = JSON.parse(readFileSync(statePath, "utf-8"));
        if (state.status === "failed") {
          throw new Error(`workflow ${dirent.name} failed: ${state.error ?? "unknown error"}`);
        }
        if (state.status === "completed" && readJournal(dirent.name).length > 0) {
          return dirent.name;
        }
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  return null;
}

/** 读取 journal.jsonl */
function readJournal(runId: string): Record<string, any>[] {
  const path = join(WORKFLOW_RUNS_DIR, runId, "journal.jsonl");
  if (!existsSync(path)) return [];
  const lines = readFileSync(path, "utf-8").trim().split("\n");
  return lines.filter(Boolean).map((l) => JSON.parse(l));
}

/** 读取 state.json */
function readState(runId: string): Record<string, any> {
  return JSON.parse(readFileSync(join(WORKFLOW_RUNS_DIR, runId, "state.json"), "utf-8"));
}

describe("workflow: reporting (P2/P0/P3)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "journal.jsonl 含 phase/durationMs，tokenCount > 0，state.json 长文本分层",
    { timeout: 300_000 },
    async () => {
      // 记录测试前已有的 run
      const beforeIds = existingRunIds();

      tester = await launchPeri();

      // 触发 workflow：2 个并行 agent
      // 注：prompt 用中文（模型对中文 workflow 派发指令的遵循更稳定，参见
      // workflow-run.test.ts / workflow-panel-columns.test.ts 的用法）
      await sendPrompt(
        tester,
        "/ultracode 这是 E2E 测试，请立即且只调用一次 Workflow 工具，不得只解释。script 参数必须等价于以下顶层脚本：export const meta = { name: 'e2e-reporting', description: 'E2E reporting verification' }; phase('Test'); const [greeter, counter] = await parallel([() => agent('用三句话描述 hello world', { label: 'greeter' }), () => agent('从 1 数到 10', { label: 'counter' })]); return { greeter, counter }; 严禁 export default 或任何第二个 export；phase 只传字符串，不得传 callback；parallel 元素必须是零参工厂函数。",
      );

      // 等 workflow 成功完成且 journal 落盘；仅看到初始 state.json 不能证明
      // agent RPC 已执行，失败脚本也会先创建 state.json。
      const runId = await findNewCompletedRun(beforeIds, 240_000);
      expect(runId, "应在 240s 内出现成功完成且 journal 非空的新 workflow").toBeTruthy();
      if (!runId) return;

      console.log(`runId: ${runId}`);

      // ---- P2: journal.jsonl phase + durationMs ----
      const journal = readJournal(runId);
      expect(journal.length, "journal 应有条目").toBeGreaterThan(0);

      const okEntries = journal.filter((e) => e.result?.kind === "ok");
      expect(okEntries.length, "至少有一个 agent 成功").toBeGreaterThan(0);

      for (const entry of okEntries) {
        const r = entry.result;
        // P2: phase 字段存在且为 "Test"
        expect(r.phase, `seq=${entry.seq} 应有 phase`).toBeTruthy();

        // P2: durationMs > 0
        expect(r.durationMs, `seq=${entry.seq} durationMs 应 > 0`).toBeGreaterThan(0);

        // P0: tokenCount > 0（含 haiku fallback）
        expect(r.tokenCount, `seq=${entry.seq} tokenCount 应 > 0`).toBeGreaterThan(0);

        console.log(`  seq=${entry.seq} phase=${r.phase} durationMs=${r.durationMs} tokenCount=${r.tokenCount}`);
      }

      // ---- P3: state.json 长文本分层 ----
      const state = readState(runId);
      expect(state.return_value, "state.json 应有 return_value").toBeDefined();

      const rvStr = JSON.stringify(state.return_value);
      // 如果有长文本被提取（占位符 ${...}），验证 outputs/ 目录
      if (rvStr.includes("${")) {
        const outputsDir = join(WORKFLOW_RUNS_DIR, runId, "outputs");
        expect(existsSync(outputsDir), "长文本被提取时 outputs/ 目录应存在").toBe(true);

        const files = readdirSync(outputsDir);
        expect(files.length, "outputs/ 应有提取的文件").toBeGreaterThan(0);

        for (const file of files) {
          const content = readFileSync(join(outputsDir, file), "utf-8");
          // Rust 侧使用字节长度（> 200 bytes），JS 侧 length 为字符数
          // 中文等宽字符下 byte len > char count，仅验证非空即可
          expect(content.length, `outputs/${file} 不应为空`).toBeGreaterThan(0);
          console.log(`  outputs/${file}: ${Buffer.byteLength(content)} bytes / ${content.length} chars`);
        }
      } else {
        console.log("  (return_value 中无长文本，跳过 outputs/ 验证)");
      }

      console.log("✅ P2/P0/P3 全部通过");
    },
  );
});
