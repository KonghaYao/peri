/**
 * 场景测试: Slash 命令 + Profile 切换
 *
 * 验证 /model 斜杠命令打开模型面板（左右分栏 Profile 编辑器）：
 * - 左侧：4 个固定档位（fable / opus / sonnet / haiku），↑/↓ 选择即切换 active profile；
 * - 右侧：当前 profile 的 K/V 编辑行，→ 进入右侧焦点，←/→ 切换字段值并立即写入；
 * - Model / Effort 字段切换后值必须真的变化（确定性断言，回归保护）；
 * - Esc 退出右侧焦点后再次 Esc 关闭面板；
 * - 状态栏中的 alias/model/effort 随 active profile 更新。
 *
 * 隔离策略：以临时 HOME（含预置测试配置）启动 peri，避免读取/修改用户真实
 * ~/.peri/settings.json（此前测试会把 sonnet 档位的 model/effort 持久化成
 * pro/low，污染用户配置；且初始 active 依赖上次运行残留导致断言不稳定）。
 * 预置配置初始 active_alias=fable，测试流程（↓↓ 切到 sonnet）确定性成立。
 */
import { describe, it, expect, afterEach, beforeAll, afterAll } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

/** 测试专用临时 HOME（含预置 settings.json），afterAll 清理 */
let testHome: string | undefined;

const TEST_SETTINGS = {
  config: {
    active_alias: "fable",
    providers: [
      {
        id: "test-provider",
        type: "openai",
        apiKey: "test-key",
        baseUrl: "http://127.0.0.1:9/v1",
        name: "test",
        models: {
          fable: "test-model-fable",
          opus: "test-model-opus",
          sonnet: "test-model-sonnet",
          haiku: "test-model-haiku",
        },
      },
    ],
    profiles: {
      fable: { provider: "test-provider", effort: "max", max_tokens: 32000 },
      opus: { provider: "test-provider", effort: "high", max_tokens: 32000 },
      sonnet: { provider: "test-provider", effort: "medium", max_tokens: 32000 },
      haiku: { provider: "test-provider", effort: "low", max_tokens: 32000 },
    },
  },
};

/**
 * 从屏幕纯文本中提取右侧 K/V 行的值（按分隔线 │ 切分右侧，再按 key 定位）。
 * 用于确定性断言（不依赖 LLM judge）：Model/Effort 等字段切换后值必须真的变化。
 */
function extractRightValue(text: string, key: string): string | null {
  for (const line of text.split("\n")) {
    if (!line.includes("│")) {
      continue;
    }
    const right = line.split("│")[1] ?? "";
    // 去掉焦点标记（❯）与行首空白
    const trimmed = right.trim().replace(/^[❯ ]+\s*/, "");
    if (trimmed.startsWith(key + " ")) {
      const value = trimmed.slice(key.length).trim();
      return value.length > 0 ? value : null;
    }
  }
  return null;
}

