import { describe, expect, test } from "bun:test";
import { workerDns } from "../worker/wasm/dns";

describe("Workers WASM DNS boundary", () => {
  test("uses supported record resolvers instead of node:dns.lookup", async () => {
    const called: string[] = [];
    const resolver = {
      resolve4: async (hostname: string) => { called.push(hostname); return ["192.0.2.1"]; },
      resolve6: async () => ["2001:db8::1"],
    } as unknown as Parameters<typeof workerDns>[1];
    const adapter = workerDns(undefined, resolver);
    const result = await new Promise((resolve, reject) => {
      adapter.lookup("model.invalid", { all: true, family: 4 }, (error, addresses) => error ? reject(error) : resolve(addresses));
    });
    expect(result).toEqual([{ address: "192.0.2.1", family: 4 }]);
    expect(called).toEqual(["model.invalid"]);
  });

  test("keeps local native DNS overrides scoped to their hostname and address family", async () => {
    const adapter = workerDns(JSON.stringify({ "model.invalid": [
      { address: "192.0.2.2", family: 4 }, { address: "2001:db8::2", family: 6 },
    ] }));
    const result = await new Promise((resolve, reject) => {
      adapter.lookup("model.invalid", { all: true, family: 6 }, (error, addresses) => error ? reject(error) : resolve(addresses));
    });
    expect(result).toEqual([{ address: "2001:db8::2", family: 6 }]);
    expect(() => workerDns('{"model.invalid":[{"address":"not-an-ip","family":4}]}')).toThrow();
  });

  test("delivers resolver failure to the callback instead of leaving WASM waiting forever", async () => {
    const expected = new Error("DNS unavailable");
    const resolver = {
      resolve4: async () => { throw expected; }, resolve6: async () => { throw expected; },
    } as unknown as Parameters<typeof workerDns>[1];
    const result = await new Promise(resolve => workerDns(undefined, resolver)
      .lookup("model.invalid", {}, error => resolve(error)));
    expect(result).toBe(expected);
  });
});
