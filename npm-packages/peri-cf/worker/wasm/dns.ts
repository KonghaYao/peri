import { isIP } from "node:net";
import { z } from "zod";
import { createNodeDnsPort, type WasmDnsPort } from "../sdk";

const overridesSchema = z.record(z.string(), z.array(z.object({
  address: z.string(), family: z.union([z.literal(4), z.literal(6)]),
}).refine(entry => isIP(entry.address) === entry.family)).min(1));

/** Deployment input is validated here; name resolution itself belongs to the SDK host port. */
export function workerDnsPort(overrides?: string): WasmDnsPort {
  const parsed = overrides ? overridesSchema.parse(JSON.parse(overrides)) : undefined;
  return createNodeDnsPort(parsed ? { overrides: parsed } : {});
}
