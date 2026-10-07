import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { localDnsOverrides } from "../scripts/local-dns";

const directories: string[] = [];

async function varsFile(contents: string): Promise<URL> {
  const directory = await mkdtemp(join(tmpdir(), "peri-local-dns-"));
  directories.push(directory);
  const path = join(directory, ".dev.vars");
  await writeFile(path, contents);
  return pathToFileURL(path);
}

afterEach(async () => {
  await Promise.all(directories.splice(0).map(directory => rm(directory, { recursive: true, force: true })));
});

describe("Vite local DNS overrides", () => {
  test("resolves only configured endpoints, preserves IPv4/IPv6, and leaves vars unchanged", async () => {
    const source = 'PERI_STORAGE_URL="libsql://store.invalid"\nMODEL_BASE_URL=https://model.invalid/v1\nAPP_AUTH_TOKEN=fixture-secret\n';
    const path = await varsFile(source);
    const lookedUp: string[] = [];
    const result = await localDnsOverrides(path, {}, async hostname => {
      lookedUp.push(hostname);
      return [{ address: "192.0.2.1", family: 4 }, { address: "2001:db8::1", family: 6 },
        { address: "192.0.2.1", family: 4 }];
    });
    expect(lookedUp.sort()).toEqual(["model.invalid", "store.invalid"]);
    expect(JSON.parse(result!)).toEqual({
      "store.invalid": [{ address: "192.0.2.1", family: 4 }, { address: "2001:db8::1", family: 6 }],
      "model.invalid": [{ address: "192.0.2.1", family: 4 }, { address: "2001:db8::1", family: 6 }],
    });
    expect(result).not.toContain("fixture-secret");
    expect(await readFile(path, "utf8")).toBe(source);
  });

  test("deduplicates hostnames and uses configured defaults only for missing vars", async () => {
    const path = await varsFile("PERI_STORAGE_URL=libsql://shared.invalid\n");
    let lookups = 0;
    const result = await localDnsOverrides(path, {
      PERI_STORAGE_URL: "libsql://unused.invalid", MODEL_BASE_URL: "https://shared.invalid/v1",
    }, async () => { lookups++; return [{ address: "192.0.2.2", family: 4 }]; });
    expect(lookups).toBe(1);
    expect(Object.keys(JSON.parse(result!))).toEqual(["shared.invalid"]);
  });

  test("IP literals do not require OS DNS", async () => {
    const path = await varsFile("PERI_STORAGE_URL=libsql://127.0.0.1\nMODEL_BASE_URL=https://[::1]/v1\n");
    const result = await localDnsOverrides(path, {}, async () => { throw new Error("unexpected lookup"); });
    expect(JSON.parse(result!)).toEqual({
      "127.0.0.1": [{ address: "127.0.0.1", family: 4 }], "::1": [{ address: "::1", family: 6 }],
    });
  });

  test("missing local vars file disables the bridge", async () => {
    const path = await varsFile("");
    await rm(path);
    expect(await localDnsOverrides(path, { MODEL_BASE_URL: "https://model.invalid" }, async () => {
      throw new Error("unexpected lookup");
    })).toBeUndefined();
  });

  test("DNS and endpoint failures do not expose credentials or underlying errors", async () => {
    const path = await varsFile("MODEL_BASE_URL=https://fixture-secret@model.invalid/v1\n");
    await expect(localDnsOverrides(path, {}, async () => { throw new Error("fixture-secret"); }))
      .rejects.toThrow("Local DNS lookup failed or exceeded 5000ms for MODEL_BASE_URL");
    await writeFile(path, "MODEL_BASE_URL=fixture-secret\n");
    await expect(localDnsOverrides(path)).rejects.toThrow("Local DNS requires a valid MODEL_BASE_URL URL with a hostname");
  });

  test("empty and malformed lookup results fail instead of masking startup problems", async () => {
    const path = await varsFile("MODEL_BASE_URL=https://model.invalid\n");
    for (const addresses of [[], [{ address: "192.0.2.3", family: 6 }]]) {
      await expect(localDnsOverrides(path, {}, async () => addresses))
        .rejects.toThrow("Local DNS lookup failed or exceeded 5000ms for MODEL_BASE_URL");
    }
  });

  test("an unresponsive resolver fails within the local startup deadline", async () => {
    const path = await varsFile("MODEL_BASE_URL=https://model.invalid\n");
    await expect(localDnsOverrides(path, {}, () => new Promise(() => {})))
      .rejects.toThrow("Local DNS lookup failed or exceeded 5000ms for MODEL_BASE_URL");
  }, 10_000);
});
