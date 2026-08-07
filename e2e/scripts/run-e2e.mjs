#!/usr/bin/env node
/**
 * Peri TUI E2E 测试控制面
 *
 * 职责：
 * - 扫描并选择测试用例（CLI 过滤参数 / 交互式多选）
 * - 并行执行：把选中用例分片，每个 worker 一个独立的 vitest 进程
 *   （独立录制目录 E2E_RECORDINGS_DIR，避免并发写 index.jsonl 竞争；
 *    worker 间互不清理对方 tmux session，残留由本脚本启动前统一清理）
 * - 失败重试：失败文件自动重跑（--retry N）
 * - 收集测试数据：vitest JSON 结果 + 录制索引（judge checks 明细）
 * - 输出：终端汇总 + Markdown 报告 + summary.json，反映具体失败点
 *
 * 用法（在 e2e/ 目录下）：
 *   node scripts/run-e2e.mjs                          # 交互式选择
 *   node scripts/run-e2e.mjs --all                    # 全部用例
 *   node scripts/run-e2e.mjs --only rewind            # 文件名子串过滤
 *   node scripts/run-e2e.mjs --only 'scenarios/*.test.ts'
 *   node scripts/run-e2e.mjs --dir tool-cards         # 目录过滤
 *   node scripts/run-e2e.mjs --file tests/smoke/basic-question.test.ts
 *   node scripts/run-e2e.mjs --parallel 3 --retry 1   # 并发 3，失败重试 1 次
 *   node scripts/run-e2e.mjs --all --no-interactive --verbose
 */
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const E2E_ROOT = path.resolve(__dirname, "..");
const TESTS_DIR = path.join(E2E_ROOT, "tests");
const RESULTS_DIR = path.join(E2E_ROOT, "results");
// vitest 的 ESM CLI 入口（package.json bin 指向 ./vitest.mjs），用 node 直接加载
const VITEST_ENTRY = path.join(E2E_ROOT, "node_modules", "vitest", "vitest.mjs");

// 单个测试文件最长耗时（与 vitest.config.ts testTimeout 一致 + 启动缓冲）
const PER_FILE_TIMEOUT_MS = 360_000;
const MIN_WORKER_TIMEOUT_MS = 600_000;

// ======================== 参数解析 ========================

const USAGE = `用法:
  node scripts/run-e2e.mjs [选项]

选项:
  --all                  运行全部用例
  --only <glob|子串>     按文件路径过滤（glob 或子串，可多次）
  --dir <目录>           按 tests/ 下目录过滤（如 tool-cards，可多次）
  --file <路径>          精确指定测试文件（可多次）
  --parallel <N>         并发 worker 数（默认 3；0 = CPU 核数）
  --retry <N>            失败文件重试次数（默认 1；0 = 不重试）
  --no-interactive       无过滤参数时不进入交互选择，直接跑全部
  --verbose              流式打印各 worker 的 vitest 输出
  --output <路径>        Markdown 报告输出路径（默认 results/run-<ts>/report.md）
  -h, --help             显示帮助`;

function parseArgs(argv) {
  const opts = {
    filters: [], // --only
    dirs: [], // --dir
    files: [], // --file
    parallel: 3,
    retry: 1,
    interactive: true,
    verbose: false,
    output: null,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = () => argv[++i];
    switch (arg) {
      case "--all":
        opts.interactive = false;
        break;
      case "--only":
        opts.filters.push(next());
        break;
      case "--dir":
        opts.dirs.push(next());
        break;
      case "--file":
        opts.files.push(next());
        break;
      case "--parallel": {
        const n = Number(next());
        opts.parallel = Number.isFinite(n) && n >= 0 ? n : 3;
        break;
      }
      case "--retry": {
        const n = Number(next());
        opts.retry = Number.isFinite(n) && n >= 0 ? n : 1;
        break;
      }
      case "--no-interactive":
        opts.interactive = false;
        break;
      case "--verbose":
        opts.verbose = true;
        break;
      case "--output":
        opts.output = next();
        break;
      case "-h":
      case "--help":
        console.log(USAGE);
        process.exit(0);
        break;
      default:
        console.error(`未知参数: ${arg}\n\n${USAGE}`);
        process.exit(2);
    }
  }

  if (opts.parallel === 0) {
    opts.parallel = Math.max(1, os.cpus().length);
  }
  return opts;
}

