import {
  createMemoryChatOwnership,
  createPeriChatActor,
  type HostResourceOwner,
  type PeriChatActorDefinition,
} from '@peri-code/rivet/rivet';
import { TursoStorage } from '@peri-code/sdk';
import type { ServerEnv } from '../env';

/** 只读取 tracing 变量，避免枚举环境值时顺带读取 Provider key。 */
function wasmEnvFromProcess(): Record<string, string> {
  const env: Record<string, string> = {};
  for (const name of Object.keys(process.env)) {
    if (!name.startsWith('LANGFUSE_')) continue;
    const value = process.env[name];
    if (value !== undefined) env[name] = value;
  }
  return env;
}

/**
 * Builds the single local chat actor.
 *
 * `owner` is process-lifetime state created by the composition root: Rivet clears activation vars
 * even when a close was not confirmed, so an SDK/database close that failed once must stay
 * reachable for `owner.closeAll()`. The database adapter is the real Store; without a writable
 * Store the engine is never started, so there is no placeholder runtime in this path.
 */
export function createChatHost(
  env: ServerEnv,
  wasmModuleUrl: string,
  owner: HostResourceOwner,
): PeriChatActorDefinition {
  const wasmEnv = wasmEnvFromProcess();

  return createPeriChatActor({
    owner,
    openDatabase: (storage) =>
      new TursoStorage({
        url: storage.url,
        ...(storage.authToken === undefined ? {} : { authToken: storage.authToken }),
      }),
    ownership: createMemoryChatOwnership(),
    machineId: env.machineId,
    chatConfig: {
      runtime: {
        kind: 'wasm',
        moduleUrl: wasmModuleUrl,
        ...(Object.keys(wasmEnv).length === 0 ? {} : { env: wasmEnv }),
      },
    },
    // Diagnostics are observational: log the kind only, never the payload (it may hold paths).
    onDiagnostic: (diagnostic) => {
      console.error('[agent-builder] chat actor diagnostic', diagnostic.type);
    },
  });
}
