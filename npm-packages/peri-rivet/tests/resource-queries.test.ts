import { describe, expect, test } from 'bun:test';
import type { ResourceSnapshot } from '../src/contracts/client.js';
import { parseConfigRevision } from '../src/contracts/index.js';
import { parseResourceId, parseResourceScope, type ResourceId, type ResourceScope } from '../src/index.js';
import type { ResourceQueries } from '../src/resource/queries.js';

class MemoryResourceQueries implements ResourceQueries {
  readonly #entries = new Map<string, ResourceSnapshot>();

  put(scope: ResourceScope, resourceId: ResourceId, snapshot: ResourceSnapshot): void {
    this.#entries.set(`${scope}\u0000${resourceId}`, snapshot);
  }

  async get(scope: ResourceScope, resourceId: ResourceId): Promise<ResourceSnapshot | null> {
    return this.#entries.get(`${scope}\u0000${resourceId}`) ?? null;
  }
}

const resourceId = parseResourceId('res_1');
const scopeA = parseResourceScope('tenant_a');
const scopeB = parseResourceScope('tenant_b');

function snapshot(model: string): ResourceSnapshot {
  return {
    kind: 'model_settings',
    revision: parseConfigRevision(`rev_${model}`),
    value: { model },
    observedAt: '2026-11-13T00:00:00.000Z',
  };
}

describe('ResourceQueries', () => {
  test('returns the snapshot when the scope and resource match', async () => {
    const queries = new MemoryResourceQueries();
    const stored = snapshot('a');
    queries.put(scopeA, resourceId, stored);

    await expect(queries.get(scopeA, resourceId)).resolves.toBe(stored);
  });

  test('returns null for a missing resource instead of throwing', async () => {
    const queries = new MemoryResourceQueries();

    await expect(queries.get(scopeA, parseResourceId('res_missing'))).resolves.toBeNull();
  });

  test('does not leak a resource across scopes', async () => {
    const queries = new MemoryResourceQueries();
    queries.put(scopeA, resourceId, snapshot('a'));
    queries.put(scopeB, resourceId, snapshot('b'));

    await expect(queries.get(scopeA, resourceId)).resolves.toMatchObject({ value: { model: 'a' } });
    await expect(queries.get(scopeB, resourceId)).resolves.toMatchObject({ value: { model: 'b' } });
    await expect(queries.get(parseResourceScope('tenant_c'), resourceId)).resolves.toBeNull();
  });
});
