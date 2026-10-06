import { buildPeriForE2e } from "../../helpers/build.js";
/**
 * W6 A 线：真实二进制链路验收（print / stdio ACP）。
 *
 * 驱动 `target/debug/peri`（生产装配路径），本地假 model server（127.0.0.1）
 * 捕获模型请求，外部 fixture MCP server（stdio，`e2e/fixtures/mcp-skill-fixture.mjs`）
 * 记录收到的 JSON-RPC（`#recv` 行即 wire 事实）。
 *
 * 覆盖（计划 §8.2 必测矩阵的可观测子集）：
 * - ① 首请求：system 含冻结技能摘要（workspace 本地来源）+ 项目指令；tools 面
 *   含宿主 SkillTool/DiscoverSkillsTool 与 workspace 七工具（裸名）；workspace
 *   不注册同名技能工具；发现阶段技能正文 `resources/read` 计数 = 0（fixture 端）；
 * - ② 显式 token（X1=R1）：`/skill-name`（本地）与 `/server:skill`（外部）触发
 *   首轮全文注入；外部形态经 `resources/read`（fixture 端计数 ≥1）；
 * - ③ 批准/拒绝：fixture 远端 Agent 的 HITL 批准面（stdio ACP 的
 *   `session/request_permission`）——批准后子代理启动、拒绝后不启动且不注入；
 * - ④ 关闭矩阵：workspace 关闭（`meta_harness.WorkspaceMiddleware=false`）时
 *   七工具/摘要/指令全部不可得、磁盘技能正文不回落；
 * - ⑤ 回归：七工具表与 resources 面（`workspace://git/ref`）不回归；
 * - ⑥ 子代理 preload 缺口（W6）：项目本地 agent 声明未命中技能 ⇒ 子代理首轮
 *   注入 is_error 回执（不静默启动不完整配置），父会话不因缺口中止。
 *
 * 夹具（makeWorld / 假 model server / fixture wire 读取）在
 * `e2e/helpers/workspace-mcp-fixture.ts`；TUI 面见同目录
 * `workspace-mcp-resources-tui.test.ts`。
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { spawn } from "node:child_process";
import { startStdioExecutionFixture } from "../../helpers/stdio-execution-fixture.js";
import {
  FX_AGENT,
  FX_SERVER,
  FX_SKILL,
  FX_SKILL_BODY_SENTINEL,
  GAP_AGENT,
  GAP_SKILL,
  GAP_SUBAGENT_PROMPT,
  INSTRUCTION_SENTINEL,
  MODEL_REPLY,
  PERI_BIN,
  SKILL_BODY_SENTINEL,
  SKILL_NAME,
  SKILL_SECTION_MARKER,
  SUBAGENT_PROMPT,
  WORKSPACE_TOOLS,
  collectToolResults,
  countFixtureMethod,
  firstMainRequest,
  fixtureWireLines,
  isolatedEnv,
  makeWorld,
  messagesJson,
  systemText,
  toolNames,
  type World,
  type ToolPlan,
} from "../../helpers/workspace-mcp-fixture.js";

/** print 路径：真实 binary，`-p "..." --output-format stream-json`。 */
async function runPrint(
  world: World,
  prompt: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(
      PERI_BIN,
      ["-p", prompt, "--output-format", "stream-json", "--permission-mode", "bypass"],
      { cwd: world.work, env: isolatedEnv(world) },
    );
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("exit", (code) => resolve({ exitCode: code ?? -1, stdout, stderr }));
  });
}

interface StdioRun {
  serverRequests: Array<{ method: string; params: any }>;
  /** `session/update` 的 available_commands_update 快照（每次通知一项，命令名列表）。 */
  commandSnapshots: string[][];
  exitCode: number | null;
}

