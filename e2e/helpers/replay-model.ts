/**
 * 本地假 model server「重放」夹具（Anthropic SSE 剧本驱动真实 peri 二进制）。
 *
 * 用途（TEST-HERMETIC-001）：L0 tier 用例不得依赖真实模型/凭据/外部网络。
 * 先例 `helpers/workspace-mcp-fixture.ts` + `tests/scenarios/workspace-mcp-resources-tui.test.ts`
 * （隔离 HOME + 真实二进制 + tmux + 假 server）；本文件是其最小重放形态：
 * 断言留在各用例，假 server 按「user 文本哨兵 / 期望 tool_result」逐步匹配剧本。
 *
 * 延迟（必须）：完成态 footer 仅在 `summary_elapsed_ms > 0` 时渲染
 * （peri-tui/src/kit/message_area/footer.rs:150-158），故每步响应前固定延迟，
 * 保证 loading 周期非零。
 */
import { createServer } from "node:http";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { TmuxTester } from "tui-tester";
import type { TerminalSize } from "tui-tester";
import { PROJECT_ROOT } from "./peri.js";

export const REPLAY_PERI_BIN = path.join(PROJECT_ROOT, "target/debug/peri");
/** 剧本未命中时的固定回复（主请求未命中计入 `misses`，辅助请求不计数）。 */
export const REPLAY_FALLBACK_TEXT = "REPLAY-UNMATCHED-STEP";
const DEFAULT_STEP_DELAY_MS = 150;

export interface ReplayToolUse {
  name: string;
  input: Record<string, unknown>;
}

export interface ReplayStep {
  /** 首步哨兵：user 消息文本须包含它（防止辅助请求误消费剧本）。 */
  prompt?: string;
  /** 文本块（可与 `toolUses` 同一步：先文本后工具）。 */
  text?: string;
  /** 一步内的多个 tool_use（id 唯一，content_block index 递增）。 */
  toolUses?: ReplayToolUse[];
}

export interface ReplayRequest {
  index: number;
  isMain: boolean;
  /** 是否消费了剧本步（主请求未命中即偏航，回固定文本）。 */
  matched: boolean;
  /** user 消息文本（截断 200 字符，供失败诊断）。 */
  userText: string;
  body: any;
}

export interface ReplayServer {
  port: number;
  requests: ReplayRequest[];
  /** 未命中剧本的主请求数——> 0 表示用例已偏航。 */
  misses(): number;
  close(): Promise<void>;
}

function messageText(message: any): string {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((block: any) => (typeof block?.text === "string" ? block.text : ""))
    .join("\n");
}

/**
 * 全部 user 消息文本（按序拼接）。
 *
 * [TRAP] 不能只看「最后一条 user 消息」：peri 会在首个主请求末尾追加
 * `<system-reminder ...>` 的 user 角色消息（MCP connection_summary 等），
 * 真实用户输入在其之前的 user 消息里。
 */
function userText(body: any): string {
  const messages: any[] = Array.isArray(body?.messages) ? body.messages : [];
  return messages
    .filter((message) => message?.role === "user")
    .map((message) => messageText(message))
    .join("\n");
}

function toolResultIds(body: any): Set<string> {
  const ids = new Set<string>();
  for (const message of body?.messages ?? []) {
    if (!Array.isArray(message?.content)) continue;
    for (const block of message.content) {
      if (block?.type === "tool_result" && typeof block.tool_use_id === "string") {
        ids.add(block.tool_use_id);
      }
    }
  }
  return ids;
}

/** 组装一步的 SSE 事件（文本块在前，tool_use 块 index 递增）。 */
function stepEvents(
  step: ReplayStep,
  stepNumber: number,
  model: string,
): { events: Array<[string, object]>; toolUseIds: string[] } {
  const events: Array<[string, object]> = [
    ["message_start", { type: "message_start", message: {
      id: `replay-${stepNumber}`, type: "message", role: "assistant", model,
      content: [], stop_reason: null, stop_sequence: null,
      usage: { input_tokens: 10, output_tokens: 0 },
    } }],
  ];
  const toolUseIds: string[] = [];
  let index = 0;
  if (step.text !== undefined) {
    events.push(
      ["content_block_start", { type: "content_block_start", index, content_block: { type: "text", text: "" } }],
      ["content_block_delta", { type: "content_block_delta", index, delta: { type: "text_delta", text: step.text } }],
      ["content_block_stop", { type: "content_block_stop", index }],
    );
    index += 1;
  }
  for (const use of step.toolUses ?? []) {
    const id = `replay-${stepNumber}-${index}`;
    toolUseIds.push(id);
    events.push(
      ["content_block_start", { type: "content_block_start", index, content_block: { type: "tool_use", id, name: use.name, input: {} } }],
      ["content_block_delta", { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: JSON.stringify(use.input) } }],
      ["content_block_stop", { type: "content_block_stop", index }],
    );
    index += 1;
  }
  events.push(
    ["message_delta", { type: "message_delta", delta: { stop_reason: toolUseIds.length > 0 ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: 5 } }],
    ["message_stop", { type: "message_stop" }],
  );
  return { events, toolUseIds };
}

