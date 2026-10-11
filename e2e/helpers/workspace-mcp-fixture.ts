/**
 * W6 A 线共享夹具：真实二进制链路验收（workspace MCP resources）。
 *
 * - 隔离世界：临时 HOME + 临时 cwd（项目技能 / AGENTS.md / `.mcp.json` 指向
 *   外部 fixture MCP server 与本地假 model server），满足 TEST-HERMETIC-001；
 * - 本地假 model server：记录每个请求 body（`isMain` = 含 tools 的主请求），
 *   第 1 个主请求可按 `toolPlan` 返回 tool_use（驱动 Agent 审批路径）；
 * - fixture MCP server：`e2e/fixtures/mcp-skill-fixture.mjs`，wire 日志即事实。
 *
 * 本文件不含断言，供 print/stdio 与 TUI 两个场景文件共用。
 */
import { createServer, type Server } from "node:http";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { PROJECT_ROOT } from "./peri.js";

export const PERI_BIN = path.join(PROJECT_ROOT, "target/debug/peri");
export const FIXTURE_SERVER = path.join(PROJECT_ROOT, "e2e", "fixtures", "mcp-skill-fixture.mjs");

export const SKILL_NAME = "probe-skill";
export const SKILL_DESC = "W6-PROBE-SKILL-DESC";
export const SKILL_BODY_SENTINEL = "W6-PROBE-SKILL-BODY-SENTINEL";
export const INSTRUCTION_SENTINEL = "W6-PROBE-INSTRUCTION-SENTINEL";
export const FX_SERVER = "skillfx";
export const FX_SKILL = "demo-skill";
export const FX_SKILL_BODY_SENTINEL = "W6-FX-SKILL-BODY-SENTINEL";
export const FX_AGENT = "beta";
export const SUBAGENT_PROMPT = "W6-SUBAGENT-TASK-TEXT";
/** 子代理 preload 缺口见证：项目本地 agent 定义 + 一个未声明的技能名。 */
export const GAP_AGENT = "gap-agent";
export const GAP_SKILL = "missing-skill-x";
export const GAP_SUBAGENT_PROMPT = "W6-GAP-SUBAGENT-TASK-TEXT";
export const WORKSPACE_TOOLS = ["Read", "Write", "Edit", "Glob", "Grep", "folder_operations", "Bash"];
export const SKILL_SECTION_MARKER = "你可以使用以下 Skills";
export const MODEL_REPLY = "W6-MODEL-REPLY";

export type ModelRequest = { index: number; isMain: boolean; body: any };
export type ToolPlan = { name: string; input: Record<string, unknown> } | null;

export interface World {
  root: string;
  home: string;
  work: string;
  modelPort: number;
  requests: ModelRequest[];
  fixtureLog: string;
  close(): Promise<void>;
}

/** 本地假 model server（Anthropic SSE）：主请求按 plan 返回 tool_use 或文本。 */
export function startModelServer(
  requests: ModelRequest[],
  toolPlan: ToolPlan,
): Promise<{ server: Server; port: number }> {
  let mainCount = 0;
  const server = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    let body: any = null;
    try {
      body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } catch {
      body = null;
    }
    const isMain = Array.isArray(body?.tools) && body.tools.length > 0;
    if (isMain) mainCount += 1;
    requests.push({ index: requests.length + 1, isMain, body });
    response.writeHead(200, { "content-type": "text/event-stream" });
    const head = [
      ["message_start", { type: "message_start", message: { id: `w6-${requests.length}`, type: "message", role: "assistant", model: body?.model ?? "w6-model", content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } }],
    ];
    const tail = (stopReason: string) => [
      ["message_delta", { type: "message_delta", delta: { stop_reason: stopReason, stop_sequence: null }, usage: { output_tokens: 5 } }],
      ["message_stop", { type: "message_stop" }],
    ];
    let events: Array<[string, object]>;
    if (isMain && toolPlan && mainCount === 1) {
      events = [
        ...head,
        ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "w6-toolu-1", name: toolPlan.name, input: {} } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(toolPlan.input) } }],
        ["content_block_stop", { type: "content_block_stop", index: 0 }],
        ...tail("tool_use"),
      ];
    } else {
      events = [
        ...head,
        ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: isMain ? MODEL_REPLY : "" } }],
        ["content_block_stop", { type: "content_block_stop", index: 0 }],
        ...tail("end_turn"),
      ];
    }
    response.end(
      events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join(""),
    );
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("假 model server 未绑定端口");
      resolve({ server, port: address.port });
    });
  });
}

