// ============ FileService 测试 ============
import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";

import { FileService } from "../src/services/file-service.js";
import { makeTempDir, writeTestFile } from "./helpers.js";

let docsDir: string;

beforeEach(async () => {
  docsDir = await makeTempDir("file-svc-");
});

describe("FileService", () => {
  describe("getTree", () => {
    it("should list empty directory", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getTree("");
      assert.ok(Array.isArray(result));
      assert.equal(result.length, 0);
    });

    it("should list files and directories sorted", async () => {
      await writeTestFile(docsDir, "z.txt", "last");
      await writeTestFile(docsDir, "a.txt", "first");
      await writeTestFile(docsDir, "subdir/b.txt", "inside");

      const svc = new FileService(docsDir);
      const result = await svc.getTree("");
      // directories come first, then files, sorted alphabetically
      const names = result.map((n: any) => n.name);
      const dirIdx = names.indexOf("subdir");
      const aIdx = names.indexOf("a.txt");
      const zIdx = names.indexOf("z.txt");
      assert.ok(dirIdx < aIdx, "directories should come before files");
      assert.ok(aIdx < zIdx, "files should be sorted alphabetically");
    });

    it("should set type correctly for files and dirs", async () => {
      await writeTestFile(docsDir, "file.txt", "content");
      await writeTestFile(docsDir, "dir/nested.txt", "nested");

      const svc = new FileService(docsDir);
      const result = await svc.getTree("");
      const fileNode = result.find((n: any) => n.name === "file.txt");
      const dirNode = result.find((n: any) => n.name === "dir");
      assert.equal(fileNode.type, "file");
      assert.equal(dirNode.type, "directory");
      assert.equal(dirNode.hasChildren, true);
    });

    it("should return error for path traversal", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getTree("../../etc");
      assert.ok("error" in result);
      assert.ok(result.error.includes("非法"));
    });

    it("should list subdirectory contents", async () => {
      await writeTestFile(docsDir, "sub/deep/file.txt", "deep");

      const svc = new FileService(docsDir);
      const result = await svc.getTree("sub/deep");
      assert.ok(Array.isArray(result));
      assert.equal(result.length, 1);
      assert.equal(result[0].name, "file.txt");
    });
  });

  describe("getFile", () => {
    it("should read text file content", async () => {
      await writeTestFile(docsDir, "hello.ts", "export const x = 1;");
      const svc = new FileService(docsDir);
      const result = await svc.getFile("hello.ts");
      assert.ok(!("error" in result));
      assert.equal(result.content, "export const x = 1;");
      assert.equal(result.language, "typescript");
      assert.ok(result.size > 0);
      assert.ok(result.mtime > 0);
    });

    it("should read markdown file with correct language", async () => {
      await writeTestFile(docsDir, "readme.md", "# Title");
      const svc = new FileService(docsDir);
      const result = await svc.getFile("readme.md");
      assert.equal(result.language, "markdown");
    });

    it("should return error for missing path parameter", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getFile("");
      assert.ok("error" in result);
      assert.ok(result.error.includes("缺少"));
    });

    it("should return error for non-existent file", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getFile("nonexistent.txt") as any;
      assert.ok("error" in result, "should return error object");
      assert.ok(result.error.includes("不存在"), "error message should mention 不存在");
    });

    it("should return error for directory read", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getFile(".");
      assert.ok("error" in result);
      assert.ok(result.error.includes("目录"));
    });

    it("should return error for path traversal", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getFile("../../etc/passwd");
      assert.ok("error" in result);
      assert.ok(result.error.includes("非法"));
    });
  });

  describe("getStat", () => {
    it("should return file metadata", async () => {
      await writeTestFile(docsDir, "statme.txt", "stat test content");
      const svc = new FileService(docsDir);
      const result = await svc.getStat("statme.txt");
      assert.ok(!("error" in result));
      assert.ok(result.size > 0);
      assert.ok(result.mtime > 0);
    });

    it("should return error for missing path", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getStat("");
      assert.ok("error" in result);
      assert.ok(result.error.includes("缺少"));
    });

    it("should return error for directory", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getStat(".");
      assert.ok("error" in result);
      assert.ok(result.error.includes("目录"));
    });

    it("should return error for path traversal", async () => {
      const svc = new FileService(docsDir);
      const result = await svc.getStat("../../etc/passwd");
      assert.ok("error" in result);
      assert.ok(result.error.includes("非法"));
    });
  });

  describe("validatePath", () => {
    it("should throw on path traversal", () => {
      const svc = new FileService(docsDir);
      assert.throws(
        () => svc.validatePath("../../etc/passwd"),
        (err: any) => err.status === 403 && err.message.includes("非法")
      );
    });

    it("should return valid full path", () => {
      const svc = new FileService(docsDir);
      const result = svc.validatePath("foo/bar.txt");
      assert.ok(result.startsWith(docsDir));
      assert.ok(result.endsWith("foo/bar.txt"));
    });
  });
});
