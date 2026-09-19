/**
 * 慢 Git 的端到端：每次 Git 调用固定等待时，仓库目录仍能建会话并发送输入。
 *
 * 只使用本地 SSE 模型端点，无真实凭据、无外部 API。补上
 * `spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md` 修复记录第 11 条
 * 的遗留「慢响应只在 resources 层用假 Git 验证，没有走 TUI / ACP 路径」：
 * 验收条件第 7 项要求「慢准备不造成输入丢失」，这属于跨层结论，不能只由
 * 单进程单元测试代表。
 *
 * 前提守卫是本用例的关键：TmuxTester 的 `-e PATH=` 会被会话 shell 重置
 * （见 `workspace-no-git.test.ts`），PATH 必须由命令自身用 `env -i` 构造；
 * 并且用例先自行调用一次 Git 断言耗时确实包含注入的等待——否则脚本没被用上时
 * 用例会静默退化成普通 Git 路径，看着通过却什么都没覆盖。
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { createServer, type Server } from "node:http";
import { execFile, execFileSync } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT } from "../../helpers/peri.js";

const execFileAsync = promisify(execFile);
const MODEL = "workspace-slow-git-model";
/** 每次 Git 调用注入的固定等待；实测耗时以它为单位，不用静态预算推算。 */
const SLEEP_MS = 300;
const GIT = /^git/;

/** 一次准入会执行的三条发现命令，用来从调用日志里挑出发现自身的调用。 */
const DISCOVERY_CALLS = ["rev-parse --is-inside-work-tree", "rev-parse --show-toplevel", "worktree list"];
/** 准备阶段的状态栏提示（`statusbar-preparing`，本用例语言为 en）。不含省略号：命中判定不与折行耦合。 */
const PREPARING_HINT = "Preparing session";
/** 版本相关选项：发现请求它们会在旧版 Git 上失败（修复记录第 7、11 条）。 */
const VERSION_DEPENDENT = ["--path-format", "--git-common-dir", "--absolute-git-dir"];

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

