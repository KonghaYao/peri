/// <reference types="bun" />

/**
 * LLM API Gateway — OpenAI / Anthropic 透明代理 + 请求日志
 *
 * 用法: bun run index.ts
 * 环境变量:
 *   PORT              监听端口 (默认 3456)
 *   OPENAI_BASE_URL   OpenAI upstream (默认 https://api.openai.com)
 *   ANTHROPIC_BASE_URL Anthropic upstream (默认 https://api.anthropic.com)
 *   LOG_LEVEL          日志级别: none | summary | body (默认 summary)
 *   LOG_DIR            日志目录 (默认 ./data)
 */

import { mkdirSync, writeFileSync } from "node:fs";

// ---------- 配置 ----------

export interface GatewayConfig {
    port: number;
    openaiBase: string;
    anthropicBase: string;
    logLevel: "none" | "summary" | "body";
    logDir: string;
}

export function loadConfig(
    env: Record<string, string | undefined> = process.env,
): GatewayConfig {
    return {
        port: Number(env.PORT) || 3456,
        openaiBase: (env.OPENAI_BASE_URL || "https://api.openai.com").replace(
            /\/+$/,
            "",
        ),
        anthropicBase: (
            env.ANTHROPIC_BASE_URL || "https://api.anthropic.com"
        ).replace(/\/+$/, ""),
        logLevel: (env.LOG_LEVEL as GatewayConfig["logLevel"]) || "summary",
        logDir: env.LOG_DIR || "./data",
    };
}

// ---------- 请求 ID & 文件日志 ----------

let reqCounter = 0;

export function resetReqCounter() {
    reqCounter = 0;
}

function nextId(): string {
    reqCounter++;
    const now = new Date();
    const date = now.toISOString().slice(0, 10);
    const time = now.toISOString().slice(11, 23).replace(/[.:]/g, "-");
    return `${date}_${time}_${String(reqCounter).padStart(4, "0")}`;
}

function reqDir(logDir: string, id: string): string {
    const dir = `${logDir}/${id}`;
    mkdirSync(dir, { recursive: true });
    return dir;
}

function writeReqFile(
    logDir: string,
    id: string,
    filename: string,
    content: string,
) {
    try {
        writeFileSync(`${reqDir(logDir, id)}/${filename}`, content, "utf-8");
    } catch {
        // 写入失败不影响代理
    }
}

// ---------- 终端日志 ----------

function logSep() {
    console.log("─".repeat(72));
}

function logC(color: number, ...args: unknown[]) {
    console.log(`\x1b[${color}m`, ...args, "\x1b[0m");
}

export function headersForLog(headers: Headers): Record<string, string> {
    return Object.fromEntries(headers.entries());
}

function logRequest(
    logLevel: GatewayConfig["logLevel"],
    route: string,
    method: string,
    url: string,
    headers: Headers,
    body: unknown,
) {
    if (logLevel === "none") return;
    const ts = new Date().toISOString();
    logSep();
    logC(36, `▶ ${ts}  ${method} ${route}`);
    logC(33, `  UPSTREAM: ${url}`);
    if (logLevel === "body") {
        logC(
            90,
            "  HEADERS:",
            JSON.stringify(headersForLog(headers), null, 2),
        );
        logC(90, "  BODY:", JSON.stringify(body, null, 2));
    }
}

function logResponse(
    logLevel: GatewayConfig["logLevel"],
    route: string,
    status: number,
    latencyMs: number,
    body: unknown,
    headers: Headers,
) {
    if (logLevel === "none") return;
    const ts = new Date().toISOString();
    const color = status >= 400 ? 31 : 32;
    logC(color, `◀ ${ts}  ${route}  → ${status}  (${latencyMs}ms)`);
    if (logLevel === "body") {
        const contentType = headers.get("content-type") || "";
        if (contentType.includes("json")) {
            logC(
                90,
                "  RESPONSE:",
                JSON.stringify(body, null, 2).slice(0, 4000),
            );
        } else {
            logC(
                90,
                "  RESPONSE: (non-JSON, length=" +
                    (body as string)?.length +
                    ")",
            );
        }
    }
    logSep();
}

