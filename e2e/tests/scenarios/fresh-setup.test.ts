/** 空 HOME → 真实配置向导 → 首条模型请求；只使用本地 SSE，无真实凭据。 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT } from "../../helpers/peri.js";

type ModelRequest = { server: number; body: { model: string; messages: unknown[] } };

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

describe("首次 setup 的保存与运行时交接", () => {
  let directory: string;
  let tester: TmuxTester | undefined;
  let settings: string;
  let servers: Server[];
  let endpoints: string[];
  let requests: ModelRequest[];

  beforeAll(async () => {
    // The standard E2E runner does not build; compile before changing HOME so
    // this suite always exercises current sources, with the real toolchain cache.
    await promisify(execFile)("cargo", ["build", "-p", "peri-tui", "--bin", "peri"], {
      cwd: PROJECT_ROOT,
      timeout: 600_000,
      maxBuffer: 8 * 1024 * 1024,
    });
  }, 610_000);

  beforeEach(async () => {
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-fresh-setup-"));
    servers = [];
    endpoints = [];
    requests = [];
    await mkdir(path.join(directory, "home"));
    await mkdir(path.join(directory, "work"));
    for (const id of [0, 1]) {
      const server = createServer(async (request, response) => {
        if (request.method !== "POST") {
          response.writeHead(200).end("local endpoint");
          return;
        }
        const chunks: Buffer[] = [];
        for await (const chunk of request) chunks.push(Buffer.from(chunk));
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        requests.push({ server: id, body });
        response.writeHead(200, { "content-type": "text/event-stream" });
        const events: Array<[string, object]> = [
          ["message_start", { type: "message_start", message: {
            id: `fresh-setup-${id}`, type: "message", role: "assistant", model: body.model,
            content: [], stop_reason: null, stop_sequence: null,
            usage: { input_tokens: 100, output_tokens: 0 },
          } }],
          ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
          ["content_block_delta", { type: "content_block_delta", index: 0,
            delta: { type: "text_delta", text: `FRESH_SETUP_REPLY_${id}` } }],
          ["content_block_stop", { type: "content_block_stop", index: 0 }],
          ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null },
            usage: { output_tokens: 6 } }],
          ["message_stop", { type: "message_stop" }],
        ];
        response.end(events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join(""));
      });
      servers.push(server);
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", resolve);
      });
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("本地 fixture 未绑定端口");
      endpoints.push(`http://127.0.0.1:${address.port}`);
    }
  });

  afterEach(async () => {
    try {
      if (tester?.isRunning()) await tester.stop();
    } finally {
      await Promise.all(servers.map(async (server) => {
        server.closeAllConnections();
        await new Promise<void>((resolve) => server.close(() => resolve()));
      }));
      await rm(directory, { recursive: true, force: true });
      tester = undefined;
    }
  });

  async function launch(customSettings = false, navigateToForm = true): Promise<void> {
    const home = path.join(directory, "home");
    settings = customSettings
      ? path.join(directory, "config-target", "settings.json")
      : path.join(home, ".peri", "settings.json");
    // 不使用 launchPeri：它会预建 .peri/settings.json，掩盖真正空 HOME 的回归。
    const env = {
      HOME: home,
      XDG_CONFIG_HOME: path.join(home, "config"),
      XDG_CACHE_HOME: path.join(home, "cache"),
      XDG_DATA_HOME: path.join(home, "data"),
      PATH: process.env.PATH || "/usr/bin:/bin",
      SHELL: "/bin/sh", TERM: "xterm-256color", LANG: "en_US.UTF-8",
      HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9",
      ALL_PROXY: "http://127.0.0.1:9", NO_PROXY: "localhost,127.0.0.1",
    };
    const invocation = [
      "env", "-i", ...Object.entries(env).map(([key, value]) => `${key}=${value}`),
      path.join(PROJECT_ROOT, "target/debug/peri"),
      ...(customSettings ? ["--config-file", settings] : []),
    ].map(shellQuote).join(" ");
    const exitFile = path.join(directory, "peri-exit-code");
    tester = new TmuxTester({
      command: ["/bin/sh", "-c", `${invocation}; result=$?; printf '%s' "$result" > ${shellQuote(exitFile)}`].map(shellQuote),
      env: { HOME: home, PATH: env.PATH },
      cwd: path.join(directory, "work"),
      size: { cols: 120, rows: 40 },
    });
    await tester.start();
    await tester.waitForText("Choose your interface language", { timeout: 20_000, interval: 100 });
    if (!navigateToForm) return;
    await tester.sendKey("enter");
    await tester.waitForText("Custom API");
    await tester.sendKey("enter");
    await tester.waitForText("Submit");
  }

  async function editProvider(endpoint: string): Promise<void> {
    await tester!.sendKey("enter");
    await tester!.waitForText("Test connectivity");
    await tester!.sendKey("down");
    await tester!.sendKey("down");
    await tester!.sendKey("end");
    await tester!.sendKey("w", { ctrl: true });
    await tester!.paste(endpoint);
    await tester!.sendKey("down");
    await tester!.sendKey("down");
    await tester!.sendKey("end");
    await tester!.sendKey("w", { ctrl: true });
    await tester!.paste("fresh-setup-dummy-key");
    await tester!.sendKey("escape");
    await tester!.waitForText("Submit");
    await tester!.sendKey("down");
    await tester!.sendKey("enter");
    await tester!.waitForText("Key:");
  }

  async function saveAndSubmit(marker: string, server: number): Promise<void> {
    await tester!.sendKey("enter");
    await tester!.waitForText("Shift+Enter", { timeout: 20_000, interval: 100 });
    const config = JSON.parse(await readFile(settings, "utf8"));
    expect(config.config.providers[0].baseUrl).toBe(endpoints[server]);
    await tester!.paste(marker);
    await tester!.sendKey("enter");
    await tester!.waitForText(`FRESH_SETUP_REPLY_${server}`, { timeout: 20_000, interval: 100 });
    const matching = requests.filter((request) => JSON.stringify(request.body.messages).includes(marker));
    expect(matching.some((request) => request.server === server), "消息必须到达刚保存的 endpoint").toBe(true);
  }

  it("全新 HOME 保存后无需重启即可提交，并能再次 setup 更新相同 provider", async () => {
    await launch();
    await editProvider(endpoints[0]);
    await saveAndSubmit("FRESH_SETUP_FIRST_INPUT", 0);

    await tester!.paste("/setup");
    await tester!.sendKey("enter");
    await tester!.waitForText(endpoints[0], { timeout: 10_000, interval: 100 });
    await editProvider(endpoints[1]);
    await saveAndSubmit("FRESH_SETUP_UPDATED_INPUT", 1);
    const config = JSON.parse(await readFile(settings, "utf8"));
    expect(config.config.providers).toHaveLength(1);
    const logDirectory = path.join(directory, "home/.peri/logs");
    const logFiles = await readdir(logDirectory);
    expect(logFiles.length).toBeGreaterThan(0);
    const logs = (await Promise.all(logFiles.map((logFile) =>
      readFile(path.join(logDirectory, logFile), "utf8")))).join("\n");
    expect(logs.length).toBeGreaterThan(0);
    expect(logs, "粘贴的 API key 不得进入原始输入日志").not.toContain("fresh-setup-dummy-key");
  });

  it.each(["escape", "double_ctrl_c"])("首次向导 %s 退出，不进入离线主界面", async (key) => {
    await launch(false, false);
    if (key === "double_ctrl_c") {
      await tester!.sendKey("c", { ctrl: true });
      // Production requires a second key after the 200ms replay guard and
      // within its 1s quit window. This is a user interaction, not a UI wait.
      await tester!.sleep(300);
      await tester!.sendKey("c", { ctrl: true });
    } else await tester!.sendKey("escape");
    await expect.poll(async () => {
      try { return await readFile(path.join(directory, "peri-exit-code"), "utf8"); }
      catch { return null; }
    }, { timeout: 10_000 }).toBe("0");
    await expect(readFile(settings, "utf8")).rejects.toThrow();
    expect(requests).toHaveLength(0);
  });

  it("保存失败保留向导，移除路径障碍后能直接重试并提交", async () => {
    await launch(true);
    await editProvider(endpoints[0]);
    const obstruction = path.dirname(settings);
    await writeFile(obstruction, "isolated parent obstruction");
    await tester!.sendKey("enter");
    await tester!.waitFor((screen) => /fail|cannot|unable|could not/i.test(screen), {
      timeout: 10_000, interval: 100, message: "保存失败必须展示在向导中",
    });
    expect(await tester!.getScreenText()).not.toContain("AI operating system");
    await expect(readFile(settings, "utf8")).rejects.toThrow();
    await rm(obstruction);
    await saveAndSubmit("FRESH_SETUP_RETRIED_INPUT", 0);
  });
});