// ======================== 用例扫描与过滤 ========================

async function scanTestFiles() {
  const out = [];
  async function walk(dir) {
    const entries = await fs.readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
      } else if (entry.name.endsWith(".test.ts")) {
        out.push(path.relative(E2E_ROOT, full));
      }
    }
  }
  await walk(TESTS_DIR);
  return out.sort();
}

/** glob（仅支持 * 通配）或子串匹配文件相对路径 */
function matchesFilter(relPath, filter) {
  const normalized = relPath.replace(/\\/g, "/");
  if (/[*?[\]{}]/.test(filter)) {
    const escaped = filter
      .replace(/[.+^${}()|\\]/g, "\\$&")
      .replace(/\*/g, ".*")
      .replace(/\?/g, ".");
    return new RegExp(`^${escaped}$`).test(normalized);
  }
  return normalized.includes(filter);
}

function selectFiles(allFiles, opts) {
  // --file 支持任意路径（不限于 tests/ 扫描结果），直接采用并校验存在性
  if (opts.files.length > 0) {
    const files = opts.files.map((f) => {
      const rel = f.replace(/^\.\//, "").replace(/\\/g, "/");
      if (!existsSync(path.join(E2E_ROOT, rel))) {
        console.error(`文件不存在: ${rel}`);
        process.exit(1);
      }
      return rel;
    });
    return [...new Set(files)].sort();
  }

  let selected = allFiles;

  if (opts.filters.length > 0) {
    selected = selected.filter((f) =>
      opts.filters.some((filter) => matchesFilter(f, filter)),
    );
  }
  if (opts.dirs.length > 0) {
    selected = selected.filter((f) =>
      opts.dirs.some((d) => f.startsWith(`tests/${d.replace(/^\/+|\/+$/g, "")}/`)),
    );
  }
  return selected;
}

// ======================== 交互式选择 ========================

/**
 * 交互多选（单轮循环）：
 * - 输入过滤子串回车 → 缩小列表
 * - 输入 "1,3-5" → 切换对应编号项的选中状态
 * - a=全选当前列表，n=清空当前列表，q=退出
 * - 空输入（直接回车）= 执行当前选中
 * 初始状态：全部选中。
 */
async function interactiveSelect(allFiles) {
  const selected = new Set(allFiles);
  let visible = allFiles;
  let filter = "";

  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
    terminal: true,
  });

  for (;;) {
    const checked = visible.filter((f) => selected.has(f)).length;
    console.clear();
    console.log(
      "Peri E2E 用例选择（输入过滤子串回车过滤；'1,3-5' 切换选中；a=全选；n=清空；q=退出；空=执行）\n",
    );
    if (filter) {
      console.log(`过滤: "${filter}"  匹配 ${visible.length}/${allFiles.length}\n`);
    }
    visible.slice(0, 40).forEach((f, i) => {
      const mark = selected.has(f) ? "●" : "○";
      console.log(`  ${String(i + 1).padStart(2)} ${mark} ${f}`);
    });
    if (visible.length > 40) console.log(`  … 其余 ${visible.length - 40} 项未显示`);
    console.log(`\n当前选中: ${checked}/${visible.length} 项`);

    const input = (await new Promise((resolve) => rl.question("> ", resolve))).trim();

    if (input === "") {
      rl.close();
      return { files: [...selected].sort(), cancelled: false };
    }
    if (input === "q") {
      rl.close();
      return { files: [], cancelled: true };
    }
    if (input === "a") {
      for (const f of visible) selected.add(f);
      continue;
    }
    if (input === "n") {
      for (const f of visible) selected.delete(f);
      continue;
    }
    if (/^[\d,\s-]+$/.test(input)) {
      // 区间选择：1,3-5 → 切换对应编号项的选中状态
      const toggle = new Set();
      for (const part of input.split(",")) {
        const m = part.trim().match(/^(\d+)(?:-(\d+))?$/);
        if (!m) continue;
        const start = Number(m[1]);
        const end = m[2] ? Number(m[2]) : start;
        for (let i = start; i <= end; i++) toggle.add(i);
      }
      toggle.forEach((i) => {
        const f = visible[i - 1];
        if (f) {
          if (selected.has(f)) selected.delete(f);
          else selected.add(f);
        }
      });
      continue;
    }
    // 子串过滤
    filter = input;
    visible = allFiles.filter((f) => f.includes(filter));
  }
}

