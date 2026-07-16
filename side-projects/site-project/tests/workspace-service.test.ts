// ============ WorkspaceService 测试 ============
import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { writeFileSync, readFileSync, unlinkSync, existsSync, mkdirSync } from "node:fs";

import { WorkspaceService } from "../src/services/workspace-service.js";
import { initWorkspace, loadWorkspace, deleteWorkspaceKey } from "../src/lib/workspace.js";

// 用临时 workspace.json 隔离，避免污染源码的 workspace.json
const tmpWorkspaceJson = join(process.env.TMPDIR || "/tmp", `ws-test-${Date.now()}.json`);

function cleanTmp() {
  if (existsSync(tmpWorkspaceJson)) unlinkSync(tmpWorkspaceJson);
}

beforeEach(() => {
  cleanTmp();
  initWorkspace(tmpWorkspaceJson);
});

describe("WorkspaceService", () => {
  const sessions = new Map();

  describe("getState", () => {
    it("should return default state with empty terminals", () => {
      const svc = new WorkspaceService(sessions);
      const state = svc.getState();
      assert.equal(state.fileTree.expandedDirs.length, 0);
      assert.equal(state.fileTree.activeFilePath, null);
      assert.equal(state.ui.sidebarWidth, 280);
      assert.ok(Array.isArray(state.terminals));
      assert.equal(state.terminals.length, 0);
    });

    it("should include alive terminal sessions", () => {
      sessions.set("t1", { alive: true, cols: 80, rows: 24, createdAt: 1000 });
      sessions.set("t2", { alive: false, cols: 80, rows: 24, createdAt: 2000 });
      const svc = new WorkspaceService(sessions);
      const state = svc.getState();
      assert.equal(state.terminals.length, 1);
      assert.equal(state.terminals[0].id, "t1");
      assert.equal(state.terminals[0].cols, 80);
    });
  });

  describe("updateState", () => {
    it("should deep merge into workspace state", () => {
      const svc = new WorkspaceService(sessions);
      svc.updateState({ ui: { sidebarWidth: 400 } });
      const state = svc.getState();
      assert.equal(state.ui.sidebarWidth, 400);
      // fileTree should remain unchanged
      assert.equal(state.fileTree.expandedDirs.length, 0);
    });

    it("should persist state to workspace.json", () => {
      const svc = new WorkspaceService(sessions);
      svc.updateState({ ui: { sidebarWidth: 500 } });
      const saved = JSON.parse(readFileSync(tmpWorkspaceJson, "utf-8"));
      assert.equal(saved.ui.sidebarWidth, 500);
    });

    it("should return success true", () => {
      const svc = new WorkspaceService(sessions);
      const result = svc.updateState({});
      assert.equal(result.success, true);
    });
  });

  describe("getKey / setKey", () => {
    it("should return undefined for non-existent key", () => {
      const svc = new WorkspaceService(sessions);
      assert.equal(svc.getKey("nonexistent"), undefined);
    });

    it("should write and read a new key", () => {
      const svc = new WorkspaceService(sessions);
      svc.setKey("myPlugin", { data: 42 });
      assert.deepEqual(svc.getKey("myPlugin"), { data: 42 });
    });

    it("should deep merge on patch, not replace", () => {
      const svc = new WorkspaceService(sessions);
      svc.setKey("myPlugin", { a: 1, b: { nested: "original" } });
      svc.setKey("myPlugin", { b: { nested: "patched" }, c: "new" });
      const result = svc.getKey("myPlugin");
      assert.equal(result.a, 1, "original key should remain");
      assert.equal(result.b.nested, "patched", "nested should be updated");
      assert.equal(result.c, "new", "new key should be added");
    });

    it("should persist key to workspace.json", () => {
      const svc = new WorkspaceService(sessions);
      svc.setKey("customData", { x: 1 });
      const saved = JSON.parse(readFileSync(tmpWorkspaceJson, "utf-8"));
      assert.deepEqual(saved.customData, { x: 1 });
    });
  });

  describe("deleteWorkspaceKey", () => {
    it("should remove key from state", () => {
      const svc = new WorkspaceService(sessions);
      svc.setKey("temp", "value");
      deleteWorkspaceKey("temp");
      assert.equal(svc.getKey("temp"), undefined);
    });
  });
});
