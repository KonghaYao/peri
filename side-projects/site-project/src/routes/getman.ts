// ============ Getman: cURL 解析 + HTTP 代理（瘦身版） ============
import type { Hono } from "hono";
import type { GetmanService } from "../services/getman-service.js";

export function registerGetmanRoutes(app: Hono, getmanService: GetmanService) {
  app.post("/api/getman/parse-curl", async (c) => {
    try {
      const body = await c.req.json() as { curl: string };
      const raw = body.curl || "";
      const result = await getmanService.parseCurl(raw);
      if (result.error) {
        const code = result.error === "输入为空" || result.error.startsWith("解析失败") || result.error === "未找到 URL" ? 400 : 500;
        return c.json(result, code);
      }
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });

  app.post("/api/getman/proxy", async (c) => {
    try {
      const body = await c.req.json();
      const result = await getmanService.proxyRequest(body);
      if (result.error) {
        const code = result.error.startsWith("仅支持") || result.error.startsWith("不支持的") ? 400 : 500;
        return c.json(result, code);
      }
      return c.json(result);
    } catch (err: any) {
      return c.json({ error: err.message }, 500);
    }
  });
}
