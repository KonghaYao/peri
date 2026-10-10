import { describe, expect, test } from "bun:test";
import { workerDnsPort } from "../worker/wasm/dns";

describe("Workers WASM DNS boundary", () => {
  test("keeps local native DNS overrides scoped to their hostname and address family", async () => {
    const adapter = workerDnsPort(JSON.stringify({ "model.invalid": [
      { address: "192.0.2.2", family: 4 }, { address: "2001:db8::2", family: 6 },
    ] }));
    const result = await new Promise((resolve, reject) => {
      adapter.lookup("model.invalid", { all: true, family: 6 }, (error, addresses) => error ? reject(error) : resolve(addresses));
    });
    expect(result).toEqual([{ address: "2001:db8::2", family: 6 }]);
  });

  test("rejects deployment overrides that are not valid hostname/address mappings", () => {
    expect(() => workerDnsPort('{"model.invalid":[{"address":"not-an-ip","family":4}]}')).toThrow();
    expect(() => workerDnsPort('{"model.invalid":[]}')).toThrow();
    expect(() => workerDnsPort("{")).toThrow();
  });

  test("a host without overrides resolves through the host resolver rather than an override table", async () => {
    const adapter = workerDnsPort(undefined);
    const error = await new Promise(resolve => adapter.lookup("invalid.invalid", { family: 4 }, resolve));
    expect(error).toBeInstanceOf(Error);
  });
});
