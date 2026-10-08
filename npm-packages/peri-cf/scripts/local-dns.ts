import { lookup } from "node:dns/promises";
import type { LookupAddress } from "node:dns";
import { readFile } from "node:fs/promises";
import { isIP } from "node:net";
import { parseEnv } from "node:util";

const endpointNames = ["PERI_STORAGE_URL", "MODEL_BASE_URL"] as const;
const lookupTimeoutMs = 5_000;
type ResolveHost = (hostname: string) => Promise<LookupAddress[]>;

async function resolveWithinDeadline(hostname: string, resolveHost: ResolveHost): Promise<LookupAddress[]> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      resolveHost(hostname),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("Local DNS lookup timed out")), lookupTimeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export async function localDnsOverrides(
  devVarsPath: URL,
  defaults: Record<string, unknown> = {},
  resolveHost: ResolveHost = (hostname) => lookup(hostname, { all: true }),
): Promise<string | undefined> {
  let source: string;
  try {
    source = await readFile(devVarsPath, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw new Error("Unable to read local DNS configuration from .dev.vars");
  }
  const devVars = parseEnv(source);
  const hosts = new Map<string, string>();
  for (const name of endpointNames) {
    const endpoint = devVars[name] ?? defaults[name];
    if (endpoint === undefined) continue;
    try {
      if (typeof endpoint !== "string") throw new Error("Invalid endpoint");
      const url = new URL(endpoint);
      const hostname = url.hostname.replace(/^\[|\]$/g, "");
      if (!hostname) throw new Error("Missing hostname");
      hosts.set(hostname, name);
    } catch {
      throw new Error(`Local DNS requires a valid ${name} URL with a hostname`);
    }
  }
  if (hosts.size === 0) return undefined;
  const entries = await Promise.all([...hosts].map(async ([hostname, name]) => {
    try {
      const family = isIP(hostname);
      const addresses = family ? [{ address: hostname, family }]
        : await resolveWithinDeadline(hostname, resolveHost);
      if (!addresses.length || addresses.some(address =>
        ![4, 6].includes(address.family) || isIP(address.address) !== address.family)) {
        throw new Error("Invalid DNS result");
      }
      const unique = new Map(addresses.map(address =>
        [`${address.family}:${address.address}`, { address: address.address, family: address.family }]));
      return [hostname, [...unique.values()]] as const;
    } catch {
      throw new Error(`Local DNS lookup failed or exceeded 5000ms for ${name}`);
    }
  }));
  return JSON.stringify(Object.fromEntries(entries));
}
