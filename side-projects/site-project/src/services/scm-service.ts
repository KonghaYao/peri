// ============ SCM 业务逻辑层 ============
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { execGit, parsePorcelainStatus } from "../lib/git.js";

export class ScmService {
  constructor(private docsDir: string) {}

  async detect() {
    try {
      const { exitCode } = await execGit(this.docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return { hasRepo: false };
      const { stdout, exitCode: refExitCode } = await execGit(this.docsDir, ["rev-parse", "--abbrev-ref", "HEAD"]);
      const rawBranch = refExitCode === 0 ? stdout : null;
      const branch = (rawBranch && rawBranch !== "HEAD") ? rawBranch : null;
      return { hasRepo: true, branch };
    } catch (err: any) {
      return { hasRepo: false, error: err.message };
    }
  }

  async getSummary() {
    try {
      const { exitCode } = await execGit(this.docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return { hasRepo: false };
      const { stdout, exitCode: statusExitCode } = await execGit(this.docsDir, ["status", "--porcelain=v1", "--branch"]);
      if (statusExitCode !== 0) {
        return { hasRepo: true, error: stdout, branch: "", ahead: 0, behind: 0, added: 0, modified: 0, deleted: 0, totalEntries: 0 };
      }
      const status = parsePorcelainStatus(stdout);
      const added = status.staged.filter((f: any) => f.index === "A").length + status.unstaged.filter((f: any) => f.worktree === "?").length;
      const modified = status.staged.filter((f: any) => f.index === "M").length + status.unstaged.filter((f: any) => f.worktree === "M").length;
      const deleted = status.staged.filter((f: any) => f.index === "D").length + status.unstaged.filter((f: any) => f.worktree === "D").length;
      return { hasRepo: true, branch: status.branch, ahead: status.ahead, behind: status.behind, totalEntries: status.staged.length + status.unstaged.length, added, modified, deleted };
    } catch (err: any) {
      return { hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, added: 0, modified: 0, deleted: 0, totalEntries: 0 };
    }
  }

  async getStatus() {
    try {
      const { exitCode } = await execGit(this.docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return { hasRepo: false };
      const { stdout, exitCode: statusExitCode } = await execGit(this.docsDir, ["status", "--porcelain=v1", "--branch"]);
      if (statusExitCode !== 0) return { hasRepo: true, error: stdout, branch: "", ahead: 0, behind: 0, staged: [], unstaged: [] };
      const status = parsePorcelainStatus(stdout);
      return { hasRepo: true, branch: status.branch, ahead: status.ahead, behind: status.behind, staged: status.staged, unstaged: status.unstaged };
    } catch (err: any) {
      return { hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, staged: [], unstaged: [] };
    }
  }

  async getDiff(filePath: string, staged: boolean) {
    try {
      if (!filePath) return { error: "缺少 file 参数" };
      const fullPath = join(this.docsDir, filePath);
      if (!fullPath.startsWith(this.docsDir)) return { error: "非法路径" };
      const args = staged ? ["diff", "--cached", "--", filePath] : ["diff", "--", filePath];
      const { stdout } = await execGit(this.docsDir, args);
      if (!stdout) {
        try {
          const content = await readFile(fullPath, "utf-8");
          const lines = content.split("\n");
          const diff = [`diff --git a/${filePath} b/${filePath}`, `new file mode 100644`, `index 0000000..0000000`, `--- /dev/null`, `+++ b/${filePath}`, `@@ -0,0 +1,${lines.length} @@`, ...lines.map(line => `+${line}`)].join("\n") + "\n";
          return { diff };
        } catch { return { diff: "" }; }
      }
      return { diff: stdout };
    } catch (err: any) {
      return { diff: "", error: err.message };
    }
  }

  async stage(files: string[], toStage: boolean) {
    try {
      if (!files || files.length === 0) return { success: false, error: "没有指定文件" };
      const fullPaths = files.map(f => join(this.docsDir, f));
      for (const p of fullPaths) {
        if (!p.startsWith(this.docsDir)) return { success: false, error: "非法路径" };
      }
      const args = toStage ? ["add", "--", ...files] : ["reset", "HEAD", "--", ...files];
      const { exitCode, stderr } = await execGit(this.docsDir, args);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async commit(message: string) {
    try {
      if (!message || !message.trim()) return { success: false, error: "提交信息不能为空" };
      const { exitCode, stdout, stderr } = await execGit(this.docsDir, ["commit", "-m", message]);
      if (exitCode !== 0) return { success: false, error: stderr || "提交失败" };
      // 兼容 root-commit 输出：[main (root-commit) abc1234]
      const hashMatch = stdout.match(/\[[\w-]+ (?:\(root-commit\) )?([a-f0-9]+)\]/);
      return { success: true, hash: hashMatch ? hashMatch[1] : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async discard(files: string[]) {
    try {
      if (!files || files.length === 0) return { success: false, error: "没有指定文件" };
      const fullPaths = files.map(f => join(this.docsDir, f));
      for (const p of fullPaths) {
        if (!p.startsWith(this.docsDir)) return { success: false, error: "非法路径" };
      }
      const { exitCode, stderr } = await execGit(this.docsDir, ["checkout", "--", ...files]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async getBranches() {
    try {
      const { exitCode } = await execGit(this.docsDir, ["rev-parse", "--is-inside-work-tree"]);
      if (exitCode !== 0) return { hasRepo: false };
      const { stdout: localOut } = await execGit(this.docsDir, ["branch", "--format=%(refname:short)\t%(HEAD)"]);
      const { stdout: remoteOut } = await execGit(this.docsDir, ["branch", "-r", "--format=%(refname:short)"]);
      const branches: Array<{ name: string; current: boolean; remote?: boolean }> = [];
      for (const line of localOut.trim().split("\n").filter(Boolean)) {
        const [name, headMarker] = line.split("\t");
        if (name) branches.push({ name, current: headMarker === "*" });
      }
      for (const line of remoteOut.trim().split("\n").filter(Boolean)) {
        const name = line.trim();
        if (name && !branches.find(b => b.name === name)) {
          branches.push({ name, current: false, remote: true });
        }
      }
      return { branches };
    } catch (err: any) {
      return { error: err.message, branches: [] };
    }
  }

  async switchBranch(name: string) {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["checkout", name]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async checkoutRemote(branch: string) {
    try {
      const localName = branch.startsWith("origin/") ? branch.slice(7) : branch;
      const { exitCode, stderr } = await execGit(this.docsDir, ["checkout", "-b", localName, branch]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async createTag(name: string, message?: string) {
    try {
      const args = message ? ["tag", "-a", name, "-m", message] : ["tag", name];
      const { exitCode, stderr } = await execGit(this.docsDir, args);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async createBranch(name: string) {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["branch", name]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async cherryPick(hash: string) {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["cherry-pick", hash]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async revert(hash: string) {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["revert", "--no-edit", hash]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async reset(hash: string, mode: "soft" | "mixed" | "hard") {
    try {
      const flag = mode === "soft" ? "--soft" : mode === "hard" ? "--hard" : "--mixed";
      const { exitCode, stderr } = await execGit(this.docsDir, ["reset", flag, hash]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async merge(branch: string) {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["merge", branch]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async fetch() {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["fetch"]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async pull() {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["pull"]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async push() {
    try {
      const { exitCode, stderr } = await execGit(this.docsDir, ["push"]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }

  async deleteRemoteBranch(branch: string) {
    try {
      const remote = "origin";
      const { exitCode, stderr } = await execGit(this.docsDir, ["push", remote, "--delete", branch]);
      return { success: exitCode === 0, error: exitCode !== 0 ? stderr : undefined };
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  }
}
