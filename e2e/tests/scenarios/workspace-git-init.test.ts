/**
 * 普通目录登记 → 目录内出现 `.git` → 真实 TUI 仍能新建会话并发送输入，重启亦然。
 *
 * 只使用本地 SSE 模型端点，无真实凭据、无外部 API。回归
 * `spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md`：
 * 修复前 `session/new` 在 `resolve_workspace` 处返回 `NeedsRelink`（-32010），
 * TUI 停在 `Input was not accepted. Your draft has been kept.`，该目录永久不可用。
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT, sendPrompt } from "../../helpers/peri.js";

const execFileAsync = promisify(execFile);
const MODEL = "workspace-git-init-model";

describe("目录登记后出现 .git 的工作区", () => {
  let directory: string;
  let home: string;
  let work: string;
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
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-workspace-git-init-"));
    home = path.join(directory, "home");
    work = path.join(directory, "plain-project");
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
          id: "workspace-git-init", object: "chat.completion.chunk", created: 1, model: MODEL,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      };
      response.writeHead(200, { "content-type": "text/event-stream" });
      send({ role: "assistant", content: `WORKSPACE_GIT_INIT_REPLY_${replies}` });
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

  async function launch(): Promise<void> {
    tester = new TmuxTester({
      command: [path.join(PROJECT_ROOT, "target/debug/peri"), `--config-file=${settings}`],
      cwd: work,
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
    await tester!.waitForText(`WORKSPACE_GIT_INIT_REPLY_${reply}`, { timeout: 30_000, interval: 100 });
    expect(await tester!.getScreenText(), "修复前这里是 -32010 的拒绝提示")
      .not.toContain("Input was not accepted");
    // 会话未能建立的提示同样表示输入没有进入队列，不能只挡住旧的拒绝文案。
    expect(await tester!.getScreenText()).not.toContain("Session could not be established");
  }

  /**
   * 提交 slash 命令：`/` 开头的输入会打开补全菜单，第一次 Enter 只接受补全
   * （把 `/clear ` 落回输入框），第二次 Enter 才提交。空输入回车是空操作。
   */
  async function command(text: string): Promise<void> {
    await sendPrompt(tester!, text);
    await tester!.sendKey("enter");
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

  async function bindingCount(): Promise<number | undefined> {
    return (await query<{ n: number }>("SELECT COUNT(*) AS n FROM session_bindings"))[0]?.n;
  }

  async function workspaceSnapshot(): Promise<{ root: string; common_dir: string | null }> {
    const rows = await query<{ discovery: string }>("SELECT discovery FROM workspaces");
    expect(rows, "登记后应恰好有一个工作区").toHaveLength(1);
    return JSON.parse(rows[0].discovery);
  }

  it("目录登记后 git init 仍能新建会话并发送输入，重启亦然", async () => {
    // ① 普通目录首次登记：会话建立、输入送达模型。
    await launch();
    await prompt("GIT_INIT_FIRST_INPUT", 1);
    expect((await workspaceSnapshot()).common_dir, "首次登记是普通目录模式").toBeNull();

    // ② 目录内出现 `.git`：目录对象没有变，注册必须继续可用。
    await execFileAsync("git", ["init", "-q"], { cwd: work });

    // ③ `/clear` 经 ACP `session/new` 建新会话，与首次启动的准入链路相同。
    await command("/clear");
    await expect.poll(bindingCount, {
      timeout: 20_000, interval: 200, message: "`/clear` 应成功建立第二个会话",
    }).toBe(2);
    await prompt("GIT_INIT_SECOND_INPUT", 2);

    // ④ 重启后在同一个目录再次建立会话（用户报告的实际路径）。
    await tester!.stop();
    tester = undefined;
    await launch();
    await prompt("GIT_INIT_THIRD_INPUT", 3);

    // ⑤ 项目 / 工作区标识保持不变，历史绑定未被改绑，观测快照更新为 Git 布局。
    const projects = await query<{ id: string }>("SELECT id FROM projects");
    expect(projects).toHaveLength(1);
    const workspaces = await query<{ id: string; project_id: string }>(
      "SELECT id, project_id FROM workspaces",
    );
    expect(workspaces).toHaveLength(1);
    expect(workspaces[0].project_id).toBe(projects[0].id);
    const bindings = await query<{ project_id: string; workspace_id: string }>(
      "SELECT project_id, workspace_id FROM session_bindings",
    );
    expect(bindings).toHaveLength(3);
    for (const binding of bindings) {
      expect(binding.project_id).toBe(projects[0].id);
      expect(binding.workspace_id).toBe(workspaces[0].id);
    }
    const snapshot = await workspaceSnapshot();
    expect(snapshot.root).toBe(await realpath(work));
    expect(snapshot.common_dir, "Git 布局已刷新为仓库模式").toBe(await realpath(path.join(work, ".git")));
    const history = await query<{ message_count: number }>(
      "SELECT message_count FROM threads ORDER BY created_at LIMIT 1",
    );
    expect(history[0].message_count, "首个会话的历史仍在").toBeGreaterThanOrEqual(2);
  }, 180_000);
});