/** 世界夹具：隔离 HOME + 临时 cwd（项目技能/指令/.mcp.json）+ 假 model server。 */
export async function makeWorld(
  options: {
    metaHarness?: Record<string, boolean>;
    toolPlan?: ToolPlan;
    /**
     * 项目本地 agent 定义（`{work}/.claude/agents/<name>.md`）；frontmatter
     * `skills` 声明驱动子代理 SkillPreloadMiddleware 的预载/缺口路径。
     */
    localAgent?: { name: string; description?: string; skills?: string[] };
  } = {},
): Promise<World> {
  const root = await mkdtemp(path.join(os.tmpdir(), "peri-w6-"));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  await mkdir(path.join(home, ".peri"), { recursive: true });
  await mkdir(path.join(work, ".claude", "skills", SKILL_NAME), { recursive: true });
  await writeFile(
    path.join(work, ".claude", "skills", SKILL_NAME, "SKILL.md"),
    `---\nname: ${SKILL_NAME}\ndescription: ${SKILL_DESC}\n---\n\n# Local skill\n${SKILL_BODY_SENTINEL}\n`,
  );
  await writeFile(path.join(work, "AGENTS.md"), `# Project rules\n\n${INSTRUCTION_SENTINEL}\n`);
  if (options.localAgent) {
    const { name, skills = [] } = options.localAgent;
    const description = options.localAgent.description ?? `W6 local agent ${name}`;
    const agentsDir = path.join(work, ".claude", "agents");
    await mkdir(agentsDir, { recursive: true });
    const skillsBlock = skills.length
      ? `skills:\n${skills.map((skill) => `  - ${skill}`).join("\n")}\n`
      : "";
    await writeFile(
      path.join(agentsDir, `${name}.md`),
      `---\nname: ${name}\ndescription: ${description}\n${skillsBlock}---\n\nW6 local agent body.\n`,
    );
  }
  const fixtureLog = path.join(root, "fixture-wire.log");
  await writeFile(
    path.join(work, ".mcp.json"),
    JSON.stringify({
      mcpServers: {
        [FX_SERVER]: {
          command: process.execPath,
          args: [FIXTURE_SERVER],
          env: { FX_LOG: fixtureLog, FX_NAME: FX_SERVER, FX_SKILL: FX_SKILL, FX_AGENT: FX_AGENT },
        },
      },
    }),
  );

  const requests: ModelRequest[] = [];
  const { server, port } = await startModelServer(requests, options.toolPlan ?? null);
  const metaHarness = options.metaHarness
    ? `,"meta_harness":${JSON.stringify(options.metaHarness)}`
    : "";
  await writeFile(
    path.join(home, ".peri", "settings.json"),
    `{"config":{"active_alias":"sonnet","providers":[{"id":"local","type":"anthropic","apiKey":"w6-dummy-key","baseUrl":"http://127.0.0.1:${port}","models":{"sonnet":"w6-model"}}]${metaHarness}}}`,
  );

  return {
    root,
    home,
    work,
    modelPort: port,
    requests,
    fixtureLog,
    async close() {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      // fixture MCP 子进程由 SUT（peri）持有并在其退出时收敛；本夹具不持有 pid，
      // 失败路径的残留进程为已登记限制（验收 §6.4）。
      await rm(root, { recursive: true, force: true });
    },
  };
}

export function isolatedEnv(world: World): Record<string, string> {
  return {
    HOME: world.home,
    XDG_CONFIG_HOME: path.join(world.home, "config"),
    XDG_CACHE_HOME: path.join(world.home, "cache"),
    XDG_DATA_HOME: path.join(world.home, "data"),
    PATH: process.env.PATH || "/usr/bin:/bin",
    TERM: "xterm-256color",
    LANG: "en_US.UTF-8",
    HTTP_PROXY: "http://127.0.0.1:9",
    HTTPS_PROXY: "http://127.0.0.1:9",
    ALL_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "localhost,127.0.0.1",
  };
}

export async function fixtureWireLines(logPath: string): Promise<string[]> {
  try {
    return (await readFile(logPath, "utf8")).split("\n").filter(Boolean);
  } catch {
    return [];
  }
}

export function countFixtureMethod(lines: string[], method: string): number {
  return lines.filter((line) => line.includes(`"method":"${method}"`)).length;
}

export function firstMainRequest(world: World): ModelRequest {
  const request = world.requests.find((entry) => entry.isMain);
  if (!request) throw new Error("未捕获到首个主模型请求");
  return request;
}

export function systemText(request: ModelRequest): string {
  const system = request.body?.system;
  if (typeof system === "string") return system;
  return (system ?? []).map((block: any) => block.text ?? "").join("\n");
}

export function toolNames(request: ModelRequest): string[] {
  return (request.body?.tools ?? []).map((tool: any) => tool.name);
}

export function messagesJson(world: World): string {
  return JSON.stringify(world.requests.map((entry) => entry.body?.messages ?? []));
}

/** 汇总所有模型请求 messages 中的 tool_result 块（text + is_error）。 */
export function collectToolResults(world: World): Array<{ text: string; isError: boolean }> {
  const results: Array<{ text: string; isError: boolean }> = [];
  for (const entry of world.requests) {
    for (const message of entry.body?.messages ?? []) {
      for (const block of message.content ?? []) {
        if (block?.type !== "tool_result") continue;
        const text = Array.isArray(block.content)
          ? block.content.map((part: any) => part?.text ?? "").join("\n")
          : String(block.content ?? "");
        results.push({ text, isError: Boolean(block.is_error) });
      }
    }
  }
  return results;
}
