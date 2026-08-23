/**
 * Peri TUI E2E 测试辅助
 *
 * - 封装 peri 的启动/输入/snapshot 流程
 * - 动态解析项目根目录，适配不同开发者环境
 */
import path from "node:path";
import fs from "node:fs";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { TmuxTester, createTester } from "tui-tester";
import type { ScreenCapture, TerminalSize } from "tui-tester";

// ---- 路径解析 ----

// e2e/helpers/peri.ts → e2e/ → 项目根目录
const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const PROJECT_ROOT = path.resolve(__dirname, "..", "..");
export const DEV_SH = path.join(PROJECT_ROOT, "dev.sh");

// ---- 默认配置 ----

const DEFAULT_SIZE: TerminalSize = { cols: 120, rows: 40 };

export interface PeriLaunchOptions {
  /** 终端尺寸 */
  size?: TerminalSize;
  /** 传递给 tmux session 的环境变量 */
  env?: Record<string, string>;
  /** 调试模式 */
  debug?: boolean;
}

/**
 * 创建并启动 tmux session（带重试）。
 *
 * tmux server 在最后一个 session 被销毁时会自动退出（exit-empty），
 * 下一个 new-session 命令会重建 server——这个重建窗口偶发失败，
 * 重试 2 次可消除大部分竞态。
 */
async function startTester(tester: TmuxTester, label: string): Promise<void> {
  const MAX_START_ATTEMPTS = 3;
  for (let attempt = 1; attempt <= MAX_START_ATTEMPTS; attempt++) {
    try {
      await tester.start();
      return;
    } catch (err) {
      if (attempt === MAX_START_ATTEMPTS) {
        throw err;
      }
      console.warn(
        `[${label}] tmux session 创建失败（第 ${attempt} 次），重试…`,
      );
      await new Promise((resolve) => setTimeout(resolve, 1500 * attempt));
    }
  }
}

/**
 * 检测屏幕是否出现"dev.sh 已退出"的特征：
 * - 纯 shell 提示符行（行首提示符且行尾无内容）——dev.sh 退出后交互 bash 返回提示符。
 *   注意：不能匹配命令回显行（如 "bash-3.2$ /path/dev.sh" 是 bash 正在执行命令的
 *   正常画面，dev.sh 可能正在编译启动）。
 * - cargo 编译错误输出。
 * TUI 欢迎屏/正常界面不含这些特征。
 */
