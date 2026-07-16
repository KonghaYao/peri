// ============ 测试辅助：tempdir / git repo / 文件写入 ============
import { mkdtemp, writeFile, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execSync } from "node:child_process";

/** 创建临时目录，返回绝对路径 */
export async function makeTempDir(prefix = "site-test-"): Promise<string> {
  return mkdtemp(join(tmpdir(), prefix));
}

/** 在临时目录中创建并初始化 git 仓库，返回绝对路径 */
export async function makeGitRepo(prefix = "site-git-test-"): Promise<string> {
  const dir = await makeTempDir(prefix);
  execSync("git init", { cwd: dir, stdio: "pipe" });
  execSync('git config user.email "test@example.com"', { cwd: dir, stdio: "pipe" });
  execSync('git config user.name "Test User"', { cwd: dir, stdio: "pipe" });
  return dir;
}

/** 在指定目录下创建文件（支持嵌套目录自动创建） */
export async function writeTestFile(dir: string, relativePath: string, content: string): Promise<string> {
  const fullPath = join(dir, relativePath);
  const dirPart = fullPath.substring(0, fullPath.lastIndexOf("/"));
  await mkdir(dirPart, { recursive: true });
  await writeFile(fullPath, content, "utf-8");
  return fullPath;
}
