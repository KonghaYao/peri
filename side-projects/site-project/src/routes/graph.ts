// ============ Git Graph + Commit Diff 路由（瘦身版） ============
import type { Hono } from "hono";
import type { GraphService } from "../services/graph-service.js";

export function registerGraphRoutes(app: Hono, graphService: GraphService) {
  app.get("/api/scm/graph", async (c) => {
    try {
      const maxCommits = parseInt(c.req.query("max") || "200", 10);
      const result = await graphService.getGraph(maxCommits);
      if (result.hasRepo === false) return c.json({ hasRepo: false });
      if (result.error) return c.json(result, 500);
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  app.get("/api/scm/commit-diff", async (c) => {
    try {
      const hash = c.req.query("hash");
      const result = await graphService.getCommitDiff(hash || "");
      if (result.error) {
        const code = result.error === "缺少 hash 参数" || result.error === "无效的 hash" ? 400 : 500;
        return c.json(result, code);
      }
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });
}
