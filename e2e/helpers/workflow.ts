/**
 * Workflow E2E 辅助：确定性 script 拼装 + 磁盘/屏幕双通道等待完成。
 */
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { TmuxTester } from "tui-tester";

import { PROJECT_ROOT, sendPrompt } from "./peri.js";

const WORKFLOW_RUNS_DIR = join(PROJECT_ROOT, ".claude", "workflow-runs");

export function existingWorkflowRunIds(): Set<string> {
  if (!existsSync(WORKFLOW_RUNS_DIR)) {
    return new Set();
  }
  return new Set(
    readdirSync(WORKFLOW_RUNS_DIR, { withFileTypes: true })
      .filter((d) => d.isDirectory())
      .map((d) => d.name),
  );
}

export function buildUltracodeWorkflowPrompt(script: string): string {
  return (
    "/ultracode 这是 E2E 测试。请立即且只调用一次 Workflow 工具，不得先试错或只解释。" +
    "script 参数必须等价于以下顶层脚本：" +
    script +
    " 严禁 export default 或任何第二个 export；phase 只传字符串，不得传 callback；parallel 元素必须是零参工厂函数。"
  );
}

export const E2E_WORKFLOW_RUN_OBSERVE_SCRIPT =
  "export const meta = { name: 'e2e-run-observe', description: 'E2E workflow panel observation' }; " +
  "phase('Run'); " +
  "const results = await parallel([" +
  "() => agent('只用 Bash 执行 echo hello-workflow-a', { label: 'agent-a' }), " +
  "() => agent('只用 Bash 执行 echo hello-workflow-b', { label: 'agent-b' })" +
  "]); " +
  "return { results };";

export const E2E_WORKFLOW_PANEL_COLUMNS_SCRIPT =
  "export const meta = { name: 'e2e-panel-columns', description: 'E2E panel columns' }; " +
  "phase('Run'); " +
  "const results = await parallel([" +
  "() => agent('只用 Bash 执行 echo hello-workflow-columns-a', { label: 'agent-a' }), " +
  "() => agent('只用 Bash 执行 echo hello-workflow-columns-b', { label: 'agent-b' })" +
  "]); " +
  "return { results };";

function journalHasEntries(runId: string): boolean {
  const path = join(WORKFLOW_RUNS_DIR, runId, "journal.jsonl");
  if (!existsSync(path)) {
    return false;
  }
  return readFileSync(path, "utf-8").trim().length > 0;
}

function findCompletedRunOnDisk(
  workflowName: string,
  excludeRunIds: Set<string>,
): string | null {
  if (!existsSync(WORKFLOW_RUNS_DIR)) {
    return null;
  }
  for (const dirent of readdirSync(WORKFLOW_RUNS_DIR, { withFileTypes: true })) {
    if (!dirent.isDirectory() || excludeRunIds.has(dirent.name)) {
      continue;
    }
    const statePath = join(WORKFLOW_RUNS_DIR, dirent.name, "state.json");
    if (!existsSync(statePath)) {
      continue;
    }
    const state = JSON.parse(readFileSync(statePath, "utf-8")) as {
      workflow_name?: string;
      status?: string;
    };
    if (state.workflow_name !== workflowName || state.status !== "completed") {
      continue;
    }
    if (!journalHasEntries(dirent.name)) {
      continue;
    }
    return dirent.name;
  }
  return null;
}

async function screenShowsCompletion(
  tester: TmuxTester,
  workflowName: string,
): Promise<boolean> {
  try {
    const screen = await tester.getScreenText();
    return screen.includes(`Workflow '${workflowName}' completed. (`);
  } catch {
    return false;
  }
}

/**
 * 等待指定 workflow 完成：优先读 `.claude/workflow-runs`（因果可靠），
 * 可选同时要求屏幕上出现完成通知（面板类用例）。
 */
export async function triggerWorkflowAndWait(
  tester: TmuxTester,
  workflowName: string,
  script: string,
  options: {
    timeoutMs?: number;
    requireScreenNotification?: boolean;
    maxAttempts?: number;
  } = {},
): Promise<string> {
  const maxAttempts = options.maxAttempts ?? 3;
  const timeoutMs = options.timeoutMs ?? 480_000;
  let lastError: unknown;

  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const excludeRunIds = existingWorkflowRunIds();
    const prompt =
      attempt === 1
        ? buildUltracodeWorkflowPrompt(script)
        : `${buildUltracodeWorkflowPrompt(script)} 上次未成功启动 workflow，请不要再解释，直接调用 Workflow。`;
    await sendPrompt(tester, prompt);
    try {
      return await waitForWorkflowCompleted(workflowName, {
        tester,
        excludeRunIds,
        timeoutMs,
        requireScreenNotification: options.requireScreenNotification,
      });
    } catch (err) {
      lastError = err;
    }
  }

  throw lastError instanceof Error
    ? lastError
    : new Error(`Workflow '${workflowName}' 在 ${maxAttempts} 次尝试后仍未完成`);
}

export async function waitForWorkflowCompleted(
  workflowName: string,
  options: {
    tester?: TmuxTester;
    timeoutMs?: number;
    excludeRunIds?: Set<string>;
    requireScreenNotification?: boolean;
  } = {},
): Promise<string> {
  const timeoutMs = options.timeoutMs ?? 360_000;
  const exclude = options.excludeRunIds ?? new Set<string>();
  const requireScreen = options.requireScreenNotification ?? false;
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    const runId = findCompletedRunOnDisk(workflowName, exclude);
    if (runId) {
      if (!requireScreen || !options.tester) {
        return runId;
      }
      if (await screenShowsCompletion(options.tester, workflowName)) {
        return runId;
      }
    }
    if (options.tester && (await screenShowsCompletion(options.tester, workflowName))) {
      const diskId = findCompletedRunOnDisk(workflowName, exclude);
      if (diskId) {
        return diskId;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 3000));
  }

  throw new Error(
    `Workflow '${workflowName}' 未在 ${timeoutMs}ms 内完成（磁盘 state + journal 或屏幕通知）`,
  );
}
