// ============ GraphService 测试 ============
import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";

import { GraphService } from "../src/services/graph-service.js";
import { makeGitRepo, writeTestFile } from "./helpers.js";

let docsDir: string;

beforeEach(async () => {
  docsDir = await makeGitRepo();
});

describe("GraphService", () => {
  describe("getGraph", () => {
    it("should return hasRepo false for non-git directory", async () => {
      const { makeTempDir } = await import("./helpers.js");
      const nonGitDir = await makeTempDir("graph-no-git-");
      const svc = new GraphService(nonGitDir);
      const result = await svc.getGraph();
      assert.equal(result.hasRepo, false);
    });

    it("should return empty commits for fresh repo", async () => {
      const svc = new GraphService(docsDir);
      const result = await svc.getGraph();
      assert.ok(result.hasRepo);
      // No commits yet, but still hasRepo=true
      assert.ok(Array.isArray(result.commits));
    });

    it("should list commits in topo order", async () => {
      await writeTestFile(docsDir, "a.txt", "aaa");
      const svc = new GraphService(docsDir);
      // 不能直接用 ScmService，所以用 execGit helper
      // 但为隔离起见，直接在这里写文件和做 git 操作
      const { execSync } = await import("node:child_process");
      execSync('git add a.txt && git commit -m "first"', { cwd: docsDir, stdio: "pipe" });
      execSync('git branch feature', { cwd: docsDir, stdio: "pipe" });
      execSync('git checkout feature', { cwd: docsDir, stdio: "pipe" });

      await writeTestFile(docsDir, "b.txt", "bbb");
      execSync('git add b.txt && git commit -m "second on feature"', { cwd: docsDir, stdio: "pipe" });

      await writeTestFile(docsDir, "c.txt", "ccc");
      execSync('git add c.txt && git commit -m "third on feature"', { cwd: docsDir, stdio: "pipe" });

      const result = await svc.getGraph();
      assert.ok(result.hasRepo);
      assert.ok(result.commits.length >= 3, `expected at least 3 commits, got ${result.commits.length}`);
      // "third on feature" should be first (most recent)
      assert.equal(result.commits[0].subject, "third on feature");
      assert.ok(result.commits[0].hash.length >= 7, "hash should be at least 7 chars");
    });

    it("should mark HEAD commit correctly", async () => {
      await writeTestFile(docsDir, "x.txt", "x");
      const { execSync } = await import("node:child_process");
      execSync('git add x.txt && git commit -m "the commit"', { cwd: docsDir, stdio: "pipe" });

      const svc = new GraphService(docsDir);
      const result = await svc.getGraph();
      const headCommit = result.commits.find((c: any) => c.head === true);
      assert.ok(headCommit, "should have a HEAD commit");
    });

    it("should include remotes list", async () => {
      await writeTestFile(docsDir, "init.txt", "init");
      const { execSync } = await import("node:child_process");
      execSync('git add init.txt && git commit -m "init"', { cwd: docsDir, stdio: "pipe" });
      execSync('git remote add origin https://example.com/repo.git', { cwd: docsDir, stdio: "pipe" });

      const svc = new GraphService(docsDir);
      const result = await svc.getGraph();
      assert.ok(result.hasRepo);
      assert.ok(result.remotes.includes("origin"));
    });
  });

  describe("getCommitDiff", () => {
    it("should return error for missing hash", async () => {
      const svc = new GraphService(docsDir);
      const result = await svc.getCommitDiff("");
      assert.ok("error" in result);
      assert.ok(result.error.includes("缺少"));
    });

    it("should return error for invalid hash format", async () => {
      const svc = new GraphService(docsDir);
      const result = await svc.getCommitDiff("ZZZ123");
      assert.ok("error" in result);
      assert.ok(result.error.includes("无效"));
    });

    it("should return diff for valid commit", async () => {
      await writeTestFile(docsDir, "d.txt", "line1\nline2\n");
      const { execSync } = await import("node:child_process");
      execSync('git add d.txt && git commit -m "add d.txt"', { cwd: docsDir, stdio: "pipe" });

      const svc = new GraphService(docsDir);
      // 获取完整 hash
      const hashOut = execSync("git rev-parse HEAD", { cwd: docsDir, encoding: "utf-8" }).trim();
      const shortHash = hashOut.slice(0, 7);

      const result = await svc.getCommitDiff(shortHash);
      assert.ok(!("error" in result));
      assert.ok(result.diff.includes("d.txt"), "diff should reference d.txt");
      assert.ok(result.author === "Test User", "author should be Test User");
    });
  });
});
