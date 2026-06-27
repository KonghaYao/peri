// ============ Git Graph + Commit Diff 路由 ============
import type { Hono } from "hono";
import { execGit } from "../lib/git.js";
import type { GraphCommit } from "../types.js";

export function registerGraphRoutes(app: Hono, docsDir: string) {
  // Git Graph 提交图数据（自建布局引擎，不用 git log --graph）
  app.get("/api/scm/graph", async (c) => {
    try {
      const { exitCode } = await execGit(docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return c.json({ hasRepo: false });

      const maxCommits = parseInt(c.req.query("max") || "200", 10);

      const headRes = await execGit(docsDir, ["rev-parse", "HEAD"]);
      const headHash = headRes.stdout ? headRes.stdout.trim() : "";

      const remoteRes = await execGit(docsDir, ["remote"]);
      const remotes: string[] = remoteRes.stdout ? remoteRes.stdout.split("\n").filter(Boolean) : [];

      let remoteBranches: string[] = [];
      try {
        const rbRes = await execGit(docsDir, ["branch", "-r"]);
        if (rbRes.stdout) {
          remoteBranches = rbRes.stdout.split("\n")
            .map(l => l.trim().replace(/^\*\s*/, ""))
            .map(l => l.replace(/\s*->.*$/, "").trim())
            .filter(Boolean);
        }
      } catch (_) {}

      const { stdout, exitCode: logExitCode } = await execGit(docsDir, [
        "log", "--all",
        "--format=%H|%h|%P|%an|%ad|%s|%D",
        "--date=short", "--topo-order", "-n", String(maxCommits + 1),
      ]);
      if (logExitCode !== 0) return c.json({ error: stdout }, 500);

      const commits: GraphCommit[] = [];
      const lines = stdout.split("\n").map(l => l.trim()).filter(Boolean);
      const moreAvailable = lines.length > maxCommits;
      const displayLines = moreAvailable ? lines.slice(0, maxCommits) : lines;

      for (const line of displayLines) {
        const parts = line.split("|");
        commits.push({
          hash: parts[0] || "",
          shortHash: parts[1] || "",
          parents: parts[2] ? parts[2].split(" ").filter(Boolean) : [],
          author: parts[3] || "",
          date: parts[4] || "",
          subject: parts[5] || "",
          refs: parts[6] ? parts[6].split(",").map(s => s.trim()).filter(Boolean) : [],
          head: parts[0] === headHash,
        });
      }

      // 合成未提交变更行
      if (commits.length > 0) {
        try {
          const statusRes = await execGit(docsDir, ["status", "--porcelain"]);
          if (statusRes.stdout) {
            const statusLines = statusRes.stdout.split("\n").filter(Boolean);
            const files = statusLines.map(l => l.slice(3).trim());
            const statRes = await execGit(docsDir, ["diff", "--stat", "HEAD"]);
            const statMatch = statRes.stdout.match(/(\d+) files? changed.*?(\d+) insertions?.*?(\d+) deletions?/);
            commits.unshift({
              hash: "UNCOMMITTED", shortHash: "",
              parents: commits[0].hash ? [commits[0].hash] : [],
              author: "", date: "", refs: [], head: false,
              subject: `${files.length} changed file${files.length !== 1 ? 's' : ''}`,
              uncommitted: true,
              uncommittedFiles: files.slice(0, 20),
              uncommittedStats: statMatch ? `${statMatch[2] || 0} insertions, ${statMatch[3] || 0} deletions` : "",
            });
          }
        } catch (_) {}
      }

      // 合成 Stash 条目
      try {
        const stashListRes = await execGit(docsDir, ["stash", "list"]);
        if (stashListRes.stdout) {
          const stashLines = stashListRes.stdout.split("\n").filter(Boolean);
          const stashCommits: GraphCommit[] = [];
          for (let i = stashLines.length - 1; i >= 0; i--) {
            const line = stashLines[i];
            const stashIndex = stashLines.length - 1 - i;
            try {
              const stashRef = `stash@{${stashIndex}}`;
              const showRes = await execGit(docsDir, ["show", "-s", "--format=%H|%h|%P|%an|%ad|%s", "--date=short", stashRef]);
              if (showRes.stdout) {
                const parts = showRes.stdout.split("|");
                const subject = parts[5] || line.replace(/^stash@\{\d+\}:\s*/, "");
                stashCommits.push({
                  hash: parts[0] || stashRef,
                  shortHash: parts[1] || parts[0]?.slice(0, 7) || stashRef,
                  parents: [],
                  author: parts[3] || "",
                  date: parts[4] || "",
                  subject: subject.length > 100 ? subject.slice(0, 100) + "..." : subject,
                  refs: [`stash@{${stashIndex}}`],
                  head: false, stash: true, stashIndex,
                });
              }
            } catch (_) {}
          }
          commits.push(...stashCommits);
        }
      } catch (_) {}

      return c.json({ hasRepo: true, commits, remotes, remoteBranches, moreAvailable });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  // 查看 commit diff
  app.get("/api/scm/commit-diff", async (c) => {
    try {
      const hash = c.req.query("hash");
      if (!hash) return c.json({ error: "缺少 hash 参数" }, 400);
      if (!/^[a-f0-9]{7,40}$/.test(hash)) return c.json({ error: "无效的 hash" }, 400);
      const metaRes = await execGit(docsDir, ["show", "--format=%an|%ad|%s", "--date=short", "--no-patch", hash]);
      const metaParts = metaRes.stdout.split("|");
      const diffRes = await execGit(docsDir, ["show", "--format=", hash]);
      return c.json({
        author: metaParts[0] || "",
        date: metaParts[1] || "",
        subject: metaParts[2] || "",
        diff: diffRes.stdout,
      });
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });
}