// ======================== 执行（多进程并行） ========================

/** 清理残留 tui-test-* tmux session（与 tests/setup.ts 同规则，命令带超时） */
async function cleanupStaleSessions() {
  const { exec } = await import("node:child_process");
  const { promisify } = await import("node:util");
  const execAsync = promisify(exec);
  try {
    const { stdout } = await execAsync(
      "tmux list-sessions -F '#{session_name}' 2>/dev/null || echo ''",
      { timeout: 15_000 },
    );
    const stale = stdout
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s.startsWith("tui-test-"));
    for (const s of stale) {
      await execAsync(`tmux kill-session -t ${s} 2>/dev/null`, {
        timeout: 15_000,
      }).catch(() => {});
    }
    if (stale.length > 0) {
      console.log(`✓ 已清理 ${stale.length} 个残留 tmux session`);
    }
  } catch {
    // tmux 不存在等场景静默跳过
  }
}

function runWorker({ files, workerId, runDir, verbose }) {
  const recordingsDir = path.join(runDir, "recordings", `worker-${workerId}`);
  const jsonOutput = path.join(runDir, `worker-${workerId}.json`);
  const timeoutMs = Math.max(
    MIN_WORKER_TIMEOUT_MS,
    files.length * PER_FILE_TIMEOUT_MS,
  );

  const args = [
    VITEST_ENTRY,
    "run",
    ...files.map((f) => path.relative(E2E_ROOT, f)),
    "--reporter=json",
    `--outputFile=${jsonOutput}`,
    "--silent",
  ];

  return new Promise((resolve) => {
    console.log(
      `[worker-${workerId}] 启动 (${files.length} 个文件, timeout ${Math.round(timeoutMs / 1000)}s)`,
    );
    const child = spawn(process.execPath, args, {
      cwd: E2E_ROOT,
      env: {
        ...process.env,
        E2E_RECORDINGS_DIR: recordingsDir,
        E2E_PARALLEL: "1",
      },
      stdio: verbose ? ["ignore", "inherit", "inherit"] : ["ignore", "ignore", "inherit"],
    });

    const timer = setTimeout(() => {
      console.error(`[worker-${workerId}] 超时 (${timeoutMs}ms)，发送 SIGTERM`);
      child.kill("SIGTERM");
      // SIGTERM 后 5s 仍未退出再 SIGKILL。
      // 优先 SIGTERM：vitest 退出时会清理 tmux session；
      // 直接 SIGKILL 会残留挂起的 dev.sh/cargo（下次启动失败的隐患）。
      setTimeout(() => {
        if (child.exitCode === null) {
          console.error(`[worker-${workerId}] SIGTERM 后仍未退出，SIGKILL`);
          child.kill("SIGKILL");
        }
      }, 5000).unref();
    }, timeoutMs);

    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ workerId, files, jsonOutput, recordingsDir, code });
    });
    child.on("error", (err) => {
      clearTimeout(timer);
      console.error(`[worker-${workerId}] 启动失败: ${err.message}`);
      resolve({ workerId, files, jsonOutput, recordingsDir, code: -1 });
    });
  });
}

/** 把文件列表 round-robin 分片给 N 个 worker */
function shardFiles(files, n) {
  const shards = Array.from({ length: n }, () => []);
  files.forEach((f, i) => shards[i % n].push(f));
  return shards.filter((s) => s.length > 0);
}

// ======================== 结果解析 ========================

function parseVitestJson(filePath) {
  try {
    return JSON.parse(readFileSync(filePath, "utf-8"));
  } catch {
    return null;
  }
}

/** 从 failureMessages 中提取首个 "at <file>:<line>:<col>" 位置 */
function extractFailureLocation(message) {
  const m = message.match(/at .*?([^/\s]+\.test\.ts):(\d+):(\d+)/);
  return m ? `${m[1]}:${m[2]}:${m[3]}` : null;
}

