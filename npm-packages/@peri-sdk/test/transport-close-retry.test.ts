import { expect, test } from "bun:test";
import { JsonRpcTransport } from "../src/transport/json-rpc-transport";

test("failed wire cleanup can be retried without reopening ACP admission", async () => {
  let attempts = 0;
  const transport = new JsonRpcTransport(async () => {}, async () => {
    attempts += 1;
    if (attempts === 1) throw new Error("cleanup incomplete");
  });

  const first = transport.close();
  expect(transport.close()).toBe(first);
  await expect(first).rejects.toThrow("cleanup incomplete");
  await expect(transport.request("session/new", {})).rejects.toThrow("ACP transport closed");

  const retry = transport.close();
  expect(transport.close()).toBe(retry);
  await retry;
  await transport.close();
  expect(attempts).toBe(2);
});