/** stdio ACP 路径：真实 binary，`peri acp --cwd`，initialize → session/new → prompt。 */
async function runStdio(
  world: World,
  prompt: string,
  permission: "approve" | "reject" | "none",
): Promise<StdioRun> {
  const serverRequests: StdioRun["serverRequests"] = [];
  const commandSnapshots: string[][] = [];
  const fixture = await startStdioExecutionFixture({
    binary: PERI_BIN,
    cwd: world.work,
    home: world.home,
    env: isolatedEnv(world),
    onNotification(message) {
      if (message.method === "session/update") {
        const commands = message.params?.update?.availableCommands;
        if (Array.isArray(commands)) {
          commandSnapshots.push(commands.map((command: any) => command.name));
        }
      }
    },
    onServerRequest(message) {
      serverRequests.push({ method: message.method, params: message.params });
    },
    handleServerRequest(message) {
      if (message.method === "session/request_permission") {
        const outcome =
          permission === "approve"
            ? { outcome: "selected", optionId: "allow_once" }
            : permission === "reject"
              ? { outcome: "selected", optionId: "reject_once" }
              : { outcome: "cancelled" };
        return { result: { outcome } };
      }
      return {
        error: { code: -32601, message: `unsupported: ${message.method}` },
      };
    },
  });
  try {
    const initialize = await fixture.call("initialize", {
      protocolVersion: 1,
      clientCapabilities: {
        _meta: { "peri.userInputQueue": true },
      },
    }, 30_000);
    expect(initialize.result?.protocolVersion, "initialize 应答 protocolVersion").toBe(1);
    expect(initialize.result?.agentCapabilities?._meta?.["peri.userInputQueue"],
      "initialize must negotiate the actual user input queue capability before session/new",
    ).toBe(true);
    const created = await fixture.call("session/new", { cwd: world.work }, 60_000);
    const sessionId = created.result?.sessionId as string;
    expect(sessionId, "session/new 返回 sessionId").toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 1200));
    const response = await fixture.call("session/prompt", {
      sessionId,
      prompt: [{ type: "text", text: prompt }],
    }, 180_000);
    expect(response.error,
      `session/prompt must reach actual execution, not fail required preflight; ACP error=${JSON.stringify(response.error)}; stdio bridge=${fixture.diagnostics()}`,
    ).toBeUndefined();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const exitCode = await fixture.close();
    return { serverRequests, commandSnapshots, exitCode };
  } finally {
    await fixture.close();
  }
}