function looksLikeLaunchFailure(screen: string): boolean {
  return (
    // 行首 shell 提示符，行尾无命令内容（纯提示符行）
    /(?:^|\n)\s*[a-zA-Z0-9_./-]*[\$%#>]\s*$/m.test(screen) ||
    // cargo 编译错误（"error: ..." / "error[...]"）
    /(?:^|\n)\s*error(\[|:)/m.test(screen)
  );
}

/** 检查 tmux session 是否还存活（直接调 tmux，绕开 tester 内部状态） */
async function isSessionAlive(sessionName: string): Promise<boolean> {
  try {
    const { exec } = await import("node:child_process");
    const { promisify } = await import("node:util");
    const execAsync = promisify(exec);
    await execAsync(`tmux has-session -t ${sessionName} 2>/dev/null`, {
      timeout: 10_000,
    });
    return true;
  } catch {
    return false;
  }
}

/**
 * 等待 peri TUI 就绪（欢迎文本出现），并在失败时给出明确诊断。
 *
 * 关键机制（tmux 默认行为）：
 * - pane 进程退出 → window 关闭 → session 自动销毁（remain-on-exit off）
 * - 最后一个 session 销毁 → tmux server 退出（exit-empty on）
 *
 * 但测试中的 pane 是交互式 bash：dev.sh 失败退出后 bash 返回提示符，
 * session 存活但欢迎文本永不出现——此时旧实现会静默继续，
 * 后续断言/judge 拍到错误屏幕导致"看起来像 tmux 挂了"。
 *
 * 本函数轮询屏幕并检测三类失败：
 * 1. session 消失（dev.sh 启动失败导致 server 退出）→ 抛"启动失败"
 * 2. 屏幕出现 shell 提示符 / cargo error（dev.sh 已退出）→ 抛"启动失败"
 * 3. 90s 后仍无欢迎文本（慢编译/卡初始化）→ 抛"启动超时"
 */
async function waitForPeriReady(
  tester: TmuxTester,
  label: string,
): Promise<void> {
  const sessionName = tester.getSessionName();

  await tester.sleep(5000);
  const start = Date.now();

  // 阶段 1：欢迎文本 / 快速失败检测（30s）
  while (Date.now() - start < 30_000) {
    let screen: string;
    try {
      screen = await tester.getScreenText();
    } catch {
      // capture 失败：session 可能已销毁
      if (!(await isSessionAlive(sessionName))) {
        throw new Error(
          `[${label}] peri 启动失败：tmux session "${sessionName}" 已退出。` +
            `通常是 dev.sh 启动失败（cargo 编译失败/环境问题），导致 pane 进程退出、` +
            `session 自动销毁、tmux server 退出（exit-empty）。` +
            `后续操作报 "can't find pane"/"no server running" 均属同一根因。`,
        );
      }
      continue;
    }
    if (screen.includes("AI operating system")) {
      return;
    }
    if (looksLikeLaunchFailure(screen)) {
      throw new Error(
        `[${label}] peri 启动失败：dev.sh 已退出（屏幕出现 shell 提示符或编译错误）。` +
          `屏幕片段: ${screen.slice(0, 300).replace(/\n/g, "⏎")}`,
      );
    }
    await tester.sleep(1000);
  }

  // 阶段 2：session 存活但无欢迎文本——慢启动（cargo 编译）再等 60s
  console.warn(
    `[${label}] 30s 内未出现欢迎文本（session 存活），继续等待慢启动…`,
  );
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    let screen: string;
    try {
      screen = await tester.getScreenText();
    } catch {
      if (!(await isSessionAlive(sessionName))) {
        throw new Error(
          `[${label}] peri 启动失败：tmux session "${sessionName}" 已退出（等待期间）。`,
        );
      }
      continue;
    }
    if (screen.includes("AI operating system")) {
      return;
    }
    if (looksLikeLaunchFailure(screen)) {
      throw new Error(
        `[${label}] peri 启动失败：dev.sh 已退出（屏幕出现 shell 提示符或编译错误）。` +
          `屏幕片段: ${screen.slice(0, 300).replace(/\n/g, "⏎")}`,
      );
    }
    await tester.sleep(2000);
  }

  const screen = await tester.getScreenText().catch(() => "");
  throw new Error(
    `[${label}] peri 启动超时（约 95s）：session 存活但欢迎文本未出现，` +
      `可能卡在 cargo 编译或 TUI 初始化。当前屏幕片段: ${screen
        .slice(0, 300)
        .replace(/\n/g, "⏎")}`,
  );
}

/**
 * 启动 peri TUI（通过 dev.sh）
 *
 * dev.sh 会 source .env 并运行 cargo run -p peri-tui
 *
 * HOME 隔离：默认注入临时 HOME（含空 .peri/settings.json），防止 e2e 会话
 * 读取/污染用户真实 ~/.peri/settings.json（TUI 启动即可能触发配置保存，如
 * daily color 落盘；此前仅 model-switch 测试隔离 HOME，其余 e2e 直接读
 * 真实配置——真实配置曾因此被透传写入高危 meta_harness 全关字段导致功能
 * 全部消失）。显式传入 env.HOME 的调用方（如 model-switch 预置测试配置）
 * 保持原语义，不被覆盖。
 */
export async function launchPeri(
  options: PeriLaunchOptions = {},
): Promise<TmuxTester> {
  const size = options.size ?? DEFAULT_SIZE;

  const env: Record<string, string> = { ...(options.env ?? {}) };
  let isoHome: string | undefined;
  if (!env.HOME) {
    isoHome = fs.mkdtempSync(path.join(os.tmpdir(), "peri-e2e-home-"));
    const periDir = path.join(isoHome, ".peri");
    fs.mkdirSync(periDir, { recursive: true });
    fs.writeFileSync(path.join(periDir, "settings.json"), "{}");
    // 用户 shell rc 可能无条件 source "$HOME/.cargo/env"。隔离 HOME 中提供空的
    // 兼容文件，避免交互 shell 在 dev.sh 启动前退出；不复制用户环境或凭据。
    const cargoDir = path.join(isoHome, ".cargo");
    fs.mkdirSync(cargoDir, { recursive: true });
    fs.writeFileSync(path.join(cargoDir, "env"), "");
    env.HOME = isoHome;
    // 测试进程退出时清理临时 HOME（best-effort，不阻塞退出）
    process.on("exit", () => {
      try {
        fs.rmSync(isoHome!, { recursive: true, force: true });
      } catch {
        // ignore cleanup errors
      }
    });
  }

  const tester = new TmuxTester({
    command: [DEV_SH],
    size,
    cwd: PROJECT_ROOT,
    env,
    debug: options.debug ?? false,
    snapshotDir: path.join(PROJECT_ROOT, "e2e", "recordings"),
  });

  await startTester(tester, "launchPeri");
  await waitForPeriReady(tester, "launchPeri");

  return tester;
}

/**
 * 向 peri 发送输入（模拟用户在输入框打字并回车）
 *
 * 注意：首次发送前会自动按 Esc 确保输入框处于可输入状态
 * （关闭可能的初始弹窗/向导）
 */
export async function sendPrompt(
  tester: TmuxTester,
  text: string,
): Promise<void> {
  // 清空输入框可能残留的内容（keepgoing 按钮预填/未提交文本），
  // 避免逐字符追加导致 prompt 被污染（C-u = readline 清行）。
  await tester.sendKey("u", { ctrl: true });
  await tester.sleep(100);

  // 逐字符发送文本
  for (const char of text) {
    await tester.sendText(char);
    await tester.sleep(50);
  }
  // 回车提交
  await tester.sendKey("Enter");
  await tester.sleep(500);
}

/**
 * 等待 peri 输出中出现指定文本（轮询屏幕纯文本）
 */
export async function waitForOutput(
  tester: TmuxTester,
  text: string,
  timeoutMs: number = 120_000,
): Promise<void> {
  await tester.waitForText(text, {
    timeout: timeoutMs,
    interval: 1000,
    message: `等待文本 "${text}" 出现超时`,
  });
}

/**
 * 抓屏并保存 snapshot 到文件
 * 同时将记录写入 index.jsonl
 */
export async function takePeriSnapshot(
  tester: TmuxTester,
  name: string,
): Promise<ScreenCapture> {
  const capture = await tester.captureScreen();

  // 保存 ANSI 原始文件
  const { writeAnsiSnapshot, appendToIndex } = await import("./recorder.js");
  await writeAnsiSnapshot(name, capture);
  await appendToIndex(name, capture);

  return capture;
}

/**
 * 启动 peri TUI（HITL 审批模式，等同 -a 参数）
 *
 * 用于测试 HITL 审批弹窗交互。
 */
export async function launchPeriHITL(
  options: PeriLaunchOptions = {},
): Promise<TmuxTester> {
  const size = options.size ?? DEFAULT_SIZE;

  const tester = new TmuxTester({
    command: [DEV_SH, "-a"],
    size,
    cwd: PROJECT_ROOT,
    env: options.env ?? {},
    debug: options.debug ?? false,
    snapshotDir: path.join(PROJECT_ROOT, "e2e", "recordings"),
  });

  await startTester(tester, "launchPeriHITL");
  await waitForPeriReady(tester, "launchPeriHITL");

  return tester;
}

/**
 * 等待屏幕内容稳定（连续 3 次轮询内容完全一致）
 * 用于确保 LLM 流式输出或工具调用完成后屏幕不再更新
 *
 * 判断依据是去 ANSI 后的完整文本内容而非长度：
 * 长度相同但内容在更新（计时器、进度百分比原地变化）不会被误判为稳定；
 * 仅样式/光标变化导致长度变化也不会误判为不稳定。
 *
 * @param tester
 * @param baseScreen  对比基准（提交 prompt 前的屏幕），若传入则先等屏幕变化
 * @param timeout     超时（默认 120s）
 */
export async function waitForStableScreen(
  tester: TmuxTester,
  timeout: number = 120_000,
  baseScreen?: string,
): Promise<void> {
  // 阶段 1：等待屏幕变化（如果有基准）
  if (baseScreen !== undefined) {
    await tester.waitFor(
      (screen) => screen !== baseScreen,
      { timeout: 30_000, interval: 500, message: "屏幕未发生变化，输入可能未被处理" },
    );
  }

  // 阶段 2：等待屏幕内容连续 3 次完全相同（interval 1.5s → 约 3s 稳定）
  const STABLE_ROUNDS = 3;
  let stableCount = 0;
  let lastText: string | null = null;

  await tester.waitFor(
    (screen: string) => {
      // 空屏/异常屏（内容过短）不参与稳定计数
      if (screen.length <= 50) {
        stableCount = 0;
        lastText = null;
        return false;
      }
      if (lastText !== null && screen === lastText) {
        stableCount++;
      } else {
        stableCount = 0;
        lastText = screen;
      }
      return stableCount >= STABLE_ROUNDS;
    },
    { timeout, interval: 1500, message: "屏幕未能在超时时间内稳定" },
  );
}