describe("panels: model switch", () => {
  let tester: TmuxTester;

  beforeAll(() => {
    // 预置隔离配置：临时 HOME + settings.json（初始 active_alias=fable）
    testHome = fs.mkdtempSync(path.join(os.tmpdir(), "peri-e2e-model-"));
    const settingsDir = path.join(testHome, ".peri");
    fs.mkdirSync(settingsDir, { recursive: true });
    fs.writeFileSync(
      path.join(settingsDir, "settings.json"),
      JSON.stringify(TEST_SETTINGS, null, 2),
    );
  });

  afterAll(() => {
    if (testHome) {
      fs.rmSync(testHome, { recursive: true, force: true });
      testHome = undefined;
    }
  });

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/model 打开面板，切换 profile，编辑 Effort，状态栏更新",
    { timeout: 120_000 },
    async () => {
      tester = await launchPeri({ env: { HOME: testHome! } });

      // 阶段 1：通过 /model 命令打开模型面板
      await sendPrompt(tester, "/model");

      await tester.waitForText("Model", {
        timeout: 10_000,
        interval: 500,
      });

      const panelCapture = await takePeriSnapshot(tester, "model-panel-open");

      // 阶段 2：左侧 ↓ 切换 profile（选择即激活，立即写入）
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("down");
      await tester.sleep(500);

      const profileCapture = await takePeriSnapshot(tester, "model-profile-switched");

      // 阶段 3a：Tab 进入右侧编辑焦点，↓ 到 Model 行，→ 切换 Model 值（立即持久化）
      // 回归保护：Model 字段此前存在"触发写入但值不变"的 bug，必须断言值真的变了。
      await tester.sendKey("tab");
      await tester.sleep(200);
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("right");
      await tester.sleep(500);

      const modelCapture = await takePeriSnapshot(tester, "model-cycled");

      // 阶段 3b：↓ 到 Effort 行，→ 切换 Effort 值（立即持久化）
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("right");
      await tester.sleep(500);

      const editCapture = await takePeriSnapshot(tester, "model-effort-edited");

      // 阶段 3c：Tab 切回左侧（左右焦点可往返），再 Tab 回到右侧
      await tester.sendKey("tab");
      await tester.sleep(200);
      await tester.sendKey("tab");
      await tester.sleep(200);

      // 阶段 4：Esc 退出右侧焦点，再 Esc 关闭面板
      await tester.sendKey("escape");
      await tester.sleep(200);
      await tester.sendKey("escape");
      await tester.sleep(800);

      const capture = await takePeriSnapshot(tester, "model-switch-done");

      // 基本断言
      expect(panelCapture.text).toContain("Model");
      expect(capture.text.length).toBeGreaterThan(50);

      // LLM judge: 面板打开阶段——左右分栏结构
      const panelResult = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "屏幕中应有 Model 面板，左侧是 4 个固定档位列表（fable、opus、sonnet、haiku），每档含 provider 与模型名",
          "左侧应有当前激活档位的选中标记（如 ● 与 ○ 区分），右侧应有 K/V 编辑行（Provider / Model / Effort / Max tokens / 1m enable）",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(panelResult, null, 2));
      expect(panelResult.pass).toBe(true);

      // LLM judge: 切换 profile 后激活标记移动
      const profileResult = await judge({
        ansiRaw: profileCapture.raw,
        criteria: [
          "Model 面板仍打开，左侧激活标记（●）应位于与打开时不同的档位行上（切换过 active profile）",
        ],
      });
      console.log("Judge (profile):", JSON.stringify(profileResult, null, 2));
      expect(profileResult.pass).toBe(true);

      // 阶段 3a 回归断言：Model 行值必须真的变化（此前 ←/→ 触发写入但值不变）
      const modelBefore = extractRightValue(profileCapture.text, "Model");
      const modelAfter = extractRightValue(modelCapture.text, "Model");
      console.log("Model value before:", modelBefore, "after:", modelAfter);
      expect(modelBefore).not.toBeNull();
      expect(modelAfter).not.toBeNull();
      expect(modelAfter).not.toBe(modelBefore);

      // 阶段 3b 回归断言：Effort 行值必须真的变化（与 Model 同理，确定性断言不依赖 LLM judge）
      const effortBefore = extractRightValue(modelCapture.text, "Effort");
      const effortAfter = extractRightValue(editCapture.text, "Effort");
      console.log("Effort value before:", effortBefore, "after:", effortAfter);
      expect(effortBefore).not.toBeNull();
      expect(effortAfter).not.toBeNull();
      expect(effortAfter).not.toBe(effortBefore);

      // LLM judge: 关闭后状态栏验证（正向断言：状态栏反映切换后的 alias/model/effort）
      const doneResult = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "屏幕底部状态栏应显示 'alias model effort' 三段式信息（如 'opus xxx high' 或 'sonnet xxx medium' 格式，alias 为 fable/opus/sonnet/haiku 之一）",
          "状态栏的 alias 部分应与切换后的 active profile 一致，不应仍是默认值",
        ],
      });
      console.log("Judge (done):", JSON.stringify(doneResult, null, 2));
      expect(doneResult.pass).toBe(true);
    },
  );
});
