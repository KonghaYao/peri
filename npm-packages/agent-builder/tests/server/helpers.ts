import type { Database } from 'bun:sqlite';
import { WorkspaceURI } from '@peri-code/rivet/rivet';
import { type AgentDraft, isReferenceProvider } from '@shared/agent';
import { ProviderCreate } from '@shared/provider';
import type { ServerEnv } from '../../server/env';
import { ProvidersRepository } from '../../server/providers/repository';
import { ProviderService } from '../../server/providers/service';

/** 可执行夹具显式建立共享 SQLite 引用；旧 inline 行为由独立回归覆盖。 */
export function withStoredProvider(
  db: Database,
  draft: AgentDraft,
  key: string | null = crypto.randomUUID(),
): AgentDraft {
  const inline = draft.config.provider;
  if (!inline || isReferenceProvider(inline)) return structuredClone(draft);
  const providers = new ProviderService(new ProvidersRepository(db));
  const provider = providers.create(
    ProviderCreate.parse({
      name: inline.id,
      type: inline.type,
      baseUrl: inline.baseUrl ?? null,
      ...(key === null ? {} : { credential: { action: 'replace', value: key } }),
    }),
  );
  return {
    ...structuredClone(draft),
    config: {
      ...structuredClone(draft.config),
      provider: { kind: 'reference', providerId: provider.id, model: inline.model },
    },
  };
}

export const TEST_WORKSPACE_PATH = '/tmp/agent-builder-workspace';
export const TEST_STORE_URL = 'http://127.0.0.1:8091/db/local_default/agent_builder/main';

/** Explicit env for tests; secrets stay undefined unless a test opts in. */
export function testEnv(overrides: Partial<ServerEnv> = {}): ServerEnv {
  const machineId = '00000000-0000-4000-8000-000000000001';
  return {
    host: '127.0.0.1',
    port: 3000,
    databasePath: ':memory:',
    workspaceId: 'agent-builder-test',
    workspacePath: TEST_WORKSPACE_PATH,
    workspaceUri: new WorkspaceURI({ machineId, runtimePath: TEST_WORKSPACE_PATH }).toString(),
    storeUrl: TEST_STORE_URL,
    storeToken: undefined,
    enginePort: 8961,
    machineId,
    rivetStoragePath: undefined,
    sandbox: {
      enabled: false,
      mode: 'disabled',
      reasons: [],
      periPath: undefined,
      settingsPath: undefined,
      sandboxesRoot: '/tmp/agent-builder-sandboxes',
      profileName: 'default',
      token: undefined,
      gatewayToken: 'default',
      startupTimeoutMs: 90_000,
      stopTimeoutMs: 20_000,
      dataDir: '/tmp',
      workspacePathPrivate: false,
    },
    webRoot: undefined,
    allowedOrigins: ['http://127.0.0.1:3000', 'http://localhost:3000'],
    ...overrides,
  };
}