/** 把 vitest json 展开为"文件 → 测试结果"映射 */
function collectSuiteResults(vitestJson) {
  const suites = new Map();
  if (!vitestJson?.testResults) return suites;

  for (const suite of vitestJson.testResults) {
    const rel = path.relative(E2E_ROOT, suite.name).replace(/\\/g, "/");
    const failed = (suite.assertionResults ?? []).filter(
      (t) => t.status === "failed",
    );
    let results = (suite.assertionResults ?? []).map((t) => ({
      fullName: t.fullName,
      title: t.title,
      status: t.status,
      durationMs: Math.round(t.duration ?? 0),
      failureMessage: t.failureMessages?.[0] ?? null,
      failureLocation: t.failureMessages?.[0]
        ? extractFailureLocation(t.failureMessages[0])
        : null,
    }));
    // Failed Suites（导入/收集错误、beforeAll 抛错）在 vitest json 中
    // 没有 assertionResults，错误在 suite.message 里。补一条假失败记录，
    // 否则"失败点详情"会为空。
    if (suite.status === "failed" && results.length === 0) {
      const message = suite.message ?? "测试套件加载/初始化失败";
      results = [
        {
          fullName: `${rel}（套件加载失败）`,
          title: "suite failed",
          status: "failed",
          durationMs: 0,
          failureMessage: message.trim().slice(0, 2000) || "套件失败",
          failureLocation: extractFailureLocation(message) ?? null,
        },
      ];
    }
    // suite 级时长（endTime - startTime）；为 0 时退化为各测试耗时之和
    let durationMs = Math.round((suite.endTime ?? 0) - (suite.startTime ?? 0));
    if (durationMs <= 0) {
      durationMs = results.reduce((acc, t) => acc + t.durationMs, 0);
    }
    suites.set(rel, {
      file: rel,
      status: suite.status,
      durationMs,
      tests: results,
      failedTests: failed.map((t) => t.fullName),
    });
  }
  return suites;
}

/** 读取录制索引，提取 judge 明细（test → checks 列表） */
function loadJudgeRecords(recordingsDir) {
  const indexFile = path.join(recordingsDir, "index.jsonl");
  try {
    const content = readFileSync(indexFile, "utf-8");
    return content
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        try {
          return JSON.parse(line);
        } catch {
          return null;
        }
      })
      .filter(Boolean);
  } catch {
    return [];
  }
}

/** 把录制记录按 vitest fullName 关联（去掉文件路径前缀后 join 匹配） */
function associateJudgeChecks(records, fullName) {
  const hits = records.filter((r) => {
    const raw = String(r.test ?? "");
    const parts = raw.split(" > ");
    // 无 " > " 分隔符时说明 task.name 只有 it 标题（无 describe 前缀），直接用原文
    const joined = parts.length > 1 ? parts.slice(1).join(" ") : raw;
    if (joined === fullName) return true;
    // 后缀匹配（task.name 可能缺少 describe 前缀）；要求足够长避免误匹配
    if (joined.length >= 8 && fullName.endsWith(joined)) return true;
    if (fullName.length >= 8 && joined.endsWith(fullName)) return true;
    return false;
  });
  return hits.flatMap((r) =>
    (r.checks ?? []).map((c) => ({
      criterion: c.criterion,
      pass: c.pass,
      detail: c.detail ?? "",
      snapshotFile: r.snapshotFile ?? null,
    })),
  );
}

// ======================== 报告 ========================

