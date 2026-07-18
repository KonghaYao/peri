/**
 * Peri TUI E2E 测试辅助
 *
 * - 封装 peri 的启动/输入/snapshot 流程
 * - 动态解析项目根目录，适配不同开发者环境
 */
import path from "node:path";
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
 * 启动 peri TUI（通过 dev.sh）
 *
 * dev.sh 会 source .env 并运行 cargo run -p peri-tui
 */
export async function launchPeri(
  options: PeriLaunchOptions = {},
): Promise<TmuxTester> {
  const size = options.size ?? DEFAULT_SIZE;

  const tester = new TmuxTester({
    command: [DEV_SH],
    size,
    cwd: PROJECT_ROOT,
    env: options.env ?? {},
    debug: options.debug ?? false,
    snapshotDir: path.join(PROJECT_ROOT, "e2e", "recordings"),
  });

  await tester.start();
  // peri 是 Rust 编译 + TUI 初始化，需要较长的启动等待
  // 等待欢迎屏幕出现（搜索 peri 特有的文本）
  await tester.sleep(5000);
  try {
    await tester.waitForText("AI operating system", {
      timeout: 30_000,
      interval: 1000,
    });
  } catch {
    // 如果没等到特定文本，至少等待足够时间
    await tester.sleep(5000);
  }

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
 * 等待屏幕内容稳定（连续 4 次轮询无变化）
 * 用于确保 LLM 流式输出或工具调用完成后屏幕不再更新
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

  // 阶段 2：等待屏幕不再变化
  let lastLen = 0;
  let stableCount = 0;

  await tester.waitFor(
    (screen: string) => {
      const len = screen.length;
      if (len > 50 && len === lastLen) {
        stableCount++;
      } else {
        stableCount = 0;
      }
      lastLen = len;
      return stableCount >= 4;
    },
    { timeout, interval: 1500, message: "屏幕未能在超时时间内稳定" },
  );
}
