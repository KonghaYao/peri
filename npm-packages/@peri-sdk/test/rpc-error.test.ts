import { expect, test } from "bun:test";
import { JsonRpcTransport } from "../src/transport/json-rpc-transport";
import { RpcError } from "../src/transport/rpc-error";

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
