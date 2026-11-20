import type { Database } from 'bun:sqlite';
import { ConfigError } from '../config-error';
import type { ServerEnv } from '../env';
import { type ProviderRecord, ProvidersRepository } from './repository';

export interface ProviderSeedResult {
  readonly type: NonNullable<ServerEnv['provider']['type']>;
  readonly action: 'inserted' | 'updated' | 'unchanged';
}

const PROVIDER_ENV_CONFLICT_MESSAGE = 'provider_env_conflict';

function providerEnvConflict(): ConfigError {
  return new ConfigError(PROVIDER_ENV_CONFLICT_MESSAGE);
}

/**
 * Projects the single environment-defined Provider onto the private SQLite row.
 *
 * The environment is the deployment source of truth, so absent optional values mean null. The
 * function never deletes rows: more than one row is a configuration conflict the operator must
 * resolve. Running the same environment twice leaves the first seed untouched (idempotent).
 */
export function seedProviderFromEnv(db: Database, env: ServerEnv): ProviderSeedResult | null {
  const { type, baseUrl, apiKey, name } = env.provider;
  if (type === undefined) return null;

  const repository = new ProvidersRepository(db);
  const rows = repository.list();
  if (rows.length > 1) throw providerEnvConflict();

  if (rows.length === 0) {
    repository.create({
      id: crypto.randomUUID(),
      name,
      type,
      baseUrl: baseUrl ?? null,
      credentialSource: 'local',
      apiKey: apiKey ?? null,
      revision: 1,
    });
    return { type, action: 'inserted' };
  }

  const current = rows[0];
  if (current === undefined) return null;
  const nextBaseUrl = baseUrl ?? null;
  const nextApiKey = apiKey ?? null;
  const changed =
    current.name !== name ||
    current.type !== type ||
    current.baseUrl !== nextBaseUrl ||
    current.apiKey !== nextApiKey;
  if (!changed) return { type, action: 'unchanged' };

  const next: ProviderRecord = {
    ...current,
    name,
    type,
    baseUrl: nextBaseUrl,
    // Legacy env rows cannot hold a key under the schema CHECK; writing one makes the row local.
    credentialSource: nextApiKey === null ? current.credentialSource : 'local',
    apiKey: nextApiKey,
    revision: current.revision + 1,
  };
  if (!repository.updateFromEnv(next, current.revision)) throw providerEnvConflict();
  return { type, action: 'updated' };
}