/** 启动假 model server：按剧本顺序消费主请求（含 tools），其余请求回固定文本。 */
export function startReplayModelServer(
  steps: ReplayStep[],
  delayMs: number = DEFAULT_STEP_DELAY_MS,
): Promise<ReplayServer> {
  const requests: ReplayRequest[] = [];
  let cursor = 0;
  let misses = 0;
  let lastToolUseIds: string[] = [];

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
    const userMessages = userText(body);

    const model = body?.model ?? "replay-model";
    const step: ReplayStep | undefined = cursor < steps.length ? steps[cursor] : undefined;
    const matched =
      isMain &&
      step !== undefined &&
      (step.prompt !== undefined
        ? userMessages.includes(step.prompt)
        : lastToolUseIds.length > 0
          ? lastToolUseIds.every((id) => toolResultIds(body).has(id))
          : true);
    requests.push({ index: requests.length + 1, isMain, matched, userText: userMessages, body });

    let events: Array<[string, object]>;
    if (matched) {
      cursor += 1;
      const built = stepEvents(step, cursor, model);
      events = built.events;
      lastToolUseIds = built.toolUseIds;
    } else {
      if (isMain) misses += 1;
      events = stepEvents({ text: REPLAY_FALLBACK_TEXT }, 0, model).events;
    }

    await new Promise((resolve) => setTimeout(resolve, delayMs));
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(
      events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join(""),
    );
  });

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("replay model server 未绑定端口");
      resolve({
        port: address.port,
        requests,
        misses: () => misses,
        async close() {
          server.closeAllConnections();
          await new Promise<void>((done) => server.close(() => done()));
        },
      });
    });
  });
}

/** 请求轨迹摘要（断言失败时输出，定位剧本偏航）。 */
export function replayDebug(server: ReplayServer): string {
  return server.requests
    .map(
      (request) =>
        `#${request.index} main=${request.isMain} matched=${request.matched} ` +
        `msgs=${(request.body?.messages ?? []).length} userText=${JSON.stringify(request.userText.slice(0, 120))}`,
    )
    .join(" | ");
}

/** 隔离 HOME：`~/.peri/settings.json` 指向假 model server（无真实凭据）。 */
export async function makeReplayHome(serverPort: number): Promise<string> {
  const home = await mkdtemp(path.join(os.tmpdir(), "peri-replay-home-"));
  await mkdir(path.join(home, ".peri"), { recursive: true });
  await writeFile(
    path.join(home, ".peri", "settings.json"),
    `{"config":{"language":"zh-CN","active_alias":"sonnet","providers":[{"id":"replay","type":"anthropic","apiKey":"replay-dummy-key","baseUrl":"http://127.0.0.1:${serverPort}","models":{"sonnet":"replay-model"}}]}}`,
  );
  return home;
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

/** 启动真实 peri TUI（隔离 HOME，`env -i` 不继承父进程凭据；cwd 默认 PROJECT_ROOT）。 */
export async function launchReplayTui(options: {
  home: string;
  size?: TerminalSize;
  /** peri 进程工作目录：默认 PROJECT_ROOT，保持 Read/Glob 相对路径语义。 */
  cwd?: string;
}): Promise<TmuxTester> {
  const env: Record<string, string> = {
    HOME: options.home,
    XDG_CONFIG_HOME: path.join(options.home, "config"),
    XDG_CACHE_HOME: path.join(options.home, "cache"),
    XDG_DATA_HOME: path.join(options.home, "data"),
    PATH: process.env.PATH || "/usr/bin:/bin",
    TERM: "xterm-256color",
    LANG: "en_US.UTF-8",
    // 死端口兜底：任何真实外网请求都不可达。
    HTTP_PROXY: "http://127.0.0.1:9",
    HTTPS_PROXY: "http://127.0.0.1:9",
    ALL_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "localhost,127.0.0.1",
  };
  const invocation = ["env", "-i", ...Object.entries(env).map(([key, value]) => `${key}=${value}`), REPLAY_PERI_BIN]
    .map(shellQuote)
    .join(" ");
  const tester = new TmuxTester({
    command: ["/bin/sh", "-c", invocation].map(shellQuote),
    env: { HOME: env.HOME, PATH: env.PATH },
    cwd: options.cwd ?? PROJECT_ROOT,
    size: options.size ?? { cols: 120, rows: 40 },
  });
  await tester.start();
  await tester.waitForText("AI operating system", { timeout: 60_000, interval: 200 });
  return tester;
}
