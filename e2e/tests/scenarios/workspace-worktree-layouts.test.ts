/**
 * 主仓库 / linked worktree / 独立 clone / 子目录四种布局的执行目录与项目归属。
 *
 * 只使用本地 SSE 模型端点，无真实凭据、无外部 API。回归
 * `spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md` 的验收条件第 5 项：
 * Git 项目聚合、checkout 识别与执行目录在真实 TUI 上仍按设计分离——项目按 common dir
 * 聚合，工作区按 checkout 区分，会话的执行目录是启动目录本身。
 *
 * symlink 场景不在这里断言：进程 cwd 由 `getcwd` 给出物理路径，TUI 层看不到符号链接
 * 代理，该路径由 resources 单元测试
 * `test_worktree_symlink_discovery_reuses_identity_but_binding_escape_is_rejected` 覆盖。
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
import { PROJECT_ROOT } from "../../helpers/peri.js";

const execFileAsync = promisify(execFile);
const MODEL = "workspace-layouts-model";

describe("仓库布局下的执行目录与项目归属", () => {
  let directory: string;
  let home: string;
  let repo: string;
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
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-workspace-layouts-"));
    home = path.join(directory, "home");
    repo = path.join(directory, "repository");
    settings = path.join(home, ".peri", "settings.json");
    database = path.join(home, ".peri", "threads", "threads.db");
    // 隔离 HOME 时提供空 `~/.cargo/env`，避免用户 shell rc source 失败。
    await mkdir(path.join(home, ".cargo"), { recursive: true });
    await writeFile(path.join(home, ".cargo", "env"), "");
    await mkdir(path.dirname(settings), { recursive: true });
    await mkdir(repo);
    replies = 0;
    server = createServer(async (request, response) => {
      for await (const bytes of request) void bytes;
      replies += 1;
      const send = (delta: object, finish: string | null = null) => {
        response.write(`data: ${JSON.stringify({
          id: "workspace-layouts", object: "chat.completion.chunk", created: 1, model: MODEL,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      };
      response.writeHead(200, { "content-type": "text/event-stream" });
      send({ role: "assistant", content: `WORKSPACE_LAYOUTS_REPLY_${replies}` });
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

  /** 在隔离 HOME 下运行 Git，身份与配置都不落到用户机器上。 */
  async function git(cwd: string, args: string[]): Promise<void> {
    await execFileAsync(
      "git",
      [
        "-c", "user.name=fixture",
        "-c", "user.email=fixture@example.invalid",
        "-c", "commit.gpgsign=false",
        ...args,
      ],
      { cwd, env: { ...process.env, HOME: home, GIT_CONFIG_NOSYSTEM: "1" } },
    );
  }

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
  async function prompt(text: string, reply: number, layout: string): Promise<void> {
    await tester!.paste(text);
    await tester!.sendKey("enter");
    await tester!.waitForText(`WORKSPACE_LAYOUTS_REPLY_${reply}`, {
      timeout: 30_000,
      interval: 100,
    });
    const screen = await tester!.getScreenText();
    expect(screen, `${layout}：出现的是输入未被接受的提示`).not.toContain(
      "Input was not accepted",
    );
    expect(screen, `${layout}：出现的是会话未能建立的提示`).not.toContain(
      "Session could not be established",
    );
  }

  /** 停止当前 TUI；下一次 launch 重新走一遍启动准入。 */
  async function restart(cwd: string): Promise<void> {
    await tester!.stop();
    tester = undefined;
    await launch(cwd);
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

  /** 最近一次准入写下的绑定：项目、工作区、相对路径、工作区根、会话 cwd。 */
  async function latestBinding(): Promise<{
    project_id: string;
    workspace_id: string;
    relative_cwd: string;
    root: string;
    locator: string;
    cwd: string;
  }> {
    const rows = await query<{
      project_id: string;
      workspace_id: string;
      relative_cwd: string;
      root: string;
      locator: string;
      cwd: string;
    }>(
      `SELECT b.project_id, b.workspace_id, b.relative_cwd, w.root, p.locator, t.cwd
       FROM session_bindings b
       JOIN workspaces w ON w.id = b.workspace_id
       JOIN projects p ON p.id = b.project_id
       JOIN threads t ON t.id = b.thread_id
       ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1`,
    );
    expect(rows, "每次进入都应有新建的绑定").toHaveLength(1);
    return rows[0];
  }

  /** 项目展示所依赖的事实：该工作区的 Git 观测快照。 */
  async function discoveryOf(
    workspaceId: string,
  ): Promise<{ root: string; common_dir: string | null }> {
    const rows = await query<{ id: string; discovery: string }>(
      "SELECT id, discovery FROM workspaces",
    );
    const row = rows.find((workspace) => workspace.id === workspaceId);
    expect(row, "工作区登记应存在").toBeDefined();
    return JSON.parse(row!.discovery);
  }

  it(
    "子目录、linked worktree 与独立 clone 各自得到正确的执行目录与项目归属",
    { timeout: 300_000 },
    async () => {
      // ① 主仓库 + 子目录 + linked worktree + 独立 clone。
      await git(repo, ["init", "-q"]);
      await git(repo, ["commit", "--allow-empty", "-qm", "base"]);
      const sub = path.join(repo, "sub");
      await mkdir(sub);
      const worktree = path.join(directory, "linked tree");
      await git(repo, ["worktree", "add", "-qb", "linked", worktree]);
      const clone = path.join(directory, "independent clone");
      await git(directory, ["clone", "-q", repo, clone]);
      const commonDir = path.join(await realpath(repo), ".git");

      // ② 子目录：执行目录是子目录本身，工作区仍是仓库根。
      await launch(sub);
      await prompt("LAYOUTS_SUBDIRECTORY_INPUT", 1, "子目录会话");
      const fromSubdirectory = await latestBinding();
      expect(fromSubdirectory.root, "子目录属于仓库根工作区").toBe(await realpath(repo));
      expect(fromSubdirectory.relative_cwd, "cwd 相对工作区根").toBe("sub");
      expect(fromSubdirectory.cwd, "会话的执行目录是启动目录本身").toBe(await realpath(sub));
      expect(
        (await discoveryOf(fromSubdirectory.workspace_id)).common_dir,
        "子目录会话的项目定位是仓库 common dir",
      ).toBe(commonDir);

      // ③ linked worktree：另一个 checkout，但仍是同一个项目。
      await restart(worktree);
      await prompt("LAYOUTS_WORKTREE_INPUT", 2, "linked worktree 会话");
      const fromWorktree = await latestBinding();
      expect(fromWorktree.root, "worktree 的工作区根是 worktree 路径").toBe(
        await realpath(worktree),
      );
      expect(fromWorktree.relative_cwd, "worktree 根目录没有子路径").toBe("");
      expect(fromWorktree.project_id, "同一仓库的 worktree 属于同一项目").toBe(
        fromSubdirectory.project_id,
      );
      expect(fromWorktree.workspace_id, "worktree 是独立的工作区").not.toBe(
        fromSubdirectory.workspace_id,
      );
      expect(
        (await discoveryOf(fromWorktree.workspace_id)).common_dir,
        "worktree 与主仓库共用 common dir，所以是同一项目",
      ).toBe(commonDir);

      // ④ 独立 clone：自带 common dir，是另一个项目。
      await restart(clone);
      await prompt("LAYOUTS_CLONE_INPUT", 3, "独立 clone 会话");
      const fromClone = await latestBinding();
      expect(fromClone.root, "clone 是独立工作区").toBe(await realpath(clone));
      expect(fromClone.project_id, "clone 不与源仓库共用项目").not.toBe(
        fromSubdirectory.project_id,
      );
      expect(
        (await discoveryOf(fromClone.workspace_id)).common_dir,
        "clone 的 common dir 在 clone 自己身上",
      ).toBe(path.join(await realpath(clone), ".git"));

      // ⑤ 三个会话各自登记，历史都在：项目 2 个（仓库 + clone），工作区 3 个。
      const projects = await query<{ id: string }>("SELECT id FROM projects");
      expect(projects, "仓库与 clone 各一个项目").toHaveLength(2);
      const workspaces = await query<{ id: string }>("SELECT id FROM workspaces");
      expect(workspaces, "子目录与 worktree 各占一个工作区，clone 是第三个").toHaveLength(3);
      const threads = await query<{ cwd: string }>(
        "SELECT cwd FROM threads ORDER BY created_at, rowid",
      );
      expect(threads, "三个会话都留下了可读历史").toHaveLength(3);
      expect(
        threads.map((thread) => thread.cwd),
        "每个会话的执行目录都是它自己的启动目录",
      ).toEqual([await realpath(sub), await realpath(worktree), await realpath(clone)]);
    },
  );
});