describe("workspace MCP resources：真实二进制验收（print / stdio）", () => {
  let worlds: World[] = [];

  beforeAll(async () => {
    await buildPeriForE2e();
  }, 610_000);

  afterEach(async () => {
    const closing = worlds;
    worlds = [];
    await Promise.all(closing.map((world) => world.close()));
  });

  async function world(options?: Parameters<typeof makeWorld>[0]): Promise<World> {
    const created = await makeWorld(options);
    worlds.push(created);
    return created;
  }

  it("① 首请求：system 含技能摘要与项目指令，tools 含七工具与宿主技能工具，发现阶段零正文读取", async () => {
    const w = await world();
    const run = await runPrint(w, "hello w6");
    expect(run.exitCode, `print 退出码；stderr=${run.stderr.slice(0, 400)}`).toBe(0);

    const first = firstMainRequest(w);
    const system = systemText(first);
    const names = toolNames(first);

    // 冻结技能摘要（workspace 本地来源：project 技能 + builtin 静态资产）与来源标签
    expect(system).toContain(SKILL_SECTION_MARKER);
    expect(system).toContain(`mcp__workspace__${SKILL_NAME}`);
    expect(system).toContain("[project]");
    // 项目指令经资源面进入 system（peri-instruction://workspace/main）
    expect(system).toContain(INSTRUCTION_SENTINEL);

    // 宿主两工具 + workspace 七工具（裸名投影）
    expect(names).toContain("SkillTool");
    expect(names).toContain("DiscoverSkillsTool");
    for (const tool of WORKSPACE_TOOLS) expect(names, `七工具表含 ${tool}`).toContain(tool);
    // workspace 不注册同名技能工具（宿主是唯一技能工具面）
    expect(names.filter((name) => name.startsWith("mcp__workspace__") && /skill/i.test(name))).toEqual([]);

    // 发现阶段不读正文：外部 fixture 只收到 skills/list，未收到 resources/read
    const wire = await fixtureWireLines(w.fixtureLog);
    expect(countFixtureMethod(wire, "skills/list"), "fixture 收到 skills/list").toBeGreaterThanOrEqual(1);
    expect(countFixtureMethod(wire, "resources/read"), "发现阶段正文读取计数").toBe(0);
    // workspace 本地来源与外部来源的正文都不因发现进入任一模型请求
    const allMessages = messagesJson(w);
    expect(allMessages.includes(SKILL_BODY_SENTINEL), "发现阶段本地正文不得出现").toBe(false);
    expect(allMessages.includes(FX_SKILL_BODY_SENTINEL), "发现阶段外部正文不得出现").toBe(false);
  });

  it("② 显式 token（本地 /skill-name）：首轮全文注入，摘要不替代全文", async () => {
    const w = await world();
    const run = await runPrint(w, `/${SKILL_NAME} please apply`);
    expect(run.exitCode).toBe(0);

    const first = firstMainRequest(w);
    const messages = JSON.stringify(first.body?.messages ?? []);
    // SkillPreload 注入：Ai[ToolUse{SkillTool}] → Tool[ToolResult(全文)]
    expect(messages).toContain('"name":"SkillTool"');
    expect(messages).toContain(`"skill_name":"${SKILL_NAME}"`);
    expect(messages.includes(SKILL_BODY_SENTINEL), "全文哨兵必须进入首轮 messages").toBe(true);
    // 摘要面仍只列元数据（正文不进 system）
    expect(systemText(first).includes(SKILL_BODY_SENTINEL), "正文不得进入 system").toBe(false);
  });

  it("② 显式 token（外部 /server:skill）：经 resources/read 触发全文（可观测计数 ≥1）", async () => {
    const w = await world();
    const run = await runPrint(w, `/${FX_SERVER}:${FX_SKILL} please apply`);
    expect(run.exitCode).toBe(0);

    const wire = await fixtureWireLines(w.fixtureLog);
    expect(countFixtureMethod(wire, "resources/read"), "激活经 resources/read").toBeGreaterThanOrEqual(1);
    const readLine = wire.find(
      (line) => line.includes('"method":"resources/read"') && line.includes(FX_SKILL),
    );
    expect(readLine, "read 目标为技能入口 URI").toBeTruthy();

    const first = firstMainRequest(w);
    const messages = JSON.stringify(first.body?.messages ?? []);
    expect(messages.includes(FX_SKILL_BODY_SENTINEL), "外部全文进入首轮").toBe(true);
  });

  it("③ 批准：远端 Agent 经 HITL 批准后启动，拒绝后不启动且不注入", async () => {
    const toolPlan: ToolPlan = {
      name: "Agent",
      input: { subagent_type: `mcp__${FX_SERVER}__${FX_AGENT}`, prompt: SUBAGENT_PROMPT },
    };
    const approved = await world({ toolPlan });
    const approvedRun = await runStdio(approved, "please delegate", "approve");

    // 批准面：session/request_permission（内容绑定批准；stdio ACP 可编程裁决）
    const permission = approvedRun.serverRequests.find(
      (request) => request.method === "session/request_permission",
    );
    expect(permission, "批准面必须到达客户端").toBeTruthy();
    expect(permission!.params?.toolCall?.title).toBe("MCP Agent activation");
    expect(permission!.params?.toolCall?.rawInput?.uri).toContain(`agent://remote/${FX_AGENT}`);
    // 激活读取：fixture 收到 agent 定义 resources/read（批准前的激活读取，如实登记：
    // mcp_activation 先 activate（读定义）再向客户端发批准请求）。
    const approvedWire = await fixtureWireLines(approved.fixtureLog);
    const agentReads = approvedWire.filter(
      (line) => line.includes('"method":"resources/read"') && line.includes(`agent://remote/${FX_AGENT}`),
    ).length;
    expect(agentReads, "agent 定义经 resources/read 读取").toBeGreaterThanOrEqual(1);
    // 批准前 fixture 端零「执行」：没有任何 tools/call
    expect(countFixtureMethod(approvedWire, "tools/call"), "批准前零工具执行").toBe(0);
    // 批准后子代理启动：出现子代理请求（messages 仅 1 条 = 独立子代理上下文且带任务文本）
    const subagentRequest = approved.requests.find((entry) => entry.isMain && entry.body?.messages?.length === 1);
    expect(subagentRequest, "批准后子代理启动并产生独立请求").toBeTruthy();
    expect(
      JSON.stringify(subagentRequest!.body.messages).includes(SUBAGENT_PROMPT),
      "子代理请求携带任务文本",
    ).toBe(true);
    // 父会话收到成功 tool_result（子代理回执：child_thread_id + 子代理回复文本）
    const approvedResults = collectToolResults(approved);
    expect(
      approvedResults.some((result) => !result.isError && result.text.includes("child_thread_id")),
      "批准后父会话收到子代理成功回执",
    ).toBe(true);
    expect(
      approvedResults.some((result) => !result.isError && result.text.includes(MODEL_REPLY)),
      "子代理运行结果（模型回复）回传父会话",
    ).toBe(true);

    const rejected = await world({ toolPlan });
    const rejectedRun = await runStdio(rejected, "please delegate", "reject");
    expect(
      rejectedRun.serverRequests.some((request) => request.method === "session/request_permission"),
      "拒绝路径同样先到达批准面",
    ).toBe(true);
    // 拒绝后不启动：无子代理请求
    const rejectedSubagent = rejected.requests.find(
      (entry) => entry.isMain && entry.body?.messages?.length === 1,
    );
    expect(rejectedSubagent, "拒绝后不得启动子代理").toBeUndefined();
    // 拒绝理由回传父会话（拒绝的 tool_result 为 error；不含子代理产物）
    const rejectedResults = collectToolResults(rejected);
    expect(
      rejectedResults.some((result) => result.text.includes("reject_once") && result.isError),
      "拒绝理由以 error tool_result 回传",
    ).toBe(true);
    expect(
      rejectedResults.some((result) => !result.isError && result.text.includes("child_thread_id")),
      "拒绝后不得出现子代理成功回执",
    ).toBe(false);
    const rejectedWire = await fixtureWireLines(rejected.fixtureLog);
    expect(countFixtureMethod(rejectedWire, "tools/call"), "拒绝后零工具执行").toBe(0);
  });

  it("④ 关闭矩阵：workspace 关闭时不注册工具、技能与指令不可得、磁盘技能不回落", async () => {
    const w = await world({ metaHarness: { WorkspaceMiddleware: false } });
    const run = await runPrint(w, `/${SKILL_NAME} should not load`);
    expect(run.exitCode).toBe(0);

    const first = firstMainRequest(w);
    const names = toolNames(first);
    const system = systemText(first);
    for (const tool of WORKSPACE_TOOLS) {
      expect(names, `关闭后不得注册 ${tool}`).not.toContain(tool);
    }
    expect(names).not.toContain("mcp__workspace__SkillTool");
    expect(system.includes(SKILL_SECTION_MARKER), "关闭后不得有技能摘要").toBe(false);
    expect(system.includes(INSTRUCTION_SENTINEL), "关闭后不得有项目指令").toBe(false);
    const wire = await fixtureWireLines(w.fixtureLog);
    const sentinelRequests = w.requests.flatMap((entry) => {
      const messages = JSON.stringify(entry.body?.messages ?? []);
      const position = messages.indexOf(SKILL_BODY_SENTINEL);
      if (position < 0) return [];
      return [{
        requestIndex: entry.index,
        isMain: entry.isMain,
        excerpt: messages.slice(Math.max(0, position - 240), position + SKILL_BODY_SENTINEL.length + 240),
      }];
    });
    const closedDiagnostic = JSON.stringify({
      sentinelRequests,
      home: w.home,
      work: w.work,
      modelPort: w.modelPort,
      fixtureWire: wire.slice(-12),
      stderr: run.stderr.slice(-4000),
    });
    expect(messagesJson(w).includes(SKILL_BODY_SENTINEL),
      `关闭态正文哨兵不得进入 messages; actual closed-world evidence=${closedDiagnostic}`,
    ).toBe(false);
    expect(countFixtureMethod(wire, "resources/read"), "关闭时不产生技能正文读取").toBe(0);
  });

  it("④ 关闭矩阵（stdio 命令面）：技能命令不可激活（不注入全文）", async () => {
    const w = await world({ metaHarness: { WorkspaceMiddleware: false } });
    await runStdio(w, `/${SKILL_NAME} should not load`, "none");
    const messages = messagesJson(w);
    expect(messages.includes(SKILL_BODY_SENTINEL), "关闭态不得注入技能正文").toBe(false);
    expect(messages.includes('"name":"SkillTool"'), "关闭态不得产生 SkillTool 调用").toBe(false);
  });

  it("⑤ 回归：resources 面保留既有 workspace 资源（git ref）", async () => {
    const w = await world();
    const run = await runPrint(w, "resources regression check");
    expect(run.exitCode).toBe(0);
    const system = systemText(firstMainRequest(w));
    // deferred 工具 mcp_read_resource 的可用资源清单仍含既有 workspace 资源
    expect(system).toContain("workspace://git/ref");
    // 七工具与宿主技能工具表（与 ① 同口径）
    const names = toolNames(firstMainRequest(w));
    for (const tool of WORKSPACE_TOOLS) expect(names).toContain(tool);
    expect(names).toContain("SkillTool");
  });

  it("⑥ 子代理 preload 缺口：声明技能未命中 ⇒ 子代理收到 is_error 回执，父会话不中止", async () => {
    const toolPlan: ToolPlan = {
      name: "Agent",
      input: { subagent_type: GAP_AGENT, prompt: GAP_SUBAGENT_PROMPT },
    };
    const w = await world({ toolPlan, localAgent: { name: GAP_AGENT, skills: [GAP_SKILL] } });
    const run = await runPrint(w, "delegate to the gap agent");
    expect(run.exitCode, `print 退出码；stderr=${run.stderr.slice(0, 400)}`).toBe(0);

    // 子代理独立上下文：请求定位锚在子代理独有事实——`messages[0]` 恰为子代理任务
    // 文本（其后才是注入序列）。父会话的续跑请求（Human/Ai(Agent tool_use)/Tool
    // 三条）`messages[0]` 是外层 prompt，不满足此判据；单独用「长度 3」或
    // 「包含任务文本」都无法排除父请求误命中。
    const messageText = (message: any): string =>
      typeof message?.content === "string"
        ? message.content
        : (message?.content ?? []).map((part: any) => part?.text ?? "").join("\n");
    const subagentRequest = w.requests.find(
      (entry) => entry.isMain && messageText(entry.body?.messages?.[0]) === GAP_SUBAGENT_PROMPT,
    );
    expect(subagentRequest, "子代理启动并产生独立请求").toBeTruthy();
    expect(
      subagentRequest!.body.messages.length,
      "子代理首轮消息 = 任务 + 注入序列（Ai[ToolUse] + Tool[回执]），非父会话历史",
    ).toBe(3);

    // 缺口见证：Ai[ToolUse{SkillTool}] 与 Tool[is_error] 回执同一批注入、按 id 配对
    const messages = (subagentRequest!.body.messages ?? []) as any[];
    const blocks = messages.flatMap((message) => message.content ?? []);
    const toolUse = blocks.find(
      (block: any) =>
        block?.type === "tool_use" &&
        block?.name === "SkillTool" &&
        block?.input?.skill_name === GAP_SKILL,
    );
    expect(toolUse, "缺口项注入假 SkillTool 调用").toBeTruthy();
    const gapResults = blocks.filter(
      (block: any) => block?.type === "tool_result" && block?.is_error === true,
    );
    expect(gapResults.length, "缺口回执数量 = 声明技能数").toBe(1);
    expect(gapResults[0].tool_use_id, "缺口回执与 ToolUse 按 id 配对").toBe(toolUse.id);
    const gapText = (gapResults[0].content ?? [])
      .map((part: any) => part?.text ?? "")
      .join("\n");
    expect(gapText, "回执点名缺口技能").toContain(GAP_SKILL);
    // 精确到「未命中」产品串（本场景 workspace 技能面已开启，见 ①；若断言放宽为
    // 「not found 或 registry is not wired」则无法发现子代理链丢装配 registry 的回归）。
    expect(gapText, `缺口文案 = SkillTool not-found 产品串：${gapText}`).toContain(
      `Skill '${GAP_SKILL}' not found`,
    );
    expect(gapText, "缺口不得来自 registry 未装配路径").not.toContain("registry is not wired");

    // 父会话不因缺口中止：子代理完成并把回执回传父会话（父请求 ≥2 轮）
    const results = collectToolResults(w);
    expect(
      results.some((result) => !result.isError && result.text.includes("child_thread_id")),
      "父会话收到子代理成功回执",
    ).toBe(true);
    expect(w.requests.filter((entry) => entry.isMain).length).toBeGreaterThanOrEqual(2);
  });
});
