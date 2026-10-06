import { buildPeriForE2e } from "../../helpers/build.js";
/**
 * W6 A 线：TUI 可观察行为（真实二进制 `target/debug/peri` + tmux）。
 *
 * 按 `docs/standards/testing.md` §8.2 命令路径（e2e 分层门禁）验证产品行为，
 * 不以组件快照替代：
 * - 命令发现：输入 `/probe` 时命令面板列出 workspace origin 的
 *   `/probe-skill`（core 投影）与 `/workspace:probe-skill`（server 面投影）；
 * - 显式 token 触发：提交 `/probe-skill` 后首轮模型请求出现 SkillTool 注入
 *   与技能正文（端到端，非快照）；
 * - 关闭：workspace 关闭（`meta_harness.WorkspaceMiddleware=false`）时
 *   `/probe-skill` 不可激活（不注入正文）；
 * - 拒绝：远端 Agent 审批弹窗（`MCP Agent activation wants to run`）Esc 拒绝
 *   后不启动子代理，拒绝理由以 error tool_result 回传父会话。
 *
 * 夹具（隔离 HOME/cwd、假 model server、fixture MCP）见
 * `e2e/helpers/workspace-mcp-fixture.ts`；本文件只覆盖 TUI 通道。
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { TmuxTester } from "tui-tester";
import {
  FX_AGENT,
  FX_SERVER,
  PERI_BIN,
  SKILL_BODY_SENTINEL,
  SKILL_NAME,
  SUBAGENT_PROMPT,
  collectToolResults,
  firstMainRequest,
  isolatedEnv,
  makeWorld,
  messagesJson,
  systemText,
  type World,
  type ToolPlan,
} from "../../helpers/workspace-mcp-fixture.js";

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

/** 启动真实 peri TUI（隔离 HOME/cwd；无 dev.sh，直接二进制）。 */
async function launchTui(world: World): Promise<TmuxTester> {
  const env = isolatedEnv(world);
  const envArgs = Object.entries(env).map(([key, value]) => `${key}=${value}`);
  const invocation = ["env", "-i", ...envArgs, PERI_BIN].map(shellQuote).join(" ");
  const tester = new TmuxTester({
    command: ["/bin/sh", "-c", invocation].map(shellQuote),
    env: { HOME: world.home, PATH: env.PATH },
    cwd: world.work,
    size: { cols: 120, rows: 40 },
  });
  await tester.start();
  await tester.waitForText("AI operating system", { timeout: 60_000, interval: 200 });
  return tester;
}

async function typeText(tester: TmuxTester, text: string): Promise<void> {
  for (const char of text) {
    await tester.sendText(char);
    await tester.sleep(60);
  }
}

async function waitFor(condition: () => boolean, timeoutMs: number, label: string): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`等待超时：${label}`);
}

describe("workspace MCP resources：TUI 可观察行为", () => {
  let worlds: World[] = [];
  let testers: TmuxTester[] = [];

  beforeAll(async () => {
    await buildPeriForE2e();
  }, 610_000);

  afterEach(async () => {
    for (const tester of testers) {
      try {
        if (tester.isRunning()) await tester.stop();
      } catch {
        // 停止失败不掩盖用例结果；残留由控制面清理
      }
    }
    testers = [];
    const closing = worlds;
    worlds = [];
    await Promise.all(closing.map((world) => world.close()));
  });

  async function world(options?: Parameters<typeof makeWorld>[0]): Promise<World> {
    const created = await makeWorld(options);
    worlds.push(created);
    return created;
  }

  async function launch(world: World): Promise<TmuxTester> {
    const tester = await launchTui(world);
    testers.push(tester);
    return tester;
  }

  it("命令发现与触发：面板列出 core/workspace 两条命令，提交后首轮注入技能全文", async () => {
    const w = await world();
    const tester = await launch(w);

    // 命令发现：`/probe` 面板列出两条 workspace origin 命令（产品行为，非快照）
    await typeText(tester, "/probe");
    await tester.waitForText("/probe-skill", { timeout: 15_000, interval: 200 });
    await tester.waitForText("/workspace:probe-skill", { timeout: 15_000, interval: 200 });

    // 补齐完整命令名并提交（Enter 关闭面板、Enter 提交消息）
    await typeText(tester, "-skill");
    await tester.sendKey("enter");
    await tester.sleep(800);
    await tester.sendKey("enter");

    await waitFor(() => w.requests.some((entry) => entry.isMain), 60_000, "TUI 首轮模型请求");
    const first = firstMainRequest(w);
    expect(JSON.stringify(first.body.messages).includes(`"skill_name":"${SKILL_NAME}"`), "出现 SkillTool 调用").toBe(true);
    expect(messagesJson(w).includes(SKILL_BODY_SENTINEL), "技能全文注入 TUI 首轮").toBe(true);
    // 摘要仍在 system（产品面一致）
    expect(systemText(first).includes(`mcp__workspace__${SKILL_NAME}`), "system 摘要含该技能").toBe(true);
  });

  it("关闭：workspace 关闭时技能命令不可激活（不注入全文）", async () => {
    const w = await world({ metaHarness: { WorkspaceMiddleware: false } });
    const tester = await launch(w);

    await typeText(tester, `/${SKILL_NAME}`);
    await tester.sendKey("enter");
    await tester.sleep(800);
    await tester.sendKey("enter");

    await waitFor(() => w.requests.some((entry) => entry.isMain), 60_000, "TUI 首轮模型请求");
    const messages = messagesJson(w);
    expect(messages.includes(SKILL_BODY_SENTINEL), "关闭态不得注入技能正文").toBe(false);
    expect(messages.includes('"name":"SkillTool"'), "关闭态不得产生 SkillTool 调用").toBe(false);
    // 关闭态下命令面板的可见性只作观测登记（不硬断言）：stdio 同名证据
    // （available_commands 残留）见 W6 报告；本例只保证「不可激活」。
  });

  it("拒绝：远端 Agent 审批弹窗 Esc 拒绝后不启动，理由以 error tool_result 回传", async () => {
    const toolPlan: ToolPlan = {
      name: "Agent",
      input: { subagent_type: `mcp__${FX_SERVER}__${FX_AGENT}`, prompt: SUBAGENT_PROMPT },
    };
    const w = await world({ toolPlan });
    const tester = await launch(w);

    await tester.paste("delegate to beta");
    await tester.sendKey("enter");

    // HITL 审批弹窗（产品文案）
    await tester.waitForText("MCP Agent activation wants to run", { timeout: 60_000, interval: 250 });
    await tester.sendKey("escape");

    await waitFor(
      () => collectToolResults(w).some((result) => result.isError),
      60_000,
      "拒绝后的 error tool_result",
    );
    const results = collectToolResults(w);
    expect(
      results.some((result) => result.isError && result.text.includes("Cancelled by user")),
      "拒绝理由回传父会话",
    ).toBe(true);
    // 拒绝后不启动：无子代理请求（独立上下文 = 单条 message 的主请求）
    const subagent = w.requests.find((entry) => entry.isMain && entry.body?.messages?.length === 1);
    expect(subagent, "拒绝后不得启动子代理").toBeUndefined();
    // 子代理成功回执（child_thread_id）不出现
    expect(
      results.some((result) => !result.isError && result.text.includes("child_thread_id")),
      "拒绝后不得出现子代理成功回执",
    ).toBe(false);
  });
});
