import { promises as dns } from "node:dns";
import { isIP } from "node:net";
import { z } from "zod";

interface LookupOptions {
  family?: number;
  all?: boolean;
}

const overridesSchema = z.record(z.string(), z.array(z.object({
  address: z.string(), family: z.union([z.literal(4), z.literal(6)]),
}).refine(entry => isIP(entry.address) === entry.family)).min(1));

export function workerDns(overrides?: string, resolver: Pick<typeof dns, "resolve4" | "resolve6"> = dns) {
  const hosts = overrides ? overridesSchema.parse(JSON.parse(overrides)) : {};
  return {
    lookup(hostname: string, options: LookupOptions, callback: (error: Error | null,
      addresses?: { address: string; family: number }[]) => void): void {
      const family = options.family === 6 ? 6 : 4;
      const resolving = hosts[hostname]
        ? Promise.resolve(hosts[hostname].filter(address => address.family === family))
        : family === 6
          ? resolver.resolve6(hostname).then(addresses => addresses.map(address => ({ address, family })))
          : resolver.resolve4(hostname).then(addresses => addresses.map(address => ({ address, family })));
      resolving.then(addresses => callback(null, addresses), error => callback(error));
    },
  };
}
