/**
 * 录制模块
 *
 * 职责：
 * - 将屏幕 ANSI 原始文本写入 snapshot 文件
 * - 维护 index.jsonl（所有 snapshot 的索引）
 * - 为 HTML 报告生成提供数据源
 */
import path from "node:path";
import fs from "node:fs/promises";
import { fileURLToPath } from "node:url";
import type { ScreenCapture } from "tui-tester";

// ---- 路径 ----

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * 录制目录：默认 e2e/recordings/，可通过环境变量 E2E_RECORDINGS_DIR 覆盖。
 * 并行执行（scripts/run-e2e.mjs）时为每个 worker 指定独立目录，
 * 避免多个 vitest 进程并发写同一个 index.jsonl 互相覆盖/损坏。
 */
export const RECORDINGS_DIR =
  process.env.E2E_RECORDINGS_DIR ??
  path.resolve(__dirname, "..", "recordings");
const INDEX_FILE = path.join(RECORDINGS_DIR, "index.jsonl");

// ---- 当前测试名（由 vitest setup 注入） ----

let _currentTestName = "unknown";

/** 由 setup.ts 在 beforeEach 中调用 */
export function setCurrentTestName(name: string): void {
  _currentTestName = name;
}

// ---- 索引记录类型 ----

export interface SnapshotRecord {
  /** 测试用例名 */
  test: string;
  /** 测试内步骤序号 */
  step: number;
  /** snapshot 名称 */
  name: string;
  /** ISO 时间戳 */
  timestamp: string;
  /** 快照文件相对路径（相对于 recordings/），扩展名取决于 recorderConfig.ansi */
  snapshotFile: string;
  /** 屏幕尺寸 */
  size: { cols: number; rows: number };
  /** 屏幕纯文本（去 ANSI）前 200 字符，方便 grep */
  textPreview: string;
  /** LLM judge 结果（可选） */
  verdict?: "pass" | "fail";
  checks?: JudgeCheckRecord[];
  duration_ms?: number;
}

export interface JudgeCheckRecord {
  criterion: string;
  pass: boolean;
  detail?: string;
}

// ---- 录制配置 ----

export interface RecorderConfig {
  /** 是否生成 .ansi 文件（含颜色 escape codes），默认 false，只生成 .txt 纯文本 */
  ansi: boolean;
}

export const recorderConfig: RecorderConfig = {
  ansi: false,
};

// ---- 工具函数 ----

/** 剥离 ANSI escape sequences，保留纯文本 */
function stripAnsi(str: string): string {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;]*m/g, "");
}

// ---- 内部计数器 ----

const stepCounters = new Map<string, number>();

function getNextStep(testName: string): number {
  const current = stepCounters.get(testName) ?? 0;
  const next = current + 1;
  stepCounters.set(testName, next);
  return next;
}

export function resetCounters(): void {
  stepCounters.clear();
}

// ---- 写入函数 ----

/**
 * 将屏幕内容写入文件
 *
 * - 始终写入 .txt（剥离 ANSI 的纯文本，方便编辑器内阅读）
 * - 根据 recorderConfig.ansi 决定是否额外写入 .ansi（含颜色 escape codes）
 *
 * 返回主文件名（.txt）
 */
export async function writeAnsiSnapshot(
  name: string,
  capture: ScreenCapture,
): Promise<string> {
  await fs.mkdir(RECORDINGS_DIR, { recursive: true });
  const safeName = name.replace(/[^a-zA-Z0-9_-]/g, "_");

  // 始终写纯文本 .txt
  const txtFileName = `${safeName}.txt`;
  const txtPath = path.join(RECORDINGS_DIR, txtFileName);
  await fs.writeFile(txtPath, stripAnsi(capture.raw), "utf-8");

  // 按配置可选写 .ansi
  if (recorderConfig.ansi) {
    const ansiFileName = `${safeName}.ansi`;
    const ansiPath = path.join(RECORDINGS_DIR, ansiFileName);
    await fs.writeFile(ansiPath, capture.raw, "utf-8");
  }

  return txtFileName;
}

/**
 * 追加一条记录到 index.jsonl
 */
export async function appendToIndex(
  snapshotName: string,
  capture: ScreenCapture,
): Promise<void> {
  await fs.mkdir(RECORDINGS_DIR, { recursive: true });

  const safeName = snapshotName.replace(/[^a-zA-Z0-9_-]/g, "_");
  const ext = recorderConfig.ansi ? ".ansi" : ".txt";

  const record: SnapshotRecord = {
    test: _currentTestName,
    step: getNextStep(_currentTestName),
    name: snapshotName,
    timestamp: new Date().toISOString(),
    snapshotFile: `${safeName}${ext}`,
    size: capture.size,
    textPreview: capture.text.slice(0, 200),
  };

  const line = JSON.stringify(record) + "\n";
  await fs.appendFile(INDEX_FILE, line, "utf-8");
}

/**
 * 更新 index.jsonl 中某条记录的 judge 结果
 *（通过 test + step 定位）
 */
export async function updateJudgeResult(
  testName: string,
  step: number,
  verdict: "pass" | "fail",
  checks: JudgeCheckRecord[],
  durationMs: number,
): Promise<void> {
  const content = await fs.readFile(INDEX_FILE, "utf-8").catch(() => "");
  const lines = content.split("\n").filter(Boolean);

  const updated = lines.map((line) => {
    const record: SnapshotRecord = JSON.parse(line);
    if (record.test === testName && record.step === step) {
      record.verdict = verdict;
      record.checks = checks;
      record.duration_ms = durationMs;
    }
    return JSON.stringify(record);
  });

  await fs.writeFile(INDEX_FILE, updated.join("\n") + "\n", "utf-8");
}

/**
 * 读取全部录制记录
 */
export async function loadAllRecords(): Promise<SnapshotRecord[]> {
  const content = await fs.readFile(INDEX_FILE, "utf-8").catch(() => "");
  return content
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line) as SnapshotRecord);
}