// ---------- URL 拼接（避免路径重复）----------

export function resolveUrl(
    baseUrl: string,
    pathname: string,
    search: string,
): string {
    const base = new URL(baseUrl);
    const basePath = base.pathname.replace(/\/+$/, "");
    if (
        basePath &&
        (pathname.startsWith(basePath + "/") || pathname === basePath)
    ) {
        return `${base.origin}${pathname}${search}`;
    }
    return `${base.origin}${basePath}${pathname}${search}`;
}

// ---------- 代理核心 ----------

export async function proxyRequest(
    config: GatewayConfig,
    route: string,
    baseUrl: string,
    targetPath: string,
    req: Request,
): Promise<Response> {
    const id = nextId();
    const url = resolveUrl(baseUrl, targetPath, new URL(req.url).search);
    const reqBody = await req.text();
    const reqJson = reqBody ? JSON.parse(reqBody) : null;

    const proxyHeaders = new Headers(req.headers);
    proxyHeaders.set("host", new URL(baseUrl).host);

    logRequest(config.logLevel, route, req.method, url, proxyHeaders, reqJson);
    writeReqFile(
        config.logDir,
        id,
        "request.json",
        JSON.stringify(
            { headers: headersForLog(proxyHeaders), body: reqJson },
            null,
            2,
        ),
    );

    const start = performance.now();
    let upstreamRes: Response;
    try {
        upstreamRes = await fetch(url, {
            method: req.method,
            headers: proxyHeaders,
            body:
                req.method !== "GET" && req.method !== "HEAD"
                    ? reqBody
                    : undefined,
        });
    } catch (err) {
        const latency = Math.round(performance.now() - start);
        logC(31, `✖ ${route}  → FETCH ERROR  (${latency}ms)`, err);
        logSep();
        return new Response(
            JSON.stringify({
                error: "upstream_fetch_failed",
                detail: String(err),
            }),
            {
                status: 502,
                headers: { "content-type": "application/json" },
            },
        );
    }
    const latency = Math.round(performance.now() - start);

    const resHeaders = new Headers(upstreamRes.headers);
    // Bun.fetch 自动解压 gzip/br，但原始头仍保留 content-encoding/content-length，
    // 会导致下游客户端（如 reqwest）尝试二次解压已解压的数据。
    resHeaders.delete("content-encoding");
    resHeaders.delete("content-length");
    const contentType = resHeaders.get("content-type") || "";

    if (!upstreamRes.body) {
        logResponse(
            config.logLevel,
            route,
            upstreamRes.status,
            latency,
            null,
            upstreamRes.headers,
        );
        return new Response(null, {
            status: upstreamRes.status,
            headers: resHeaders,
        });
    }

    const isJson = contentType.includes("json");
    const logFileName = isJson
        ? "response.json"
        : contentType.includes("event-stream")
          ? "stream.log"
          : "response.bin";
    const chunks: Uint8Array[] = [];

    const transform = new TransformStream({
        transform(chunk, controller) {
            chunks.push(chunk);
            controller.enqueue(chunk);
        },
        flush() {
            const total = chunks.reduce((sum, c) => sum + c.length, 0);
            const merged = new Uint8Array(total);
            let offset = 0;
            for (const c of chunks) {
                merged.set(c, offset);
                offset += c.length;
            }
            const text = new TextDecoder().decode(merged);

            let resBody: unknown = text;
            if (isJson) {
                try {
                    resBody = JSON.parse(text);
                } catch {
                    /* keep raw text */
                }
            }

            writeReqFile(
                config.logDir,
                id,
                logFileName,
                isJson && typeof resBody !== "string"
                    ? JSON.stringify(resBody, null, 2)
                    : text,
            );
            logResponse(
                config.logLevel,
                route,
                upstreamRes.status,
                latency,
                resBody,
                new Headers(),
            );

            // 仅错误请求生成 error.txt
            if (
                upstreamRes.status >= 400 &&
                isJson &&
                resBody &&
                typeof resBody === "object"
            ) {
                const errObj = resBody as Record<string, unknown>;
                const model = reqJson?.model ?? "unknown";
                const msgCount = Array.isArray(reqJson?.messages)
                    ? reqJson.messages.length
                    : 0;
                const lines = [
                    `route: ${route}`,
                    `model: ${model}`,
                    `status: ${upstreamRes.status}`,
                    `latency: ${latency}ms`,
                    `messages: ${msgCount}`,
                    `---`,
                ];
                if (typeof errObj.error === "string") {
                    lines.push(errObj.error);
                } else if (errObj.error && typeof errObj.error === "object") {
                    const e = errObj.error as Record<string, unknown>;
                    lines.push(`type: ${e.type ?? "unknown"}`);
                    lines.push(`message: ${e.message ?? JSON.stringify(e)}`);
                } else {
                    lines.push(JSON.stringify(resBody, null, 2));
                }
                writeReqFile(
                    config.logDir,
                    id,
                    "error.txt",
                    lines.join("\n") + "\n",
                );
            }
        },
    });

    upstreamRes.body.pipeTo(transform.writable).catch(() => {});
    return new Response(transform.readable, {
        status: upstreamRes.status,
        headers: resHeaders,
    });
}

