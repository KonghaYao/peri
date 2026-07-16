// ============ ScmService 测试 ============
import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";

import { ScmService } from "../src/services/scm-service.js";
import { makeGitRepo, writeTestFile } from "./helpers.js";

let docsDir: string;

beforeEach(async () => {
  docsDir = await makeGitRepo();
});

describe("ScmService", () => {
  describe("detect", () => {
    it("should detect git repo and return branch", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.detect();
      assert.equal(result.hasRepo, true);
      // Initial commit尚未创建，branch可能为 null 或 "main"/"master"
      assert.ok(result.branch === null || typeof result.branch === "string");
    });

    it("should return hasRepo false for non-git directory", async () => {
      const { makeTempDir } = await import("./helpers.js");
      const nonGitDir = await makeTempDir("scm-no-git-");
      const svc = new ScmService(nonGitDir);
      const result = await svc.detect();
      assert.equal(result.hasRepo, false);
    });
  });

  describe("getStatus", () => {
    it("should return hasRepo false for non-git directory", async () => {
      const { makeTempDir } = await import("./helpers.js");
      const nonGitDir = await makeTempDir("scm-status-no-git-");
      const svc = new ScmService(nonGitDir);
      const result = await svc.getStatus();
      assert.equal(result.hasRepo, false);
    });
  });

  describe("stage and commit workflow", () => {
    it("should stage a file and commit", async () => {
      await writeTestFile(docsDir, "hello.txt", "hello world");
      const svc = new ScmService(docsDir);

      // 初始提交
      let stageResult = await svc.stage(["hello.txt"], true);
      assert.equal(stageResult.success, true);

      let commitResult = await svc.commit("initial commit");
      assert.equal(commitResult.success, true);
      // 回归测试：root-commit 输出格式 "[main (root-commit) abc123]"
      // 曾经的正则无法匹配，导致首次 commit hash 为 undefined，已修复
      assert.ok(commitResult.hash, "should return hash for root-commit (regression)");

      // 检查 status
      const status = await svc.getStatus();
      assert.equal(status.hasRepo, true);
      assert.ok(status.branch, "should have a branch after commit");
    });

    it("should fail commit with empty message", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.commit("");
      assert.equal(result.success, false);
      assert.ok(result.error.includes("不能为空"));
    });

    it("should fail commit with whitespace-only message", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.commit("   ");
      assert.equal(result.success, false);
    });

    it("should fail stage with no files", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.stage([], true);
      assert.equal(result.success, false);
      assert.ok(result.error.includes("没有指定"));
    });
  });

  describe("unstage", () => {
    it("should unstage a file", async () => {
      await writeTestFile(docsDir, "test.txt", "content");
      const svc = new ScmService(docsDir);

      await svc.stage(["test.txt"], true);
      await svc.commit("initial");
      await writeTestFile(docsDir, "test.txt", "modified");
      await svc.stage(["test.txt"], true);

      // unstage
      const result = await svc.stage(["test.txt"], false);
      assert.equal(result.success, true);
    });
  });

  describe("diff", () => {
    it("should return diff for modified file", async () => {
      await writeTestFile(docsDir, "a.txt", "line1\nline2\n");
      const svc = new ScmService(docsDir);
      await svc.stage(["a.txt"], true);
      await svc.commit("initial");
      await writeTestFile(docsDir, "a.txt", "line1\nmodified\nline2\n");

      const result = await svc.getDiff("a.txt", false);
      assert.ok(!result.error);
      assert.ok(result.diff.includes("modified"), "diff should contain the changed line");
    });

    it("should return empty diff for non-existent file with no git diff", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.getDiff("nonexistent.txt", false);
      // 未跟踪文件且无法读取，返回空 diff
      assert.ok(!result.error || result.diff === "");
    });

    it("should return error for path traversal", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.getDiff("../../etc/passwd", false);
      assert.ok("error" in result);
      assert.ok(result.error.includes("非法"));
    });
  });

  describe("stage path validation", () => {
    it("should reject path traversal in stage", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.stage(["../../etc/passwd"], true);
      assert.equal(result.success, false);
      assert.ok(result.error.includes("非法"));
    });
  });

  describe("discard", () => {
    it("should fail discard with no files", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.discard([]);
      assert.equal(result.success, false);
    });

    it("should reject path traversal in discard", async () => {
      const svc = new ScmService(docsDir);
      const result = await svc.discard(["../../etc/passwd"]);
      assert.equal(result.success, false);
      assert.ok(result.error.includes("非法"));
    });
  });

  describe("getSummary", () => {
    it("should return hasRepo false for non-git directory", async () => {
      const { makeTempDir } = await import("./helpers.js");
      const nonGitDir = await makeTempDir("scm-summary-no-git-");
      const svc = new ScmService(nonGitDir);
      const result = await svc.getSummary();
      assert.equal(result.hasRepo, false);
    });

    it("should count staged and unstaged files correctly", async () => {
      await writeTestFile(docsDir, "a.txt", "aaa");
      const svc = new ScmService(docsDir);

      await svc.stage(["a.txt"], true);
      await svc.commit("initial");

      // 修改 a.txt（modified unstaged）
      await writeTestFile(docsDir, "a.txt", "modified");
      // 新建 b.txt（untracked）
      await writeTestFile(docsDir, "b.txt", "bbb");
      // stage c.txt（newly staged）
      await writeTestFile(docsDir, "c.txt", "ccc");
      await svc.stage(["c.txt"], true);

      const summary = await svc.getSummary();
      assert.ok(summary.hasRepo);
      // b.txt is untracked, counted as added
      assert.ok(summary.added >= 1, "should have at least 1 added/untracked file");
      // a.txt is modified
      assert.ok(summary.modified >= 1, "should have at least 1 modified file");
    });
  });

  describe("branches", () => {
    it("should list branches after commit", async () => {
      await writeTestFile(docsDir, "init.txt", "init");
      const svc = new ScmService(docsDir);
      await svc.stage(["init.txt"], true);
      await svc.commit("init");

      const result = await svc.getBranches();
      assert.ok(!("error" in result) || result.error === undefined);
      assert.ok(result.branches.length >= 1, "should have at least one branch");
      const currentBranch = result.branches.find((b: any) => b.current === true);
      assert.ok(currentBranch, "should have a current branch");
    });

    it("should create a new branch", async () => {
      await writeTestFile(docsDir, "init.txt", "init");
      const svc = new ScmService(docsDir);
      await svc.stage(["init.txt"], true);
      await svc.commit("init");

      const createResult = await svc.createBranch("feature");
      assert.equal(createResult.success, true);

      const branches = await svc.getBranches();
      assert.ok(branches.branches.find((b: any) => b.name === "feature"));
    });
  });
});
