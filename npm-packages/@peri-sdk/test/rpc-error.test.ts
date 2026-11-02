import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, expect, test } from "bun:test";
import { JsonRpcTransport } from "../src/transport/json-rpc-transport";
import { isRpcError, RpcError } from "../src/transport/rpc-error";
import { controlFailureDisposition } from "../src/chat/error-mapping";

let bundleRoot: string | undefined;
let bundledRpc: typeof import("../src/transport/rpc-error");
let bundledMapping: typeof import("../src/chat/error-mapping");
let bundledClose: typeof import("../src/agent/session-close");

beforeAll(async () => {
    bundleRoot = await mkdtemp(join(tmpdir(), "peri-rpc-error-"));
    const sourceRoot = join(import.meta.dir, "../src");
    // 独立构建 transport 与控制面，重现主入口和 /wasm 各自内联 RpcError 的边界。
    const builds = await Promise.all([
        Bun.build({
            entrypoints: [join(sourceRoot, "transport/rpc-error.ts")],
            outdir: join(bundleRoot, "transport"),
            target: "browser",
            format: "esm",
        }),
        Bun.build({
            entrypoints: [
                join(sourceRoot, "chat/error-mapping.ts"),
                join(sourceRoot, "agent/session-close.ts"),
            ],
            root: sourceRoot,
            outdir: join(bundleRoot, "control"),
            target: "bun",
            format: "esm",
        }),
    ]);
    for (const build of builds) {
        if (!build.success) throw new AggregateError(build.logs, "测试 bundle 构建失败");
    }
    bundledRpc = await import(join(bundleRoot, "transport/rpc-error.js"));
    bundledMapping = await import(join(bundleRoot, "control/chat/error-mapping.js"));
    bundledClose = await import(join(bundleRoot, "control/agent/session-close.js"));
});

afterAll(async () => {
    if (bundleRoot) await rm(bundleRoot, { recursive: true, force: true });
});

test("ACP error retains domain code and structured data", async () => {
    let requestId = 0;
    const transport = new JsonRpcTransport(async (frame) => { requestId = JSON.parse(frame).id; }, async () => {});
    const response = transport.request("session/close", {});
    await Promise.resolve();
    transport.acceptFrame(JSON.stringify({ jsonrpc: "2.0", id: requestId,
        error: { code: -32010, message: "Incomplete", data: { status: "incomplete", retainedIntent: true } } }));
    await expect(response).rejects.toBeInstanceOf(RpcError);
    await expect(response).rejects.toMatchObject({ code: -32010, data: { retainedIntent: true } });
});

test("only definitive JSON-RPC rejections are safe to retry", () => {
    expect(controlFailureDisposition(new RpcError(-32602, "invalid"))).toBe("rejected");
    expect(controlFailureDisposition(new RpcError(-32010, "rejected"))).toBe("rejected");
    expect(controlFailureDisposition(new RpcError(-32603, "internal"))).toBe("unknown");
    expect(controlFailureDisposition(new Error("transport failed"))).toBe("unknown");
});

test("独立 bundle 的 RPC 错误保留契约与拒绝语义", () => {
    const data = { reason: "invalid_option" };
    const error = new bundledRpc.RpcError(-32602, "invalid", data);
    expect(bundledRpc.RpcError).not.toBe(RpcError);
    expect(error).not.toBeInstanceOf(RpcError);
    expect(error).toBeInstanceOf(Error);
    expect(error).toMatchObject({ name: "RpcError", code: -32602, message: "invalid" });
    expect(error.data).toBe(data);
    expect(isRpcError(error)).toBe(true);
    expect(bundledMapping.controlFailureDisposition(error)).toBe("rejected");
    expect(bundledMapping.controlFailureDisposition(new bundledRpc.RpcError(-32603, "internal")))
        .toBe("unknown");
});

test("独立 bundle 不把普通错误或伪造的 RPC 字段当作明确拒绝", () => {
    const branded = new bundledRpc.RpcError(-32602, "invalid");
    for (const error of [
        new Error("transport failed"),
        { code: -32602 },
        { name: "RpcError", code: -32602, message: "invalid" },
        Object.assign(new Error("invalid"), { name: "RpcError", code: -32602 }),
        { ...branded, message: "invalid" },
        Object.assign(new bundledRpc.RpcError(-32602, "invalid"), { code: "-32602" }),
        Object.assign(new bundledRpc.RpcError(-32602, "invalid"), { message: 42 }),
        Object.assign(new bundledRpc.RpcError(-32602, "invalid"), { name: "Error" }),
        null,
        undefined,
    ]) {
        expect(isRpcError(error)).toBe(false);
        expect(bundledMapping.controlFailureDisposition(error)).toBe("unknown");
    }
});

test("独立 bundle 的 incomplete close 会重试直至完成", async () => {
    const incomplete = new bundledRpc.RpcError(-32010, "close incomplete");
    let attempts = 0;
    let now = 0;
    const sleeps: number[] = [];
    await bundledClose.closeUntilSettled(async () => {
        if (++attempts <= 2) throw incomplete;
    }, { drainTimeoutMs: 100 }, {
        now: () => now,
        sleep: async (milliseconds) => {
            sleeps.push(milliseconds);
            now += milliseconds;
        },
    });
    expect(attempts).toBe(3);
    expect(sleeps).toEqual([50, 50]);
});

test("独立 bundle 的非 incomplete 错误不会触发关闭重试", async () => {
    const error = new bundledRpc.RpcError(-32602, "invalid");
    let attempts = 0;
    await expect(bundledClose.closeUntilSettled(async () => {
        attempts += 1;
        throw error;
    }, { drainTimeoutMs: 100 }, {
        now: () => 0,
        sleep: async () => { throw new Error("不应重试"); },
    })).rejects.toBe(error);
    expect(attempts).toBe(1);
});