// ---------- 路由 ----------

export function createHandler(config: GatewayConfig) {
    return function handler(req: Request): Response | Promise<Response> {
        const { pathname } = new URL(req.url);

        // Anthropic 路由优先匹配（含不带 /v1 前缀的情况）
        if (pathname === "/messages" || pathname.startsWith("/v1/messages")) {
            return proxyRequest(
                config,
                "[anthropic]",
                config.anthropicBase,
                pathname.startsWith("/v1/") ? pathname : `/v1${pathname}`,
                req,
            );
        }
        // OpenAI 路由：/v1/* 通配 + 无前缀自动补 /v1
        if (pathname.startsWith("/v1/")) {
            return proxyRequest(
                config,
                "[openai]",
                config.openaiBase,
                pathname,
                req,
            );
        }
        // 兜底：常见 OpenAI 路径（不带 /v1 前缀）自动补 /v1 转发
        if (
            pathname.startsWith("/chat/") ||
            pathname.startsWith("/completions") ||
            pathname.startsWith("/models") ||
            pathname.startsWith("/embeddings") ||
            pathname.startsWith("/images/") ||
            pathname.startsWith("/audio/") ||
            pathname.startsWith("/responses") ||
            pathname.startsWith("/moderations") ||
            pathname.startsWith("/files") ||
            pathname.startsWith("/batches")
        ) {
            return proxyRequest(
                config,
                "[openai]",
                config.openaiBase,
                `/v1${pathname}`,
                req,
            );
        }
        if (pathname === "/health") {
            return Response.json({ ok: true, ts: new Date().toISOString() });
        }
        if (pathname === "/") {
            return Response.json({
                gateway: "llm-gateway",
                routes: {
                    "/v1/*": `→ ${config.openaiBase}/v1/*`,
                    "/v1/messages": `→ ${config.anthropicBase}/v1/messages`,
                    "/health": "health check",
                },
            });
        }
        if (config.logLevel !== "none") {
            logC(31, `✖ 404  ${req.method} ${pathname}`);
        }
        return new Response("Not Found", { status: 404 });
    };
}

// ---------- 启动（仅直接运行时）----------

if (import.meta.main) {
    const config = loadConfig();
    mkdirSync(config.logDir, { recursive: true });

    Bun.serve({
        port: config.port,
        fetch: createHandler(config),
        idleTimeout: 120,
    });
    console.log(`LLM Gateway listening on http://localhost:${config.port}`);
    console.log();
    console.log("Routes:");
    console.log(
        `  /v1/* (except /v1/messages) → ${config.openaiBase}/v1/*`,
    );
    console.log(
        `  /v1/messages                → ${config.anthropicBase}/v1/messages`,
    );
    console.log(`  /health                     → health check`);
    console.log();
    console.log(`  LOG_LEVEL = ${config.logLevel}`);
    console.log(`  LOG_DIR   = ${config.logDir}`);
}
