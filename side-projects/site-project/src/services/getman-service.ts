// ============ Getman 业务逻辑层 ============
import parseCurlLib from "parse-curl";
import type { ParsedCurl, GetmanProxyBody } from "../types.js";

export class GetmanService {
  async parseCurl(raw: string) {
    if (!raw.trim()) return { error: "输入为空" };

    let parsed: any;
    try {
      parsed = parseCurlLib(raw);
    } catch (err: any) {
      return { error: "解析失败: " + (err.message || String(err)) };
    }

    if (!parsed || !parsed.url) return { error: "未找到 URL" };

    // 提取查询参数
    let url = parsed.url || "";
    let params: Array<{ key: string; value: string; enabled: boolean }> = [];
    const qsIdx = url.indexOf("?");
    if (qsIdx > 0) {
      const qs = url.slice(qsIdx + 1);
      url = url.slice(0, qsIdx);
      qs.split("&").forEach(pair => {
        const eq = pair.indexOf("=");
        params.push({
          key: eq >= 0 ? decodeURIComponent(pair.slice(0, eq)) : pair,
          value: eq >= 0 ? decodeURIComponent(pair.slice(eq + 1)) : "",
          enabled: true,
        });
      });
    }

    // 转换 headers
    const headers: Array<{ key: string; value: string; enabled: boolean }> = [];
    const rawHeaders: Record<string, string> = parsed.header || {};
    for (const [k, v] of Object.entries(rawHeaders)) {
      headers.push({ key: k, value: String(v), enabled: true });
    }

    // 检测 body 类型
    let bodyType: "none" | "json" | "x-www-form-urlencoded" | "form-data" = "none";
    let bodyContent = parsed.body || "";
    let formFields: Array<{ key: string; value: string; enabled: boolean }> = [];

    if (bodyContent) {
      const ctHeader = headers.find(h => h.key.toLowerCase() === "content-type");
      const ct = ctHeader ? ctHeader.value.toLowerCase() : "";

      if (ct.includes("x-www-form-urlencoded") || (/^[^=]+=[^&]+(&[^=]+=[^&]+)*$/.test(bodyContent) && bodyContent.indexOf("{") === -1)) {
        bodyType = "x-www-form-urlencoded";
        bodyContent.split("&").forEach(pair => {
          const eq = pair.indexOf("=");
          formFields.push({
            key: eq >= 0 ? decodeURIComponent(pair.slice(0, eq)) : pair,
            value: eq >= 0 ? decodeURIComponent(pair.slice(eq + 1)) : "",
            enabled: true,
          });
        });
      } else {
        try { JSON.parse(bodyContent); bodyType = "json"; } catch { bodyType = "json"; }
      }
    }

    // 检测 auth 类型
    let authType: "none" | "bearer" | "basic" = "none";
    let authBasicUser = "", authBasicPass = "", authBearer = "";

    const authHeader = headers.find(h => h.key.toLowerCase() === "authorization");
    if (authHeader) {
      const val = authHeader.value;
      if (val.startsWith("Basic ")) {
        authType = "basic";
        try {
          const decoded = atob(val.slice(6));
          const colonIdx = decoded.indexOf(":");
          authBasicUser = colonIdx > 0 ? decoded.slice(0, colonIdx) : decoded;
          authBasicPass = colonIdx > 0 ? decoded.slice(colonIdx + 1) : "";
        } catch { /* ignore */ }
      } else if (val.startsWith("Bearer ")) {
        authType = "bearer";
        authBearer = val.slice(7);
      }
    }

    const result: ParsedCurl = {
      method: (parsed.method || "GET").toUpperCase(),
      url,
      headers: Object.fromEntries(headers.map(h => [h.key, h.value])),
      body: bodyType === "x-www-form-urlencoded" ? "" : bodyContent,
      bodyType,
      params,
      formFields,
      authType,
      authBasicUser,
      authBasicPass,
      authBearer,
    };

    return result;
  }

  async proxyRequest(body: GetmanProxyBody) {
    const { method, url, headers, body: reqBody, formFields } = body;

    if (!url || (!url.startsWith("http://") && !url.startsWith("https://"))) {
      return { error: "仅支持 http/https URL" };
    }

    const allowedMethods = ["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"];
    const upperMethod = (method || "GET").toUpperCase();
    if (!allowedMethods.includes(upperMethod)) {
      return { error: "不支持的 HTTP 方法: " + method };
    }

    // 剥离 host 类 header
    let fetchHeaders: Record<string, string> = {};
    if (headers) {
      for (const [k, v] of Object.entries(headers)) {
        const lk = k.toLowerCase();
        if (lk === "host" || lk === "content-length" || lk === "transfer-encoding") continue;
        fetchHeaders[k] = v;
      }
    }

    // 构建请求体
    let fetchBody: BodyInit | null = null;
    if (upperMethod !== "GET" && upperMethod !== "HEAD") {
      if (formFields && formFields.length > 0) {
        const fd = new FormData();
        for (const f of formFields) {
          if (f.enabled !== false && f.key) fd.append(f.key, f.value);
        }
        fetchBody = fd;
        Object.keys(fetchHeaders).forEach(k => {
          if (k.toLowerCase() === "content-type") delete fetchHeaders[k];
        });
      } else if (reqBody) {
        fetchBody = reqBody;
      }
    }

    const start = performance.now();
    let resp: Response;
    try {
      resp = await fetch(url, { method: upperMethod, headers: fetchHeaders, body: fetchBody, redirect: "follow" });
    } catch (err: any) {
      return { error: "请求失败", detail: err.message || String(err), time: Math.round(performance.now() - start) };
    }

    const elapsed = Math.round(performance.now() - start);
    const responseHeaders: Record<string, string> = {};
    resp.headers.forEach((v, k) => { responseHeaders[k] = v; });

    const contentType = resp.headers.get("Content-Type") || "";
    let responseBody: string;
    let size: number;

    const isText = contentType.includes("text") || contentType.includes("json")
      || contentType.includes("xml") || contentType.includes("javascript") || contentType.includes("form");
    if (isText) {
      responseBody = await resp.text();
      size = new TextEncoder().encode(responseBody).length;
    } else {
      const buf = await resp.arrayBuffer();
      size = buf.byteLength;
      if (size > 512 * 1024) {
        responseBody = `[Binary data: ${(size / 1024).toFixed(1)} KB]`;
      } else {
        const bytes = new Uint8Array(buf);
        let binStr = "";
        for (let i = 0; i < bytes.length; i++) binStr += String.fromCharCode(bytes[i]);
        responseBody = btoa(binStr);
        if (!responseHeaders["X-Getman-Encoding"]) responseHeaders["X-Getman-Encoding"] = "base64";
      }
    }

    return { status: resp.status, statusText: resp.statusText, headers: responseHeaders, body: responseBody, contentType, size, time: elapsed };
  }
}
