/**
 * 已登记目录整体搬迁到新位置 → 新位置仍能建立会话并发送输入，旧绑定与历史原样保留。
 *
 * 只使用本地 SSE 模型端点，无真实凭据、无外部 API。回归
 * `spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md` 的「目录移动」
 * 场景：修复前同一文件对象出现在新路径时，`workspaces` 按 `root_identity` 命中旧
 * 登记而路径不同，`session/new` 返回 `NeedsRelink`（-32010），TUI 停在
 * `Input was not accepted. Your draft has been kept.`，新位置不可用且没有恢复入口。
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, realpath, rename, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT } from "../../helpers/peri.js";

const execFileAsync = promisify(execFile);
const MODEL = "workspace-moved-model";

describe("已登记目录搬迁到新位置", () => {
  let directory: string;
  let home: string;
  let work: string;
  let moved: string;
  let settings: string;
  let database: string;
  let tester: TmuxTester | undefined;
  let server: Server | undefined;
  let replies: number;

  beforeAll(async () => {
    // 控制面脚本不构建 binary；本用例必须跑当前源码。
    await execFileAsync("cargo", ["build", "-p", "peri-tui", "--bin", "peri"], {
      cwd: PROJECT_ROOT,
      timeout: 600_000,
      maxBuffer: 8 * 1024 * 1024,
    });
  }, 610_000);

  beforeEach(async () => {
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-workspace-moved-"));
    home = path.join(directory, "home");
    work = path.join(directory, "project");
    moved = path.join(directory, "relocated");
    settings = path.join(home, ".peri", "settings.json");
    database = path.join(home, ".peri", "threads", "threads.db");
    // 隔离 HOME 时提供空 `~/.cargo/env`，避免用户 shell rc source 失败。
    await mkdir(path.join(home, ".cargo"), { recursive: true });
    await writeFile(path.join(home, ".cargo", "env"), "");
    await mkdir(path.dirname(settings), { recursive: true });
    await mkdir(work);
    replies = 0;
    server = createServer(async (request, response) => {
      for await (const bytes of request) void bytes;
      replies += 1;
      const send = (delta: object, finish: string | null = null) => {
        response.write(`data: ${JSON.stringify({
          id: "workspace-moved", object: "chat.completion.chunk", created: 1, model: MODEL,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      };
      response.writeHead(200, { "content-type": "text/event-stream" });
      send({ role: "assistant", content: `WORKSPACE_MOVED_REPLY_${replies}` });
      send({}, "stop");
      response.end("data: [DONE]\n\n");
    });
    await new Promise<void>((resolve, reject) => {
      server!.once("error", reject);
      server!.listen(0, "127.0.0.1", resolve);
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("本地模型端口未绑定");
    await writeFile(settings, JSON.stringify({ config: {
      language: "en",
      active_alias: "sonnet",
      show_cache_warning: false,
      providers: [{
        id: "local-only",
        type: "openai",
        apiKey: "unused-test-key",
        baseUrl: `http://127.0.0.1:${address.port}/v1`,
        models: { sonnet: MODEL, haiku: MODEL, opus: MODEL, fable: MODEL },
      }],
      profiles: Object.fromEntries(
        ["sonnet", "haiku", "opus", "fable"].map((name) => [name, { provider: "local-only" }]),
      ),
    }}));
  });

  afterEach(async () => {
    try {
      if (tester?.isRunning()) await tester.stop();
    } finally {
      server?.closeAllConnections();
      if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
      if (directory) await rm(directory, { recursive: true, force: true });
      tester = undefined;
      server = undefined;
    }
  });

  /** 在给定目录启动真实 TUI；每次启动都走完整 ACP `session/new` 准入链路。 */
  async function launch(cwd: string): Promise<void> {
    tester = new TmuxTester({
      command: [path.join(PROJECT_ROOT, "target/debug/peri"), `--config-file=${settings}`],
      cwd,
      size: { cols: 120, rows: 40 },
      env: {
        HOME: home,
        XDG_CONFIG_HOME: path.join(home, "config"),
        XDG_CACHE_HOME: path.join(home, "cache"),
        XDG_DATA_HOME: path.join(home, "data"),
        LANG: "en_US.UTF-8", LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
        RUST_LOG_FILE: path.join(directory, "peri.log"),
        LANGFUSE_PUBLIC_KEY: "", LANGFUSE_SECRET_KEY: "",
        OPENAI_API_KEY: "", ANTHROPIC_API_KEY: "",
      },
    });
    await tester.start();
    await tester.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
  }

  /** 发送一句用户输入并断言模型回复到达（回复序号唯一，避免旧画面误命中）。 */
  async function prompt(text: string, reply: number): Promise<void> {
    await tester!.paste(text);
    await tester!.sendKey("enter");
    await tester!.waitForText(`WORKSPACE_MOVED_REPLY_${reply}`, { timeout: 30_000, interval: 100 });
    expect(await tester!.getScreenText(), "修复前这里是 -32010 的拒绝提示")
      .not.toContain("Input was not accepted");
    // 会话未能建立的提示同样表示输入没有进入队列，不能只挡住旧的拒绝文案。
    expect(await tester!.getScreenText()).not.toContain("Session could not be established");
  }

  /** 只读查询隔离数据库；peri 正在运行时也允许并发读，写入瞬间短暂重试。 */
  async function query<T>(sql: string): Promise<T[]> {
    for (let attempt = 0; ; attempt++) {
      const db = new DatabaseSync(database, { readOnly: true });
      try {
        return db.prepare(sql).all() as T[];
      } catch (error) {
        if (attempt >= 20) throw error;
        await new Promise((resolve) => setTimeout(resolve, 100));
      } finally {
        db.close();
      }
    }
  }

  it("目录搬迁后新位置可建会话，旧绑定与历史不被改写或隐藏", async () => {
    // ① 原位置建立会话并留下历史。
    await launch(work);
    await prompt("MOVED_FIRST_INPUT", 1);
    const before = await query<{
      project_id: string; workspace_id: string; root: string; locator: string;
    }>(
      `SELECT b.project_id, b.workspace_id, w.root, p.locator FROM session_bindings b
       JOIN workspaces w ON w.id = b.workspace_id JOIN projects p ON p.id = b.project_id`,
    );
    expect(before, "首个会话已登记绑定").toHaveLength(1);

    // ② 用户把整棵目录搬到新位置：同一文件对象、不同路径。
    await tester!.stop();
    tester = undefined;
    await rename(work, moved);

    // ③ 在新位置启动：修复前 `resolve_workspace` 在这里返回 NeedsRelink。
    await launch(moved);
    await prompt("MOVED_SECOND_INPUT", 2);

    // ④ 新位置得到独立登记，执行目录是搬迁后的真实路径。
    const projects = await query<{ id: string }>("SELECT id FROM projects");
    const workspaces = await query<{ id: string; project_id: string; discovery: string }>(
      "SELECT id, project_id, discovery FROM workspaces",
    );
    expect(projects, "旧登记保持原样、新位置单独登记").toHaveLength(2);
    expect(workspaces).toHaveLength(2);
    const relocated = workspaces.find((workspace) => workspace.id !== before[0].workspace_id);
    expect(relocated, "新会话属于新登记的工作区").toBeDefined();
    expect(relocated!.project_id).not.toBe(before[0].project_id);
    expect(JSON.parse(relocated!.discovery).root).toBe(await realpath(moved));

    // ⑤ 旧绑定没有被改绑、隐藏或重写到新位置，历史消息仍在。
    const bindings = await query<{ project_id: string; workspace_id: string }>(
      "SELECT project_id, workspace_id FROM session_bindings",
    );
    expect(bindings).toHaveLength(2);
    expect(bindings.some((binding) =>
      binding.project_id === before[0].project_id
      && binding.workspace_id === before[0].workspace_id)).toBe(true);
    const oldWorkspace = (await query<{ id: string; root: string }>(
      "SELECT id, root FROM workspaces",
    )).find((workspace) => workspace.id === before[0].workspace_id);
    const oldProject = (await query<{ id: string; locator: string }>(
      "SELECT id, locator FROM projects",
    )).find((project) => project.id === before[0].project_id);
    expect(oldWorkspace?.root, "旧登记仍指向原位置").toBe(before[0].root);
    expect(oldProject?.locator, "旧项目定位不被改写").toBe(before[0].locator);
    const history = await query<{ cwd: string; message_count: number }>(
      "SELECT cwd, message_count FROM threads ORDER BY created_at LIMIT 1",
    );
    expect(history[0].message_count, "首个会话的历史仍在").toBeGreaterThanOrEqual(2);
    expect(history[0].cwd, "旧会话的创建目录不被改写").toBe(before[0].root);
  }, 180_000);
});