function formatDuration(ms) {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`;
}

function summarize(allSuites, runDir, attempts) {
  const files = [...allSuites.keys()].sort();
  let passed = 0;
  let failed = 0;
  const failures = [];
  let totalMs = 0;

  const fileSummaries = files.map((file) => {
    const s = allSuites.get(file);
    totalMs += s.durationMs;
    if (s.status === "failed" || s.failedTests.length > 0) {
      failed++;
      failures.push(s);
      return { file, status: "failed", durationMs: s.durationMs };
    }
    passed++;
    return { file, status: "passed", durationMs: s.durationMs };
  });

  return {
    runDir,
    attempts,
    total: files.length,
    passed,
    failed,
    skipped: 0,
    totalMs,
    files: fileSummaries,
    failures,
  };
}

function printSummary(summary, opts) {
  const { total, passed, failed, totalMs } = summary;
  console.log("\n" + "=".repeat(64));
  console.log("Peri E2E 结果汇总");
  console.log("=".repeat(64));
  console.log(`用例: ${total} 个文件 | ✅ ${passed} 通过 | ❌ ${failed} 失败 | 总耗时 ${formatDuration(totalMs)}（并发 ${opts.parallel}，重试 ${opts.retry}）`);

  if (failed === 0) {
    console.log("\n全部通过 🎉");
    return;
  }

  console.log("\n失败文件：");
  for (const s of summary.failures) {
    console.log(`  ❌ ${s.file} (${formatDuration(s.durationMs)})`);
  }

  console.log("\n失败点详情：");
  for (const s of summary.failures) {
    console.log(`\n❌ ${s.file} (${formatDuration(s.durationMs)})`);
    for (const t of s.tests.filter((x) => x.status === "failed")) {
      console.log(`  测试: ${t.fullName}`);
      if (t.failureLocation) console.log(`  位置: ${t.failureLocation}`);
      if (t.failureMessage) {
        const firstLine = t.failureMessage.split("\n")[0];
        console.log(`  断言: ${firstLine.slice(0, 240)}`);
      }
      const judgeChecks = t.judgeChecks?.filter((c) => !c.pass) ?? [];
      if (judgeChecks.length > 0) {
        console.log("  Judge 失败项:");
        for (const c of judgeChecks) {
          console.log(`    - ${c.criterion}`);
          console.log(`      ${c.detail}`);
          if (c.snapshotFile) console.log(`      快照: ${c.snapshotFile}`);
        }
      }
    }
  }
}

function buildMarkdown(summary, attempts, opts, startedAt) {
  const lines = [];
  lines.push(`# Peri E2E 测试结果（${startedAt}）`);
  lines.push("");
  lines.push(`- 运行方式：并行执行（${opts.parallel} worker），失败重试 ${opts.retry} 次`);
  lines.push(`- 用例数：${summary.total} 个文件（${summary.passed} 通过 / ${summary.failed} 失败）`);
  lines.push(`- 总耗时：${formatDuration(summary.totalMs)}`);
  lines.push("");
  lines.push("## 结果汇总");
  lines.push("");
  lines.push("| 文件 | 结果 | 耗时 |");
  lines.push("| --- | --- | --- |");
  for (const f of summary.files) {
    const mark = f.status === "failed" ? "❌" : "✅";
    lines.push(`| ${f.file} | ${mark} | ${formatDuration(f.durationMs)} |`);
  }

  if (summary.failures.length > 0) {
    lines.push("");
    lines.push("## 失败点详情");
    for (const s of summary.failures) {
      lines.push("");
      lines.push(`### ❌ ${s.file}（${formatDuration(s.durationMs)}）`);
      for (const t of s.tests.filter((x) => x.status === "failed")) {
        lines.push("");
        lines.push(`**${t.fullName}**`);
        if (t.failureLocation) lines.push(`- 位置：${t.failureLocation}`);
        if (t.failureMessage) {
          const msg = t.failureMessage.split("\n")[0];
          lines.push(`- 断言：\`${msg}\``);
        }
        const judgeChecks = t.judgeChecks?.filter((c) => !c.pass) ?? [];
        if (judgeChecks.length > 0) {
          lines.push("- Judge 失败项：");
          for (const c of judgeChecks) {
            lines.push(`  - ${c.criterion}`);
            lines.push(`    - ${c.detail}`);
            if (c.snapshotFile) lines.push(`    - 快照：\`${c.snapshotFile}\``);
          }
        }
      }
    }
  }
  lines.push("");
  return lines.join("\n");
}

