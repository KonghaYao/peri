// ============ SCM: git 命令执行 + porcelain 解析 ============
import { spawn } from "node:child_process";
import type { GitResult, ScmStatus, ScmFile } from "../types.js";

export function execGit(docsDir: string, args: string[]): Promise<GitResult> {
  return new Promise((resolve) => {
    const proc = spawn("git", ["-C", docsDir, ...args], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    proc.stdout?.on("data", (chunk: Buffer) => { stdout += chunk.toString(); });
    proc.stderr?.on("data", (chunk: Buffer) => { stderr += chunk.toString(); });
    proc.on("close", (exitCode) => {
      resolve({ stdout: stdout.trimEnd(), stderr: stderr.trimEnd(), exitCode: exitCode ?? 0 });
    });
  });
}

export function parsePorcelainStatus(stdout: string): ScmStatus {
  let branch = "";
  let ahead = 0;
  let behind = 0;
  const staged: ScmFile[] = [];
  const unstaged: ScmFile[] = [];

  for (const rawLine of stdout.split("\n")) {
    const line = rawLine.trimEnd();
    if (!line) continue;

    if (line.startsWith("## ")) {
      const parts = line.slice(3).split(" ");
      const head = parts[0];
      if (head !== "No" && head !== "Initial") {
        const dotdot = head.indexOf("...");
        branch = dotdot >= 0 ? head.slice(0, dotdot) : head;
      }
      const rest = parts.slice(1).join(" ");
      const aheadMatch = rest.match(/ahead\s+(\d+)/);
      const behindMatch = rest.match(/behind\s+(\d+)/);
      if (aheadMatch) ahead = parseInt(aheadMatch[1]);
      if (behindMatch) behind = parseInt(behindMatch[1]);
      continue;
    }

    const index = line[0] || " ";
    const worktree = line[1] || " ";
    let filePath = line.slice(3);

    // 处理重命名 (R + "original -> new")
    const arrowIdx = filePath.indexOf(" -> ");
    if (arrowIdx >= 0) filePath = filePath.slice(arrowIdx + 4);

    // 去掉引号包裹
    if (filePath.startsWith('"') && filePath.endsWith('"')) {
      filePath = JSON.parse(filePath);
    }

    const entry: ScmFile = { path: filePath, index, worktree };

    if (index !== " " && index !== "?") {
      staged.push(entry);
    }
    if (worktree !== " ") {
      unstaged.push(entry);
    }
  }

  return { branch, ahead, behind, staged, unstaged };
}
