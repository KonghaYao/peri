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
const RECORDINGS_DIR = path.resolve(__dirname, "..", "recordings");
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
  /** ANSI 文件相对路径（相对于 recordings/） */
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
 * 将 ANSI 原始内容写入文件，返回文件名
 */
export async function writeAnsiSnapshot(
  name: string,
  capture: ScreenCapture,
): Promise<string> {
  await fs.mkdir(RECORDINGS_DIR, { recursive: true });
  const safeName = name.replace(/[^a-zA-Z0-9_-]/g, "_");
  const fileName = `${safeName}.ansi`;
  const filePath = path.join(RECORDINGS_DIR, fileName);
  await fs.writeFile(filePath, capture.raw, "utf-8");
  return fileName;
}

/**
 * 追加一条记录到 index.jsonl
 */
export async function appendToIndex(
  snapshotName: string,
  capture: ScreenCapture,
): Promise<void> {
  await fs.mkdir(RECORDINGS_DIR, { recursive: true });

  const record: SnapshotRecord = {
    test: _currentTestName,
    step: getNextStep(_currentTestName),
    name: snapshotName,
    timestamp: new Date().toISOString(),
    snapshotFile: `${snapshotName.replace(/[^a-zA-Z0-9_-]/g, "_")}.ansi`,
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