describe("慢 Git 的工作区发现", () => {
  let directory: string;
  let home: string;
  let work: string;
  let shim: string;
  let log: string;
  let settings: string;
  let database: string;
  let tester: TmuxTester | undefined;
  let server: Server | undefined;
  let replies: number;

  /**
   * 建立「除 `git` 外与系统一致」的 PATH 目录，再把 `git` 换成一个先记录、
   * 再固定等待、最后把参数原样转交真实 Git 的脚本。
   */
  async function buildSlowGitPath(): Promise<string> {
    const bin = path.join(directory, "path-with-slow-git");
    await mkdir(bin);
    for (const source of ["/usr/bin", "/bin"]) {
      for (const name of await readdir(source)) {
        if (GIT.test(name)) continue;
        await symlink(path.join(source, name), path.join(bin, name)).catch(() => {});
      }
    }
    await symlink(process.execPath, path.join(bin, "node")).catch(() => {});
    const real = execFileSync("/bin/sh", ["-c", "command -v git"], { encoding: "utf8" }).trim();
    await writeFile(
      path.join(bin, "git"),
      `#!/bin/sh\nprintf '%s\\n' "$*" >> ${shellQuote(log)}\n/bin/sleep ${SLEEP_MS / 1000}\nexec ${shellQuote(real)} "$@"\n`,
      { mode: 0o755 },
    );
    return bin;
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
    directory = await mkdtemp(path.join(os.tmpdir(), "peri-workspace-slow-git-"));
    home = path.join(directory, "home");
    work = path.join(directory, "slow-repository");
    log = path.join(directory, "git-calls.log");
    settings = path.join(home, ".peri", "settings.json");
    database = path.join(home, ".peri", "threads", "threads.db");
    // 隔离 HOME 时提供空 `~/.cargo/env`，避免用户 shell rc source 失败。
    await mkdir(path.join(home, ".cargo"), { recursive: true });
    await writeFile(path.join(home, ".cargo", "env"), "");
    await mkdir(path.dirname(settings), { recursive: true });
    await mkdir(work, { recursive: true });
    await execFileAsync("git", ["init", "-q"], { cwd: work });
    await writeFile(log, "");
    shim = await buildSlowGitPath();
    replies = 0;
    server = createServer(async (request, response) => {
      for await (const bytes of request) void bytes;
      replies += 1;
      const send = (delta: object, finish: string | null = null) => {
        response.write(`data: ${JSON.stringify({
          id: "workspace-slow-git", object: "chat.completion.chunk", created: 1, model: MODEL,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      };
      response.writeHead(200, { "content-type": "text/event-stream" });
      send({ role: "assistant", content: `WORKSPACE_SLOW_GIT_REPLY_${replies}` });
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
   * 用 `env -i` 显式构造进程环境：PATH 只有 shim 目录，其中的 `git` 是慢脚本。
   * 与无 Git 用例同理，靠 tmux 传环境会被会话 shell 覆盖。
   */
  async function launch(): Promise<void> {
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
      command: ["/bin/sh", "-c", invocation].map(shellQuote),
      cwd: work,
      size: { cols: 120, rows: 40 },
      env: { HOME: home },
    });
    await tester.start();
    const launched = Date.now();
    // 准备窗口的可见状态（简化目标第 4 项后半）：会话建立期间状态栏必须说明
    // 「正在准备会话」。建立由启动期 `ensure_session` 发起：用户若在这段窗口里提交
    // 首次输入，等待的就是这一次建立——用例在本文件下方等 Git 安静后再提交，
    // 那时会话已可用、输入不再等待，所以窗口只能在这里观测。
    // 两个等待并发：界面先画出欢迎语时提示也已在同一帧上，不能因为等欢迎语而错过窗口。
    let visibleSince = 0;
    await Promise.all([
      tester.waitForText("AI operating system", { timeout: 20_000, interval: 100 }),
      tester.waitForText(PREPARING_HINT, { timeout: 20_000, interval: 50 }).then(() => {
        visibleSince = Date.now();
      }),
    ]);
    // 提示必须覆盖整段建立过程，而不是恰好被轮询抓到的一帧：慢 Git 的三条发现
    // 命令都在建立期间执行，窗口下限就是注入的等待——短于它说明提示提前收尾了。
    const deadline = Date.now() + 10_000;
    while ((await tester.getScreenText()).includes(PREPARING_HINT)) {
      if (Date.now() > deadline) throw new Error("准备提示在会话建立后仍然可见");
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    const visibleFor = Date.now() - visibleSince;
    console.log(
      `准备窗口实测：状态栏提示自 ${visibleSince - launched}ms 起可见 ${visibleFor}ms（注入等待每次 ${SLEEP_MS}ms）`,
    );
    expect(
      visibleFor,
      `准备提示只可见 ${visibleFor}ms，短于注入的 ${SLEEP_MS}ms 等待：建立期间的状态不完整`,
    ).toBeGreaterThanOrEqual(SLEEP_MS);
  }

  /** 调用日志的当前字节长度，作为观测窗口的起点。 */
  async function logOffset(): Promise<number> {
    return (await stat(log)).size;
  }

  /** 窗口内新增的调用行。 */
  async function callsSince(offset: number): Promise<string[]> {
    const content = await readFile(log, "utf8");
    return content.slice(offset).split("\n").filter((line) => line.trim() !== "");
  }

  /** 等到 Git 调用安静下来：启动期的发现与 git_watch 轮询都可能还在进行。 */
  async function waitForQuietGit(quietMs = 1_200, timeoutMs = 30_000): Promise<void> {
    const started = Date.now();
    let last = await logOffset();
    let stableSince = Date.now();
    while (Date.now() - started < timeoutMs) {
      await new Promise((resolve) => setTimeout(resolve, 200));
      const size = await logOffset();
      if (size !== last) {
        last = size;
        stableSince = Date.now();
      } else if (Date.now() - stableSince >= quietMs) {
        return;
      }
    }
    throw new Error("Git 调用未在预期时间内安静下来");
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

  it("仓库目录在每次 Git 调用都固定等待时仍能建会话并发送输入", async () => {
    // 前提守卫：这个 PATH 必须真的用到慢脚本，否则用例会静默退化成正常 Git 路径。
    const guardStarted = Date.now();
    const guard = await execFileAsync(
      "/bin/sh",
      ["-c", `command -v git && git -C ${shellQuote(work)} rev-parse --is-inside-work-tree`],
      { env: { PATH: shim, HOME: home } },
    );
    const guardElapsed = Date.now() - guardStarted;
    expect(guard.stdout).toContain("true");
    expect(
      guardElapsed,
      `守卫调用耗时 ${guardElapsed}ms 没有包含注入的 ${SLEEP_MS}ms 等待：慢 Git 没有被用上`,
    ).toBeGreaterThanOrEqual(SLEEP_MS);

    await launch();
    await waitForQuietGit();

    // 慢响应必须真的落在观测窗口内：窗口起点之后仍有 Git 调用，耗时才有意义。
    const offset = await logOffset();
    const started = Date.now();
    await tester!.paste("SLOW_GIT_INPUT");
    await tester!.sendKey("enter");
    await tester!.waitForText(`WORKSPACE_SLOW_GIT_REPLY_1`, { timeout: 30_000, interval: 100 });
    const elapsed = Date.now() - started;
    const calls = await callsSince(offset);
    const discovery = calls.filter((line) => DISCOVERY_CALLS.some((call) => line.includes(call)));
    console.log(
      `慢 Git 端到端实测：输入到回复 ${elapsed}ms，窗口内 Git 调用 ${calls.length} 次（其中发现 ${discovery.length} 次），注入等待每次 ${SLEEP_MS}ms`,
    );

    const screen = await tester!.getScreenText();
    expect(screen, "慢 Git 不应被当成输入未被接收").not.toContain("Input was not accepted");
    expect(screen, "慢 Git 不应被当成会话无法建立").not.toContain("Session could not be established");
    // 准备状态只属于准备窗口：会话建立并受理后继续显示就是在说谎。
    expect(screen, "会话建立后不应继续显示准备状态").not.toContain(PREPARING_HINT);
    expect(discovery.length, "一次准入至少一条发现命令（每轮三条）").toBeGreaterThanOrEqual(3);
    // 上限与下限一样是契约：一次准入至多一轮完整发现（见「修复记录」第 14 条）。
    // 窗口里多出的轮次只可能来自同一个目录被重复解析——准入内的重复检查或
    // 调用方按目录重新解析，都会把慢 Git 的等待成倍叠加到这次输入上。
    expect(
      discovery.length,
      `一次准入至多一轮发现（每轮三条命令）：窗口内发现 ${discovery.length} 次、`
        + `调用 ${calls.length} 次，说明同一个目录被重复解析`,
    ).toBeLessThanOrEqual(3);
    expect(elapsed, "窗口内必须有真实的 Git 等待").toBeGreaterThanOrEqual(SLEEP_MS);
    for (const option of VERSION_DEPENDENT) {
      expect(
        calls.filter((line) => line.includes(option)),
        `发现不得请求旧版 Git 不认识的 ${option}`,
      ).toEqual([]);
    }

    // 慢 Git 仍然是 Git：观测是仓库模式，不是降级出来的目录模式。
    const workspaces = await query<{ discovery: string }>("SELECT discovery FROM workspaces");
    expect(workspaces, "登记后应恰好有一个工作区").toHaveLength(1);
    const observed = JSON.parse(workspaces[0].discovery) as {
      root: string; common_dir: string | null; private_dir: string | null;
    };
    expect(observed.common_dir, "慢 Git 的仓库布局必须被识别").not.toBeNull();
    expect(observed.private_dir).not.toBeNull();
    expect(observed.root.endsWith("slow-repository"), `观测根应是仓库根：${observed.root}`).toBe(true);
    expect(await query("SELECT id FROM projects")).toHaveLength(1);
    expect(await query("SELECT thread_id FROM session_bindings")).toHaveLength(1);

    // 慢准备不得造成重复执行：一次输入对应一次模型请求。
    expect(replies, "一次输入只应产生一次模型请求").toBe(1);
    const counts = await query<{ role: string; n: number }>(
      "SELECT role, COUNT(*) AS n FROM messages GROUP BY role ORDER BY role",
    );
    expect(counts).toEqual([
      { role: "assistant", n: 1 },
      { role: "user", n: 1 },
    ]);
  }, 180_000);
});
