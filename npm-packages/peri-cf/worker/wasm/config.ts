import type { Env } from "../types";
import { BareHarnessConfig } from "../sdk";
import { WORKSPACE_CWD } from "../chat/bootstrap";
import { chatIdSchema } from "../../shared/chat";

export function wasmConfig(env: Env): string {
  for (const name of ["PERI_MACHINE_ID", "PERI_STORAGE_URL", "PERI_STORAGE_TOKEN", "MODEL_BASE_URL", "MODEL_API_KEY", "MODEL_ID"] as const) {
    if (typeof env[name] !== "string" || !env[name]) throw new Error(`${name} is not configured`);
  }
  if (!chatIdSchema.safeParse(env.PERI_MACHINE_ID).success)
    throw new Error("PERI_MACHINE_ID must be a persistent deployment UUID");
  const provider = env.MODEL_PROVIDER ?? "openai";
  if (provider !== "openai" && provider !== "anthropic")
    throw new Error("MODEL_PROVIDER must be openai or anthropic");
  return JSON.stringify({
    cwd: WORKSPACE_CWD,
    machineId: env.PERI_MACHINE_ID,
    storage: { url: env.PERI_STORAGE_URL, authToken: env.PERI_STORAGE_TOKEN },
    settings: { config: {
      active_alias: "sonnet",
      providers: [{ id: "model", type: provider, apiKey: env.MODEL_API_KEY, baseUrl: env.MODEL_BASE_URL }],
      profiles: { sonnet: { provider: "model", model: env.MODEL_ID } },
      meta_harness: BareHarnessConfig,
    } },
  });
}
