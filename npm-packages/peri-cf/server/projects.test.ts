import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import {
  DEFAULT_PROJECT_ID,
  DEFAULT_PROJECT_NAME,
  ProjectNameConflictError,
  ProjectNameError,
  ProjectNotFoundError,
  ProjectStore,
} from "./projects";

function tempRoot(): string {
  return mkdtempSync(join(tmpdir(), "peri-projects-"));
}

describe("ProjectStore", () => {
  test("missing manifest exposes only the default project and writes nothing", async () => {
    const root = tempRoot();
    try {
      const store = new ProjectStore(root);
      expect(await store.listViews()).toEqual([{
        id: DEFAULT_PROJECT_ID, name: DEFAULT_PROJECT_NAME, createdAt: "", isDefault: true,
      }]);
      expect(existsSync(join(root, "projects.json"))).toBe(false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("create normalizes names, persists atomically, and rejects duplicates", async () => {
    const root = tempRoot();
    try {
      const store = new ProjectStore(root, {
        idFactory: () => "prj_1",
        now: () => new Date("2026-01-01T00:00:00Z"),
      });
      const created = await store.create("  我的   项目 ");
      expect(created).toEqual({
        id: "prj_1", name: "我的 项目",
        createdAt: "2026-01-01T00:00:00.000Z", isDefault: false,
      });

      const reloaded = new ProjectStore(root);
      expect(await reloaded.get("prj_1")).toMatchObject({ name: "我的 项目", sandboxId: null });

      await expect(store.create("我的 项目")).rejects.toBeInstanceOf(ProjectNameConflictError);
      await expect(store.create(DEFAULT_PROJECT_NAME)).rejects.toBeInstanceOf(ProjectNameConflictError);
      await expect(store.create("   ")).rejects.toBeInstanceOf(ProjectNameError);
      await expect(store.create("x".repeat(65))).rejects.toBeInstanceOf(ProjectNameError);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("a corrupt manifest is not cached as empty and is never overwritten", async () => {
    const root = tempRoot();
    const file = join(root, "projects.json");
    try {
      writeFileSync(file, "{ not json", "utf8");
      const store = new ProjectStore(root, { idFactory: () => "prj_1" });

      await expect(store.listViews()).rejects.toThrow();
      await expect(store.create("项目 A")).rejects.toThrow();
      expect(readFileSync(file, "utf8")).toBe("{ not json");

      // 修复文件后同一实例重试读取，不沿用失败结果。
      writeFileSync(file, JSON.stringify({
        version: 1,
        projects: [{ id: "prj_old", name: "旧项目", sandboxId: "sbx_old", createdAt: "" }],
      }), "utf8");
      expect(await store.list()).toEqual([
        { id: "prj_old", name: "旧项目", sandboxId: "sbx_old", createdAt: "" },
      ]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("assignSandbox binds once and keeps the first workspace", async () => {
    const root = tempRoot();
    try {
      const store = new ProjectStore(root, { idFactory: () => "prj_1" });
      const project = await store.create("项目 A");
      expect((await store.assignSandbox(project.id, "sbx_a")).sandboxId).toBe("sbx_a");
      expect((await store.assignSandbox(project.id, "sbx_b")).sandboxId).toBe("sbx_a");
      await expect(store.assignSandbox("missing", "sbx_c"))
        .rejects.toBeInstanceOf(ProjectNotFoundError);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("projectIdForCwd matches only the project workspace subtree", () => {
    const root = "/sandboxes";
    const store = new ProjectStore(root);
    const records = [{ id: "prj_1", name: "项目 A", sandboxId: "sbx_a", createdAt: "" }];
    expect(store.projectIdForCwd("/sandboxes/sbx_a/workspace", records, root)).toBe("prj_1");
    expect(store.projectIdForCwd("/sandboxes/sbx_a/workspace/nested", records, root)).toBe("prj_1");
    expect(store.projectIdForCwd("/sandboxes/sbx_a/workspace-other", records, root))
      .toBe(DEFAULT_PROJECT_ID);
    expect(store.projectIdForCwd("/workspace", records, root)).toBe(DEFAULT_PROJECT_ID);
  });
});
