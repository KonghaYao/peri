import { expect, test } from "bun:test";
import { JsonRpcTransport } from "../src/transport/json-rpc-transport";
import { RpcError } from "../src/transport/rpc-error";
import { controlFailureDisposition } from "../src/chat/error-mapping";

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
