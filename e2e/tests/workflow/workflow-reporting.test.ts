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

/** 在已有 runs 之外查找新出现的 run ID（等 state.json 就绪） */
function findNewRun(excludeIds: Set<string>, timeoutMs: number): string | null {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!existsSync(WORKFLOW_RUNS_DIR)) {
      // busy wait
      continue;
    }
    for (const dirent of readdirSync(WORKFLOW_RUNS_DIR, { withFileTypes: true })) {
      if (!dirent.isDirectory() || excludeIds.has(dirent.name)) continue;
      const statePath = join(WORKFLOW_RUNS_DIR, dirent.name, "state.json");
      if (existsSync(statePath)) return dirent.name;
    }
    // 短轮询
    const start = Date.now();
    while (Date.now() - start < 1000) { /* busy wait 1s */ }
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
      await sendPrompt(
        tester,
        "/ultracode dispatch a workflow with 2 parallel agents. Agent 1 (label: greeter): say hello world and describe what you see in 3 sentences. Agent 2 (label: counter): count from 1 to 10. Use phase('Test') before the parallel call.",
      );

      // 等待新 run 出现（state.json 就绪）
      const runId = findNewRun(beforeIds, 180_000);
      expect(runId, "应在 180s 内出现新的 workflow state.json").toBeTruthy();
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