// ======================== 主流程 ========================

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const startedAt = new Date().toISOString();
  const allFiles = await scanTestFiles();

  if (allFiles.length === 0) {
    console.error("未发现测试文件（tests/**/*.test.ts）");
    process.exit(1);
  }

  // ---- 用例选择 ----
  let files;
  const hasSelectionArgs =
    opts.files.length > 0 || opts.filters.length > 0 || opts.dirs.length > 0;

  if (hasSelectionArgs) {
    files = selectFiles(allFiles, opts);
  } else if (opts.interactive && process.stdin.isTTY) {
    const result = await interactiveSelect(allFiles);
    if (result.cancelled) {
      console.log("已取消");
      process.exit(0);
    }
    files = result.files;
  } else {
    files = [...allFiles];
  }

  if (files.length === 0) {
    console.error("没有选中的用例");
    process.exit(1);
  }

  // ---- 运行目录 ----
  const runId = `run-${new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19)}`;
  const runDir = path.join(RESULTS_DIR, runId);
  await fs.mkdir(runDir, { recursive: true });

  console.log(`\nPeri E2E 控制面`);
  console.log(`运行目录: ${runDir}`);
  console.log(`选中 ${files.length} 个文件, 并发 ${opts.parallel}, 重试 ${opts.retry}\n`);

  await cleanupStaleSessions();

  // ---- 执行（含重试） ----
  const allSuites = new Map();
  let attempts = 0;
  let pending = files;

  while (pending.length > 0) {
    attempts++;
    const shards = shardFiles(pending, Math.min(opts.parallel, pending.length));
    console.log(`\n── 第 ${attempts} 轮执行 (${pending.length} 个文件, ${shards.length} 个 worker) ──\n`);

    const workerResults = await Promise.all(
      shards.map((shard, i) =>
        runWorker({ files: shard, workerId: i, runDir, verbose: opts.verbose }),
      ),
    );

    // 收集本轮结果
    for (const wr of workerResults) {
      const json = parseVitestJson(wr.jsonOutput);
      const suites = json ? collectSuiteResults(json) : new Map();
      const records = loadJudgeRecords(wr.recordingsDir);

      for (const file of wr.files) {
        const suite = suites.get(file.replace(/\\/g, "/"));
        if (!suite) {
          allSuites.set(file, {
            file,
            status: "failed",
            durationMs: 0,
            tests: [
              {
                fullName: file,
                title: file,
                status: "failed",
                durationMs: 0,
                failureMessage:
                  "vitest 未产出该文件的 JSON 结果（进程崩溃/超时/未找到文件）",
                failureLocation: null,
              },
            ],
            failedTests: [file],
          });
          continue;
        }
        // 关联 judge 明细
        for (const t of suite.tests) {
          t.judgeChecks = associateJudgeChecks(records, t.fullName);
        }
        allSuites.set(file, suite);
      }
    }

    // 找出需要重试的失败文件
    const failedFiles = [...allSuites.entries()]
      .filter(([file]) => allSuites.get(file).status === "failed")
      .map(([file]) => file);

    if (failedFiles.length === 0 || attempts > opts.retry) {
      break;
    }
    pending = failedFiles;
    console.log(`\n${failedFiles.length} 个文件失败，重试第 ${attempts} 轮…`);
  }

  // ---- 汇总与报告 ----
  const summary = summarize(allSuites, runDir, attempts);
  printSummary(summary, opts);

  const reportPath =
    opts.output ?? path.join(runDir, "report.md");
  await fs.mkdir(path.dirname(reportPath), { recursive: true });
  await fs.writeFile(
    reportPath,
    buildMarkdown(summary, attempts, opts, startedAt),
    "utf-8",
  );
  await fs.writeFile(
    path.join(runDir, "summary.json"),
    JSON.stringify(
      {
        runId,
        startedAt,
        attempts,
        opts: { parallel: opts.parallel, retry: opts.retry },
        ...summary,
        failures: summary.failures.map((s) => ({
          file: s.file,
          durationMs: s.durationMs,
          failedTests: s.failedTests,
          details: s.tests
            .filter((t) => t.status === "failed")
            .map((t) => ({
              fullName: t.fullName,
              location: t.failureLocation,
              message: t.failureMessage?.split("\n")[0] ?? null,
              judgeChecks: (t.judgeChecks ?? [])
                .filter((c) => !c.pass)
                .map((c) => ({ criterion: c.criterion, detail: c.detail })),
            })),
        })),
      },
      null,
      2,
    ),
    "utf-8",
  );

  console.log(`\n📄 Markdown 报告: ${reportPath}`);
  console.log(`📄 数据摘要: ${path.join(runDir, "summary.json")}`);

  process.exit(summary.failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error("控制面执行失败:", err);
  process.exit(1);
});
