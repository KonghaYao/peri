/** 正式 TUI → ACP → Mailbox → 模型请求：本地屏障 SSE，无外部模型调用。 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server, type ServerResponse } from "node:http";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT, takePeriSnapshot } from "../../helpers/peri.js";

type HeldRequest = {
  body: { messages: Array<{ role: string; content: unknown }> };
  response: ServerResponse;
  closed: boolean;
};

function chunk(response: ServerResponse, delta: object, finish: string | null = null): void {
  response.write(`data: ${JSON.stringify({
    id: "local-steer-response",
    object: "chat.completion.chunk",
    created: 1,
    model: "steer-main",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`);
}

function finish(request: HeldRequest): void {
  chunk(request.response, { content: " STEER_LOCAL_DONE" });
  chunk(request.response, {}, "stop");
  request.response.end("data: [DONE]\n\n");
}

function plain(screen: string): string {
  return screen.replace(/[\u2066-\u2069]/g, "");
}

function pendingCount(screen: string): number {
  const count = plain(screen).match(/(?:^|\n)\s*(?:[▸▾>v]\s+)?待发送\s+(\d+)/)?.[1];
  return count === undefined ? 0 : Number(count);
}

function pendingRows(screen: string): string[] {
  const lines = plain(screen).split("\n");
  const start = lines.findIndex((line) => /待发送\s+\d+/.test(line));
  if (start === -1) return [];
  const end = lines.findIndex((line, i) => i > start && /^\s*[─━]/.test(line));
  return lines.slice(start + 1, end < 0 ? undefined : end);
}

function userContent(request: HeldRequest): string {
  return JSON.stringify(request.body.messages.filter((message) => message.role === "user"));
}

describe("smoke: 正式待发送队列", () => {
  let tester: TmuxTester | undefined;
  let directory: string;
  let server: Server | undefined;
  let requests: HeldRequest[];

  beforeEach(async () => {
    requests = [];
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-steer-live-"));
    server = createServer(async (request, response) => {
      const bytes: Buffer[] = [];
      for await (const data of request) bytes.push(Buffer.from(data));
      const body = JSON.parse(Buffer.concat(bytes).toString("utf8"));
      response.writeHead(200, { "content-type": "text/event-stream" });
      if (body.model !== "steer-main") {
        chunk(response, { role: "assistant", content: "Local test" });
        chunk(response, {}, "stop");
        response.end("data: [DONE]\n\n");
        return;
      }
      const held: HeldRequest = { body, response, closed: false };
      response.on("close", () => { held.closed = true; });
      requests.push(held);
      chunk(response, { role: "assistant", content: `STEER_RUN_${requests.length}_OPEN` });
    });
    await new Promise<void>((resolve, reject) => {
      server!.once("error", reject);
      server!.listen(0, "127.0.0.1", resolve);
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("本地模型端口未绑定");
    const settings = path.join(directory, ".peri", "settings.json");
    await mkdir(path.dirname(settings), { recursive: true });
    await mkdir(path.join(directory, ".cargo"), { recursive: true });
    await writeFile(path.join(directory, ".cargo", "env"), "");
    await writeFile(settings, JSON.stringify({ config: {
      language: "zh-CN",
      active_alias: "sonnet",
      show_cache_warning: false,
      providers: [{
        id: "local-steer",
        type: "openai",
        apiKey: "local-test-only",
        baseUrl: `http://127.0.0.1:${address.port}/v1`,
        models: { sonnet: "steer-main", haiku: "steer-meta", opus: "steer-meta", fable: "steer-meta" },
      }],
      profiles: Object.fromEntries(["sonnet", "haiku", "opus", "fable"].map((name) => [name, {
        provider: "local-steer", model: name === "sonnet" ? "steer-main" : "steer-meta",
        effort: "low", max_tokens: 4096,
      }])),
    }}));
    // 直接使用构建产物，避免 dev.sh source 仓库 .env；所有配置/日志留在临时目录。
    tester = new TmuxTester({
      command: [path.join(PROJECT_ROOT, "target/debug/peri"), `--config-file=${settings}`],
      cwd: directory,
      size: { cols: 120, rows: 40 },
      env: {
        HOME: directory,
        XDG_CONFIG_HOME: path.join(directory, "config"),
        XDG_CACHE_HOME: path.join(directory, "cache"),
        XDG_DATA_HOME: path.join(directory, "data"),
        LANG: "zh_CN.UTF-8", LC_ALL: "zh_CN.UTF-8", TERM: "xterm-256color",
        RUST_LOG_FILE: path.join(directory, "peri.log"),
        LANGFUSE_PUBLIC_KEY: "", LANGFUSE_SECRET_KEY: "",
        OPENAI_API_KEY: "", ANTHROPIC_API_KEY: "",
      },
    });
    await tester.start();
    await tester.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
  });

  afterEach(async () => {
    try {
      if (tester?.isRunning()) await tester.stop();
    } finally {
      server?.closeAllConnections();
      if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
      if (directory) await rm(directory, { recursive: true, force: true });
      tester = undefined;
    }
  });

  async function submit(text: string): Promise<void> {
    await tester!.paste(text);
    await tester!.sendKey("enter");
  }

  async function waitPending(count: number): Promise<void> {
    await tester!.waitFor((screen) => pendingCount(screen) === count, {
      timeout: 10_000, interval: 100, message: `待发送队列应有 ${count} 条`,
    });
  }

  async function waitRequest(count: number): Promise<HeldRequest> {
    await expect.poll(() => requests.length, { timeout: 15_000, interval: 50 }).toBe(count);
    return requests[count - 1];
  }

  async function click(label: string, symbol: string): Promise<void> {
    const lines = plain(await tester!.getScreenText()).split("\n");
    const row = lines.findIndex((line) => line.includes(label) && line.includes(symbol));
    expect(row, `应存在 ${label} 的 ${symbol} 操作`).toBeGreaterThanOrEqual(0);
    const index = lines[row].lastIndexOf(symbol);
    const col = Array.from(lines[row].slice(0, index)).reduce(
      (width, char) => width + (/[\u2e80-\u9fff\uff01-\uff60]/.test(char) ? 2 : 1), 0,
    );
    await tester!.click(col, row);
  }

  async function queued(text: string): Promise<void> {
    await tester!.waitFor((screen) => pendingRows(screen).some(
      (line) => line.includes(text) && /\s↑\s+↶\s*$/.test(line),
    ), { timeout: 10_000, interval: 100, message: `${text} 应确认入队且可操作` });
  }

  it("单发 B 保留 A/C，全发不带上随后新增 D，自然完成再发送 D", async () => {
    await submit("STEER_SEED");
    const first = await waitRequest(1);
    await waitPending(0);
    const imagePath = path.join(directory, "queued-image.png");
    const imageBase64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aXioAAAAASUVORK5CYII=";
    await writeFile(imagePath, Buffer.from(imageBase64, "base64"));
    for (const [label, text] of [
      ["STEER_A", `STEER_A @image ${imagePath}`],
      ["STEER_B", "STEER_B"],
      ["STEER_C", "STEER_C"],
    ]) {
      await submit(text);
      await queued(label);
    }
    await click("STEER_B", "↑");
    const second = await waitRequest(2);
    await waitPending(2);
    expect(userContent(second)).toContain("STEER_B");
    expect(userContent(second)).not.toContain("STEER_A");
    expect(userContent(second)).not.toContain("STEER_C");
    await expect.poll(() => first.closed).toBe(true);
    await click("待发送", "⇈");
    const third = await waitRequest(3);
    await submit("STEER_D");
    await queued("STEER_D");
    await waitPending(1);
    const delivered = userContent(third);
    for (const text of ["STEER_A", "STEER_B", "STEER_C"]) {
      expect(delivered.split(text).length - 1, `${text} 只进入历史一次`).toBe(1);
    }
    expect(delivered.indexOf("STEER_B")).toBeLessThan(delivered.indexOf("STEER_A"));
    expect(delivered.indexOf("STEER_A")).toBeLessThan(delivered.indexOf("STEER_C"));
    expect(delivered).not.toContain("STEER_D");
    // A 不是本批最后一条；它的图片仍须进入真实 provider 请求。
    expect(delivered).toContain(`data:image/png;base64,${imageBase64}`);
    finish(third);
    const fourth = await waitRequest(4);
    expect(userContent(fourth).split("STEER_D").length - 1).toBe(1);
    finish(fourth);
    await waitPending(0);
    const capture = await takePeriSnapshot(tester!, "steer-live-single-all-natural");
    expect(capture.text).not.toContain("待发送");
  });

  it("已有草稿时撤回队列项，保留草稿且不再发送撤回内容", async () => {
    await submit("STEER_SEED");
    await waitRequest(1);
    await submit("STEER_WITHDRAW");
    await queued("STEER_WITHDRAW");
    await tester!.paste("STEER_CURRENT_DRAFT");
    await click("STEER_WITHDRAW", "↶");
    await waitPending(0);
    expect(plain(await tester!.getScreenText())).toContain("STEER_CURRENT_DRAFT");
    await tester!.sendKey("enter");
    await queued("STEER_CURRENT_DRAFT");
    expect(plain(await tester!.getScreenText())).not.toContain("STEER_WITHDRAW");
    await click("待发送", "⇈");
    const second = await waitRequest(2);
    expect(userContent(second)).toContain("STEER_CURRENT_DRAFT");
    expect(userContent(second)).not.toContain("STEER_WITHDRAW");
    finish(second);
    await waitPending(0);
    await takePeriSnapshot(tester!, "steer-live-withdraw-preserve-draft");
  });

  it("取回多行原稿后重新提交，Stop 保留待发队列并可再次单发", async () => {
    await submit("STEER_SEED");
    const first = await waitRequest(1);
    await submit("STEER_EDIT_FIRST\nSTEER_EDIT_SECOND");
    await queued("STEER_EDIT_FIRST");
    await click("STEER_EDIT_FIRST", "↶");
    await waitPending(0);
    await tester!.waitForText("STEER_EDIT_SECOND", { timeout: 5_000, interval: 100 });
    await tester!.sendText("_RESTORED");
    await tester!.sendKey("enter");
    await queued("STEER_EDIT_FIRST");
    await tester!.sendKey("c", { ctrl: true });
    await expect.poll(() => first.closed, { timeout: 10_000 }).toBe(true);
    await queued("STEER_EDIT_FIRST");
    expect(requests).toHaveLength(1);
    await click("STEER_EDIT_FIRST", "↑");
    const second = await waitRequest(2);
    const content = userContent(second);
    expect(content).toContain("STEER_EDIT_FIRST\\nSTEER_EDIT_SECOND_RESTORED");
    expect(content.split("STEER_EDIT_FIRST").length - 1).toBe(1);
    finish(second);
    await waitPending(0);
    await takePeriSnapshot(tester!, "steer-live-takeback-stop-resume");
  });
});
