import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { PeriChatActorDefinition, PeriSandboxMcpActorDefinition } from '@peri-code/rivet/rivet';
import type { Client, Registry } from 'rivetkit';
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';

/**
 * The stock engine reserves 4 GiB for its log shard and refuses to start on smaller free disks.
 * The wrapper is the only place that can lower the quota: rivetkit's native layer injects its own
 * defaults into the engine environment after the host process starts.
 */
export const ENGINE_WRAPPER_PATH = fileURLToPath(
  new URL('../../scripts/rivet-engine-wrapper.sh', import.meta.url),
);

export type ChatRegistry = Registry<{
  chat: PeriChatActorDefinition;
  sandbox: PeriSandboxMcpActorDefinition;
}>;

export interface StartedRegistry {
  readonly registry: ChatRegistry;
  readonly client: Client<ChatRegistry>;
  /** Loopback engine endpoint used to construct the sandbox gateway MCP URL. */
  readonly endpoint: string;
  shutdown(): Promise<void>;
}

/**
 * `@rivetkit/engine-cli` reads this in the host process, so a runtime assignment is enough here.
 * An explicit user override always wins.
 */
export function configureEngineBinary(): void {
  if (process.env.RIVET_ENGINE_BINARY !== undefined) return;
  if (process.env.RIVET_ENGINE_BINARY_PATH !== undefined) return;
  if (!existsSync(ENGINE_WRAPPER_PATH)) {
    throw new Error(`rivet engine wrapper is missing at ${ENGINE_WRAPPER_PATH}`);
  }
  process.env.RIVET_ENGINE_BINARY_PATH = ENGINE_WRAPPER_PATH;
}

export interface StartRegistryOptions {
  readonly enginePort: number;
  readonly chat: PeriChatActorDefinition;
  /** Absent when sandbox is disabled; the actor still requires a token when present. */
  readonly sandbox?: PeriSandboxMcpActorDefinition;
}

export async function startChatRegistry(options: StartRegistryOptions): Promise<StartedRegistry> {
  const storagePath = process.env.RIVETKIT_STORAGE_PATH;
  if (storagePath === undefined || storagePath.length === 0) {
    // Falling back to ~/.rivetkit would silently escape the workspace and may collide with a
    // user-owned engine. Refuse instead; scripts/api.sh exports the workspace-relative default.
    throw new Error(
      'RIVETKIT_STORAGE_PATH is not set; start the API through scripts/api.sh (bun run start) so the rivet engine stays inside the workspace',
    );
  }

  configureEngineBinary();

  const registry = setup({
    // `use` is assembled dynamically; the client type below is the deployment contract.
    use: (options.sandbox === undefined
      ? { chat: options.chat }
      : { chat: options.chat, sandbox: options.sandbox }) as never,
    startEngine: true,
    engineHost: '127.0.0.1',
    enginePort: options.enginePort,
    startServices: false,
    shutdown: { disableSignalHandlers: true },
  });
  await registry.startAndWait();

  const endpoint = `http://127.0.0.1:${options.enginePort}`;
  const client = createClient<ChatRegistry>({
    endpoint,
    disableMetadataLookup: true,
  });

  let stopped = false;
  return {
    registry,
    client,
    endpoint,
    async shutdown() {
      if (stopped) return;
      stopped = true;
      const failures: unknown[] = [];
      try {
        await client.dispose();
      } catch (error) {
        failures.push(error);
      }
      try {
        await registry.shutdown();
      } catch (error) {
        failures.push(error);
      }
      if (failures.length > 0) throw failures[0];
    },
  };
}
