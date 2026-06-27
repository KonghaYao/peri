// ============ SCM 路由：git 操作 API ============
import type { Hono } from "hono";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { execGit, parsePorcelainStatus } from "../lib/git.js";

export function registerScmRoutes(app: Hono, docsDir: string) {
  // 检测是否为 git 仓库
  app.get("/api/scm/detect", async (c) => {
    try {
      const { exitCode } = await execGit(docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return c.json({ hasRepo: false });
      const { stdout, exitCode: refExitCode } = await execGit(docsDir, ["rev-parse", "--abbrev-ref", "HEAD"]);
      const rawBranch = refExitCode === 0 ? stdout : null;
      const branch = (rawBranch && rawBranch !== "HEAD") ? rawBranch : null;
      return c.json({ hasRepo: true, branch });
    } catch (err: any) {
      return c.json({ hasRepo: false, error: err.message }, 500);
    }
  });

  // SCM 摘要
  app.get("/api/scm/summary", async (c) => {
    try {
      const { exitCode } = await execGit(docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return c.json({ hasRepo: false });
      const { stdout, exitCode: statusExitCode } = await execGit(docsDir, ["status", "--porcelain=v1", "--branch"]);
      if (statusExitCode !== 0) {
        return c.json({ hasRepo: true, error: stdout, branch: "", ahead: 0, behind: 0, added: 0, modified: 0, deleted: 0, totalEntries: 0 });
      }
      const status = parsePorcelainStatus(stdout);
      const added = status.staged.filter(f => f.index === "A").length + status.unstaged.filter(f => f.worktree === "?").length;
      const modified = status.staged.filter(f => f.index === "M").length + status.unstaged.filter(f => f.worktree === "M").length;
      const deleted = status.staged.filter(f => f.index === "D").length + status.unstaged.filter(f => f.worktree === "D").length;
      return c.json({ hasRepo: true, branch: status.branch, ahead: status.ahead, behind: status.behind, totalEntries: status.staged.length + status.unstaged.length, added, modified, deleted });
    } catch (err: any) {
      return c.json({ hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, added: 0, modified: 0, deleted: 0, totalEntries: 0 }, 500);
    }
  });

  // SCM 全量状态
  app.get("/api/scm/status", async (c) => {
    try {
      const { exitCode } = await execGit(docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return c.json({ hasRepo: false });
      const { stdout, exitCode: statusExitCode } = await execGit(docsDir, ["status", "--porcelain=v1", "--branch"]);
      if (statusExitCode !== 0) return c.json({ hasRepo: true, error: stdout, branch: "", ahead: 0, behind: 0, staged: [], unstaged: [] });
      const status = parsePorcelainStatus(stdout);
      return c.json({ hasRepo: true, branch: status.branch, ahead: status.ahead, behind: status.behind, staged: status.staged, unstaged: status.unstaged });
    } catch (err: any) {
      return c.json({ hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, staged: [], unstaged: [] }, 500);
    }
  });

  // 文件 diff
  app.get("/api/scm/diff", async (c) => {
    try {
      const filePath = c.req.query("file");
      const staged = c.req.query("staged") === "true";
      if (!filePath) return c.json({ error: "缺少 file 参数" }, 400);
      const fullPath = join(docsDir, filePath);
      if (!fullPath.startsWith(docsDir)) return c.json({ error: "非法路径" }, 403);
      const args = staged ? ["diff", "--cached", "--", filePath] : ["diff", "--", filePath];
      const { stdout } = await execGit(docsDir, args);
      if (!stdout) {
        try {
          const content = await readFile(fullPath, "utf-8");
          const lines = content.split("\n");
          const diff = [`diff --git a/${filePath} b/${filePath}`, `new file mode 100644`, `index 0000000..0000000`, `--- /dev/null`, `+++ b/${filePath}`, `@@ -0,0 +1,${lines.length} @@`, ...lines.map(line => `+${line}`)].join("\n") + "\n";
          return c.json({ diff });
        } catch { return c.json({ diff: "" }); }
      }
      return c.json({ diff: stdout });
    } catch (err: any) {
      return c.json({ diff: "", error: err.message }, 500);
    }
  });

  // stage / unstage
  app.post("/api/scm/stage", async (c) => {
    try {
      const body = await c.req.json();
      const files: string[] = body.files || [];
      const toStage: boolean = body.staged !== false;
      if (files.length === 0) return c.json({ error: "缺少 files 参数" }, 400);
      for (const f of files) {
        const fullPath = join(docsDir, f);
        if (!fullPath.startsWith(docsDir)) return c.json({ error: `非法路径: ${f}` }, 403);
      }
      const args = toStage ? ["add", ...files] : ["reset", "HEAD", "--", ...files];
      const { stderr, exitCode } = await execGit(docsDir, args);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 提交
  app.post("/api/scm/commit", async (c) => {
    try {
      const body = await c.req.json();
      const message = (body.message || "").trim();
      if (!message) return c.json({ success: false, error: "缺少 commit message" }, 400);
      const { stdout, stderr, exitCode } = await execGit(docsDir, ["commit", "-m", message]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      const hashMatch = stdout.match(/\[[\w\-.]+\s+([a-f0-9]+)\]/);
      return c.json({ success: true, hash: hashMatch ? hashMatch[1] : null });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 丢弃变更
  app.post("/api/scm/discard", async (c) => {
    try {
      const body = await c.req.json();
      const files: string[] = body.files || [];
      if (files.length === 0) return c.json({ error: "缺少 files 参数" }, 400);
      for (const f of files) {
        const fullPath = join(docsDir, f);
        if (!fullPath.startsWith(docsDir)) return c.json({ error: `非法路径: ${f}` }, 403);
      }
      const { stderr, exitCode } = await execGit(docsDir, ["checkout", "--", ...files]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 分支列表
  app.get("/api/scm/branches", async (c) => {
    try {
      const { stdout, exitCode } = await execGit(docsDir, ["branch"]);
      if (exitCode !== 0) return c.json({ error: stdout }, 500);
      const branches = stdout.split("\n").map(line => {
        const name = line.replace(/^\*?\s+/, "").trim();
        return { name, current: line.startsWith("*"), remote: false };
      }).filter(b => b.name);
      try {
        const rbRes = await execGit(docsDir, ["branch", "-r"]);
        if (rbRes.stdout) {
          const remoteBranches = rbRes.stdout.split("\n").map(line => {
            const name = line.trim().replace(/^\*\s*/, "").replace(/\s*->.*$/, "").trim();
            return { name, current: false, remote: true };
          }).filter(b => b.name && b.name.indexOf("/HEAD") === -1);
          for (const rb of remoteBranches) {
            if (!branches.find(b => b.name === rb.name.replace(/^[^/]+\//, ""))) branches.push(rb);
          }
        }
      } catch (_) {}
      const currentBranch = branches.find(b => b.current)?.name || "";
      return c.json({ branches, current: currentBranch });
    } catch (err: any) {
      return c.json({ error: err.message, branches: [], current: "" }, 500);
    }
  });

  // 切换分支
  app.post("/api/scm/branch", async (c) => {
    try {
      const body = await c.req.json();
      const branch = (body.branch || "").trim();
      if (!branch) return c.json({ success: false, error: "缺少 branch 参数" }, 400);
      const { stderr, exitCode } = await execGit(docsDir, ["checkout", branch]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Checkout 远程分支
  app.post("/api/scm/checkout-remote", async (c) => {
    try {
      const body = await c.req.json();
      const remoteBranch = (body.branch || "").trim();
      if (!remoteBranch) return c.json({ success: false, error: "缺少 branch 参数" }, 400);
      const parts = remoteBranch.split("/");
      const localName = parts.slice(1).join("/");
      const { stderr, exitCode } = await execGit(docsDir, ["checkout", "-b", localName, remoteBranch]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true, branch: localName });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 创建 tag
  app.post("/api/scm/tag", async (c) => {
    try {
      const body = await c.req.json();
      const name = (body.name || "").trim();
      const hash = (body.hash || "").trim();
      if (!name) return c.json({ success: false, error: "缺少 tag 名称" }, 400);
      if (!hash || !/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ success: false, error: "无效的 hash" }, 400);
      const args = ["tag", name, hash];
      if (body.message) args.splice(1, 0, "-m", body.message);
      const { stderr, exitCode } = await execGit(docsDir, args);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 从 commit 创建分支
  app.post("/api/scm/create-branch", async (c) => {
    try {
      const body = await c.req.json();
      const name = (body.name || "").trim();
      const hash = (body.hash || "").trim();
      if (!name) return c.json({ success: false, error: "缺少分支名称" }, 400);
      if (!hash || !/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ success: false, error: "无效的 hash" }, 400);
      const { stderr, exitCode } = await execGit(docsDir, ["branch", name, hash]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Cherry-pick
  app.post("/api/scm/cherry-pick", async (c) => {
    try {
      const body = await c.req.json();
      const hash = (body.hash || "").trim();
      if (!hash || !/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ success: false, error: "无效的 hash" }, 400);
      const { stderr, exitCode } = await execGit(docsDir, ["cherry-pick", hash]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Revert
  app.post("/api/scm/revert", async (c) => {
    try {
      const body = await c.req.json();
      const hash = (body.hash || "").trim();
      if (!hash || !/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ success: false, error: "无效的 hash" }, 400);
      const { stderr, exitCode } = await execGit(docsDir, ["revert", "--no-edit", hash]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Reset
  app.post("/api/scm/reset", async (c) => {
    try {
      const body = await c.req.json();
      const hash = (body.hash || "").trim();
      const mode = (body.mode || "mixed").trim();
      if (!hash || !/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ success: false, error: "无效的 hash" }, 400);
      const flag = mode === "hard" ? "--hard" : mode === "soft" ? "--soft" : "--mixed";
      const { stderr, exitCode } = await execGit(docsDir, ["reset", flag, hash]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Merge
  app.post("/api/scm/merge", async (c) => {
    try {
      const body = await c.req.json();
      const branch = (body.branch || "").trim();
      if (!branch) return c.json({ success: false, error: "缺少 branch 参数" }, 400);
      const { stderr, exitCode } = await execGit(docsDir, ["merge", branch]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Fetch
  app.post("/api/scm/fetch", async (c) => {
    try {
      const body = await c.req.json();
      const remote = (body.remote || "").trim();
      const args = ["fetch"];
      if (remote) args.push(remote);
      args.push("--prune");
      const { stderr, exitCode } = await execGit(docsDir, args);
      if (exitCode !== 0) return c.json({ success: false, error: stderr || "Fetch failed" }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Pull
  app.post("/api/scm/pull", async (c) => {
    try {
      const body = await c.req.json();
      const remote = (body.remote || "").trim();
      const branch = (body.branch || "").trim();
      const args = ["pull"];
      if (remote) args.push(remote);
      if (branch) args.push(branch);
      const { stderr, exitCode } = await execGit(docsDir, args);
      if (exitCode !== 0) return c.json({ success: false, error: stderr || "Pull failed" }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // Push
  app.post("/api/scm/push", async (c) => {
    try {
      const body = await c.req.json();
      const remote = (body.remote || "").trim();
      const branch = (body.branch || "").trim();
      const args = ["push"];
      if (remote) args.push(remote);
      if (branch) args.push(branch);
      const { stderr, exitCode } = await execGit(docsDir, args);
      if (exitCode !== 0) return c.json({ success: false, error: stderr || "Push failed" }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });

  // 删除远程分支
  app.post("/api/scm/delete-remote-branch", async (c) => {
    try {
      const body = await c.req.json();
      const branch = (body.branch || "").trim();
      if (!branch) return c.json({ success: false, error: "缺少 branch 参数" }, 400);
      const parts = branch.split("/");
      if (parts.length < 2) return c.json({ success: false, error: "无效的远程分支名" }, 400);
      const remote = parts[0];
      const branchName = parts.slice(1).join("/");
      const { stderr, exitCode } = await execGit(docsDir, ["push", remote, "--delete", branchName]);
      if (exitCode !== 0) return c.json({ success: false, error: stderr || "Delete failed" }, 500);
      return c.json({ success: true });
    } catch (err: any) {
      return c.json({ success: false, error: err.message }, 400);
    }
  });
}
