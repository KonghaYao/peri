/**
 * 无 Git 环境的普通目录会话：PATH 中不存在 `git` 时，仍能新建会话并发送输入。
 *
 * 只使用本地 SSE 模型端点，无真实凭据、无外部 API。验收
 * `spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md` 第 2 项：
 * 「无 Git 的普通目录能新建会话并成功发送一次输入；无重复入队，草稿状态正确」。
 *
 * 依据：`discovery.rs::git` 只在 spawn 返回 `NotFound` 时降级为目录模式，因此本
 * 用例的 PATH 必须是「真的找不到 git」而不是「git 失败」——`execvp` 在 ENOENT 时
 * 会继续搜索后续 PATH 项，所以 shim 目录之外不能再挂真实工具目录。
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, readdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT } from "../../helpers/peri.js";

const execFileAsync = promisify(execFile);
const MODEL = "workspace-no-git-model";
const GIT = /^git/;

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

describe("无 Git 环境的工作区", () => {
  let directory: string;
  let home: string;
  let work: string;
  let shim: string;
  let settings: string;
  let database: string;
  let tester: TmuxTester | undefined;
  let server: Server | undefined;
  let replies: number;

  /**
   * 建立「除 `git` 外与系统一致」的 PATH 目录：逐项软链 `/usr/bin`、`/bin`，
   * 跳过所有 `git*`。这样沙箱里缺的只有 Git，不是整个工具链。
   */
  async function buildGitlessPath(): Promise<string> {
    const shim = path.join(directory, "path-without-git");
    await mkdir(shim);
    for (const source of ["/usr/bin", "/bin"]) {
      for (const name of await readdir(source)) {
        if (GIT.test(name)) continue;
        await symlink(path.join(source, name), path.join(shim, name)).catch(() => {});
      }
    }
    const node = path.join(shim, "node");
    await symlink(process.execPath, node).catch(() => {});
    return shim;
  }

  beforeAll(async () => {
    // 控制面脚本不构建 binary；本用例必须跑当前源码。
    await execFileAsync("cargo", ["build", "-p", "peri-tui", "--bin", "peri"], {
      cwd: PROJECT_ROOT,
      timeout: 600_000,
      maxBuffer: 8 * 1024 * 1024,
    });
  }, 610_000);

  beforeEach(async () => {
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-workspace-no-git-"));
    home = path.join(directory, "home");
    work = path.join(directory, "plain-project");
    settings = path.join(home, ".peri", "settings.json");
    database = path.join(home, ".peri", "threads", "threads.db");
    // 隔离 HOME 时提供空 `~/.cargo/env`，避免用户 shell rc source 失败。
    await mkdir(path.join(home, ".cargo"), { recursive: true });
    await writeFile(path.join(home, ".cargo", "env"), "");
    await mkdir(path.dirname(settings), { recursive: true });
    await mkdir(work);
    shim = await buildGitlessPath();
    replies = 0;
    server = createServer(async (request, response) => {
      for await (const bytes of request) void bytes;
      replies += 1;
      const send = (delta: object, finish: string | null = null) => {
        response.write(`data: ${JSON.stringify({
          id: "workspace-no-git", object: "chat.completion.chunk", created: 1, model: MODEL,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      };
      response.writeHead(200, { "content-type": "text/event-stream" });
      send({ role: "assistant", content: `WORKSPACE_NO_GIT_REPLY_${replies}` });
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

  /**
   * 用 `env -i` 显式构造进程环境。tmux 的 `-e PATH=` 会被会话 shell 的 PATH 覆盖
   * （实测：login bash 经 `/etc/profile` 重置 PATH），因此「无 Git」必须由命令
   * 自身保证，而不是靠 tmux 传环境；否则用例会在有 Git 的宿主上静默退化。
   */
  async function launch(cwd: string = work): Promise<void> {
    const environment = {
      HOME: home,
      PATH: shim,
      SHELL: "/bin/sh",
      XDG_CONFIG_HOME: path.join(home, "config"),
      XDG_CACHE_HOME: path.join(home, "cache"),
      XDG_DATA_HOME: path.join(home, "data"),
      LANG: "en_US.UTF-8", LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
      RUST_LOG_FILE: path.join(directory, "peri.log"),
      LANGFUSE_PUBLIC_KEY: "", LANGFUSE_SECRET_KEY: "",
      OPENAI_API_KEY: "", ANTHROPIC_API_KEY: "",
    };
    const invocation = [
      "env", "-i",
      ...Object.entries(environment).map(([key, value]) => `${key}=${value}`),
      path.join(PROJECT_ROOT, "target/debug/peri"),
      `--config-file=${settings}`,
    ].map(shellQuote).join(" ");
    tester = new TmuxTester({
      // 整个数组都要引用：TmuxTester 把 command 用空格拼成一行交给会话 shell，
      // 少了这步 `sh -c env -i …` 会被拆成多个参数，等于只执行 `env`。
      command: ["/bin/sh", "-c", invocation].map(shellQuote),
      cwd,
      size: { cols: 120, rows: 40 },
      env: { HOME: home },
    });
    await tester.start();
    await tester.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
  }

  /** 发送一句用户输入并断言模型回复到达（回复序号唯一，避免旧画面误命中）。 */
  async function prompt(text: string, reply: number): Promise<void> {
    await tester!.paste(text);
    await tester!.sendKey("enter");
    await tester!.waitForText(`WORKSPACE_NO_GIT_REPLY_${reply}`, { timeout: 30_000, interval: 100 });
    expect(await tester!.getScreenText(), "无 Git 不应阻断输入").not.toContain("Input was not accepted");
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

  async function messageCounts(): Promise<{ role: string; n: number }[]> {
    return query<{ role: string; n: number }>(
      "SELECT role, COUNT(*) AS n FROM messages GROUP BY role ORDER BY role",
    );
  }

  it("PATH 中没有 git 时仍能建会话、发送输入且不重复入队", async () => {
    // 前提守卫：该 PATH 必须真的找不到 git，否则用例会退化成有 Git 路径。
    await expect(
      execFileAsync("/bin/sh", ["-c", "command -v git"], { env: { PATH: shim } }),
    ).rejects.toThrow();

    await launch();
    await prompt("NO_GIT_FIRST_INPUT", 1);

    // 普通目录模式：没有 Git 布局证据，不猜测父目录归属。
    const workspaces = await query<{ discovery: string }>("SELECT discovery FROM workspaces");
    expect(workspaces, "登记后应恰好有一个工作区").toHaveLength(1);
    const discovery = JSON.parse(workspaces[0].discovery) as {
      root: string; common_dir: string | null; private_dir: string | null;
    };
    expect(discovery.common_dir, "无 Git 时不得推断出仓库布局").toBeNull();
    expect(discovery.private_dir).toBeNull();
    expect(await query("SELECT id FROM projects")).toHaveLength(1);
    expect(await query("SELECT thread_id FROM session_bindings")).toHaveLength(1);

    // 草稿状态：输入已被接收，空输入框再回车不得重复入队，也不得再请求模型。
    await tester!.sendKey("enter");
    await new Promise((resolve) => setTimeout(resolve, 1_500));
    expect(replies, "空回车不得产生第二次模型请求").toBe(1);
    expect(await messageCounts()).toEqual([
      { role: "assistant", n: 1 },
      { role: "user", n: 1 },
    ]);

    // 同一会话继续可用：第二次输入正常送达，历史里仍只有各自一条。
    await prompt("NO_GIT_SECOND_INPUT", 2);
    expect(await messageCounts()).toEqual([
      { role: "assistant", n: 2 },
      { role: "user", n: 2 },
    ]);
    expect(await query("SELECT thread_id FROM session_bindings")).toHaveLength(1);
  }, 180_000);

  it("重启后同一目录再次建会话（无 Git 时不依赖仓库锚点）", async () => {
    await launch();
    await prompt("NO_GIT_LAUNCH_ONE_INPUT", 1);
    await tester!.stop();
    tester = undefined;
    await launch();
    await prompt("NO_GIT_LAUNCH_TWO_INPUT", 2);

    const projects = await query<{ id: string }>("SELECT id FROM projects");
    const workspaces = await query<{ id: string; project_id: string }>(
      "SELECT id, project_id FROM workspaces",
    );
    expect(projects).toHaveLength(1);
    expect(workspaces).toHaveLength(1);
    expect(workspaces[0].project_id).toBe(projects[0].id);
    expect(await query("SELECT thread_id FROM session_bindings")).toHaveLength(2);
  }, 180_000);

  /**
   * 判别用例：路径确实在 Git 仓库里，但 Peri 的 PATH 中没有 git。
   * 若 PATH 未被真正应用（git 可见），`discovery.root` 会是仓库 toplevel 且
   * `common_dir` 非空，本用例立即失败——它同时守住了前两个用例的前提。
   */
  it("Git 不可用时仓库子目录按 cwd 建区，不推断仓库关系", async () => {
    const repository = path.join(directory, "repository");
    const nested = path.join(repository, "nested");
    await mkdir(nested, { recursive: true });
    await execFileAsync("git", ["init", "-q"], { cwd: repository });

    await launch(nested);
    await prompt("NO_GIT_REPO_INPUT", 1);

    const workspaces = await query<{ discovery: string }>("SELECT discovery FROM workspaces");
    expect(workspaces, "登记后应恰好有一个工作区").toHaveLength(1);
    const discovery = JSON.parse(workspaces[0].discovery) as {
      root: string; common_dir: string | null; private_dir: string | null;
    };
    const expectedRoot = await realpath(nested);
    expect(
      discovery.root === expectedRoot
        ? "cwd"
        : `root=${discovery.root} repo=${await realpath(repository)}`,
      "Git 不可用时执行根应为 cwd 本身",
    ).toBe("cwd");
    expect(discovery.common_dir, "不得推断出未观测到的仓库布局").toBeNull();
    expect(discovery.private_dir).toBeNull();
    expect(await query("SELECT id FROM projects")).toHaveLength(1);
    expect(await query("SELECT thread_id FROM session_bindings")).toHaveLength(1);
  }, 180_000);
});
