// ============ SCM 路由：git 操作 API（瘦身版） ============
import type { Hono } from "hono";
import type { ScmService } from "../services/scm-service.js";

export function registerScmRoutes(app: Hono, scmService: ScmService) {
  app.get("/api/scm/detect", async (c) => {
    try { return c.json(await scmService.detect()); }
    catch (err: any) { return c.json({ hasRepo: false, error: err.message }, 500); }
  });

  app.get("/api/scm/summary", async (c) => {
    try { return c.json(await scmService.getSummary()); }
    catch (err: any) { return c.json({ hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, added: 0, modified: 0, deleted: 0, totalEntries: 0 }, 500); }
  });

  app.get("/api/scm/status", async (c) => {
    try { return c.json(await scmService.getStatus()); }
    catch (err: any) { return c.json({ hasRepo: true, error: err.message, branch: "", ahead: 0, behind: 0, staged: [], unstaged: [] }, 500); }
  });

  app.get("/api/scm/diff", async (c) => {
    try {
      const filePath = c.req.query("file");
      const staged = c.req.query("staged") === "true";
      const result = await scmService.getDiff(filePath || "", staged);
      if (result.error) return c.json(result, result.error === "缺少 file 参数" ? 400 : 403);
      return c.json(result);
    } catch (err: any) { return c.json({ diff: "", error: err.message }, 500); }
  });

  app.post("/api/scm/stage", async (c) => {
    try {
      const body = await c.req.json();
      const files: string[] = body.files || [];
      const toStage: boolean = body.action !== "unstage";
      return c.json(await scmService.stage(files, toStage));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/commit", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.commit(body.message || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/discard", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.discard(body.files || []));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.get("/api/scm/branches", async (c) => {
    try { return c.json(await scmService.getBranches()); }
    catch (err: any) { return c.json({ error: err.message, branches: [] }, 500); }
  });

  app.post("/api/scm/branch", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.switchBranch(body.branch || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/checkout-remote", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.checkoutRemote(body.branch || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/tag", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.createTag(body.name || "", body.message));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/create-branch", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.createBranch(body.name || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/cherry-pick", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.cherryPick(body.hash || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/revert", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.revert(body.hash || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/reset", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.reset(body.hash || "", body.mode || "mixed"));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/merge", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.merge(body.branch || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/fetch", async (c) => {
    try { return c.json(await scmService.fetch()); }
    catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/pull", async (c) => {
    try { return c.json(await scmService.pull()); }
    catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/push", async (c) => {
    try { return c.json(await scmService.push()); }
    catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });

  app.post("/api/scm/delete-remote-branch", async (c) => {
    try {
      const body = await c.req.json();
      return c.json(await scmService.deleteRemoteBranch(body.branch || ""));
    } catch (err: any) { return c.json({ success: false, error: err.message }, 500); }
  });
}
