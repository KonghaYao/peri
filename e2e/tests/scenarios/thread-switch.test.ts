/**
 * 场景测试: Thread 切换 + History 面板
 *
 * 验证 /threads 斜杠命令打开历史面板，用户可选择历史线程并按 Enter 切换，
 * 切换后消息区加载该线程的内容。
 */
import { describe, it, expect, afterEach } from "vitest";
import { rm } from "node:fs/promises";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import {
  launchReplayTui,
  makeReplayHome,
  replayDebug,
  startReplayModelServer,
  type ReplayServer,
} from "../../helpers/replay-model.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: thread switch", () => {
  let tester: TmuxTester;
  let replay: ReplayServer;
  let home: string;

  function persistedMessages(): Array<{ thread_id: string; content: string }> {
    const database = new DatabaseSync(path.join(home, ".peri", "threads", "threads.db"), { readOnly: true });
    try {
      return database.prepare("SELECT thread_id, content FROM messages").all() as Array<{
        thread_id: string; content: string;
      }>;
    } finally {
      database.close();
    }
  }

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
  });

  it(
    "/threads 打开历史面板并切换线程",
    { timeout: 300_000 },
    async () => {
      replay = await startReplayModelServer([
        { prompt: "hello E2E_THREAD_ONE", text: "hello，线程一的持久化回复：THREAD_ONE_PERSISTED。" },
        { prompt: "E2E_THREAD_TWO", text: "线程二的持久化回复：THREAD_TWO_PERSISTED。" },
      ]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home });

      // 阶段 1：先发一条消息创建当前线程
      const base = await tester.getScreenText();
      await sendPrompt(tester, "hello E2E_THREAD_ONE");
      await tester.waitFor(
        (screen) => screen.includes("THREAD_ONE_PERSISTED") && /(?:Brewed for|处理耗时)/.test(screen),
        { timeout: 60_000, interval: 200, message: "等待线程一真实 turn 完成" },
      );
      await waitForStableScreen(tester, 60_000, base);
      await expect.poll(() => persistedMessages().some((message) =>
        message.content.includes("THREAD_ONE_PERSISTED"),
      ), { timeout: 10_000 }).toBe(true);
      const firstThreadId = persistedMessages().find((message) =>
        message.content.includes("THREAD_ONE_PERSISTED"),
      )!.thread_id;
      await tester.stop();
      tester = await launchReplayTui({ home });
      expect(persistedMessages().some((message) =>
        message.thread_id === firstThreadId && message.content.includes("THREAD_ONE_PERSISTED"),
      )).toBe(true);
      await sendPrompt(tester, "/clear");
      await tester.waitFor(
        (screen) => !screen.includes("THREAD_ONE_PERSISTED"),
        { timeout: 10_000, interval: 200, message: "等待切到新的空会话" },
      );
      await sendPrompt(tester, "E2E_THREAD_TWO");
      await tester.waitFor(
        (screen) => screen.includes("THREAD_TWO_PERSISTED") && /(?:Brewed for|处理耗时)/.test(screen),
        { timeout: 60_000, interval: 200, message: "等待线程二真实 turn 完成" },
      );
      await expect.poll(() => persistedMessages().some((message) =>
        message.thread_id !== firstThreadId && message.content.includes("THREAD_TWO_PERSISTED"),
      ), { timeout: 10_000 }).toBe(true);
      const secondThreadId = persistedMessages().find((message) =>
        message.content.includes("THREAD_TWO_PERSISTED"),
      )!.thread_id;
      expect(secondThreadId).not.toBe(firstThreadId);
      await tester.sleep(2500);

      // 阶段 2：通过 /threads 命令打开历史面板
      await sendPrompt(tester, "/threads");

      await tester.waitForText("Threads", {
        timeout: 10_000,
        interval: 500,
      });

      const panelCapture = await takePeriSnapshot(tester, "thread-panel-open");
      const database = new DatabaseSync(path.join(home, ".peri", "threads", "threads.db"), { readOnly: true });
      try {
        const threads = database.prepare(
          "SELECT id FROM threads WHERE parent_thread_id IS NULL AND hidden = 0 AND message_count > 0 ORDER BY updated_at DESC, id DESC",
        ).all() as Array<{ id: string }>;
        expect(threads.map((thread) => thread.id)).toEqual([secondThreadId, firstThreadId]);
      } finally {
        database.close();
      }

      // 阶段 3：在面板中选择一个不同的线程
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("Enter");
      await tester.waitForText("THREAD_ONE_PERSISTED", { timeout: 60_000, interval: 200 });

      // 等待线程切换完成（消息区加载历史内容）
      await waitForStableScreen(tester, 60_000);

      const capture = await takePeriSnapshot(tester, "thread-switch-done");

      // 基本断言
      expect(panelCapture.text).toContain("Threads");
      expect(capture.text.length).toBeGreaterThan(50);
      expect(capture.text).not.toMatch(/─+\s*Threads\s*─+/);
      expect(capture.text).toContain("hello");
      expect(capture.text).toContain("THREAD_ONE_PERSISTED");
      expect(capture.text).not.toContain("THREAD_TWO_PERSISTED");
      expect(replay.requests.filter((request) => request.isMain), replayDebug(replay)).toHaveLength(2);
      expect(replay.misses(), replayDebug(replay)).toBe(0);

      // LLM judge: 面板阶段
      const panelResult = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "屏幕中应有 Threads 面板，显示历史线程列表（含线程标题和消息计数）",
          "面板中应有可选的线程条目，当前选中项应有视觉提示（如高亮、> 符号）",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(panelResult, null, 2));
      expect(panelResult.pass).toBe(true);

      // 完成态使用结构化文本断言：线程 tab 也会显示标题 "hello"，视觉
      // Judge 容易把 tab 误认成仍打开的 Threads 列表面板。上面的边框标题
      // absence + 历史消息 presence 已精确覆盖切换结果。
    },
  );
});
