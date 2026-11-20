import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import path from 'node:path';
import { openSealedFileContent } from '../../../../server/platform/environments/content';
import { base64, type EnvironmentHarness, request, setupHarness, storedFileRow } from './helpers';

interface EnvironmentBody {
  readonly id: string;
  readonly object: string;
  readonly type: string;
  readonly status: string;
  readonly skills: unknown[];
  readonly plugins: unknown[];
  readonly files: unknown[];
}

interface EnvironmentListBody {
  readonly object: string;
  readonly data: EnvironmentBody[];
  readonly first_id: string | null;
  readonly last_id: string | null;
  readonly has_more: boolean;
}

interface ErrorBody {
  readonly error: {
    readonly type: string;
    readonly code: string;
    readonly message: string;
    readonly param: string | null;
  };
}

async function body<T>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

function storedEnvironmentConfig(
  harness: EnvironmentHarness,
  id: string,
): { raw: string; config: Record<string, unknown> } {
  const row = harness.db
    .query<{ config: string }, [string]>('SELECT config FROM platform_environments WHERE id = ?')
    .get(id);
  if (row === null) throw new Error(`Environment ${id} was not stored`);
  return { raw: row.config, config: JSON.parse(row.config) as Record<string, unknown> };
}

describe('platform environments', () => {
  let harness: EnvironmentHarness;

  beforeEach(() => {
    harness = setupHarness();
  });

  afterEach(() => {
    harness.cleanup();
  });

  test('creates, lists and retrieves an environment', async () => {
    const created = await request(harness.app, 'POST', '/agents/environments', {
      name: 'Primary',
      environment: { type: 'local', workspace_directory: harness.workspace },
    });
    expect(created.status).toBe(201);
    const environment = await body<EnvironmentBody>(created);
    expect(environment).toMatchObject({
      object: 'agent.environment',
      type: 'local',
      status: 'ready',
      skills: [],
      plugins: [],
      files: [],
    });
    expect(environment).not.toHaveProperty('name');
    expect(environment).not.toHaveProperty('workspace_directory');
    expect(environment).not.toHaveProperty('packages');
    expect(environment).not.toHaveProperty('network');
    expect(environment).not.toHaveProperty('desktop');
    expect(environment).not.toHaveProperty('container_size');
    expect(environment.status).not.toBe('connected');
    expect(created.headers.get('x-platform-degraded')).toBeNull();

    const list = await request(harness.app, 'GET', '/agents/environments');
    expect(list.status).toBe(200);
    const listBody = await body<EnvironmentListBody>(list);
    expect(listBody.object).toBe('list');
    expect(listBody.data.map((item) => item.id)).toEqual([environment.id]);
    expect(listBody.first_id).toBe(environment.id);
    expect(listBody.last_id).toBe(environment.id);
    expect(listBody.has_more).toBe(false);

    const fetched = await request(harness.app, 'GET', `/agents/environments/${environment.id}`);
    expect(fetched.status).toBe(200);
    expect((await body<EnvironmentBody>(fetched)).id).toBe(environment.id);

    // The documented contract has no environment deletion operation, so DELETE falls through
    // to the shared 404 instead of removing the resource.
    const deleted = await request(harness.app, 'DELETE', `/agents/environments/${environment.id}`);
    expect(deleted.status).toBe(404);
    expect((await body<ErrorBody>(deleted)).error.code).toBe('not_found');

    const stillThere = await request(harness.app, 'GET', `/agents/environments/${environment.id}`);
    expect(stillThere.status).toBe(200);
    expect((await body<EnvironmentBody>(stillThere)).id).toBe(environment.id);
  });

  test('maps unsupported environment types to local and marks the response degraded', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'remote_managed' },
    });
    expect(response.status).toBe(201);
    const environment = await body<EnvironmentBody>(response);
    expect(environment.type).toBe('local');
    expect(response.headers.get('x-platform-degraded')).toContain('environment_type_mapped_to_local');

    const fetched = await request(harness.app, 'GET', `/agents/environments/${environment.id}`);
    expect(fetched.headers.get('x-platform-degraded')).toContain('environment_type_mapped_to_local');
  });

  test('accepts the flat request shape and the local environment types', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      type: 'self_hosted',
      name: 'Flat',
    });
    expect(response.status).toBe(201);
    expect(await body<EnvironmentBody>(response)).toMatchObject({
      type: 'self_hosted',
      status: 'ready',
      files: [],
    });

    const none = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'none' },
    });
    expect(none.status).toBe(201);
    expect((await body<EnvironmentBody>(none)).type).toBe('none');
  });

  test('rejects a workspace_directory outside the deployment workspace', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', workspace_directory: path.dirname(harness.workspace) },
    });
    expect(response.status).toBe(400);
    const error = await body<ErrorBody>(response);
    expect(error.error.code).toBe('workspace_not_allowed');
    expect(error.error.param).toBe('workspace_directory');
  });

  test('rejects capability directories that cannot be mounted', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', capability_directories: [harness.workspace] },
    });
    expect(response.status).toBe(400);
    const error = await body<ErrorBody>(response);
    expect(error.error.code).toBe('environment_capability_unsupported');
    expect(error.error.param).toBe('capability_directories');
  });

  test('rejects unknown request fields', async () => {
    const nested = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', unexpected_option: true },
    });
    expect(nested.status).toBe(400);
    expect((await body<ErrorBody>(nested)).error.code).toBe('invalid_input');

    const topLevel = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local' },
      unexpected_option: true,
    });
    expect(topLevel.status).toBe(400);
    expect((await body<ErrorBody>(topLevel)).error.code).toBe('invalid_input');
  });

  test('keeps non-executed settings out of the resource and confidential inputs out of storage', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: {
        type: 'local',
        packages: { python: ['numpy'] },
        setup_commands: [{ command: 'make setup --secret-value' }],
        env: { SERVICE_TOKEN: 'environment-secret-value' },
        network: { access: 'enabled' },
        desktop: { enabled: true },
        container_size: 'large',
      },
      vault_ids: ['vault_test'],
    });
    expect(response.status).toBe(201);
    const environment = await body<EnvironmentBody>(response);
    for (const localField of [
      'name',
      'workspace_directory',
      'packages',
      'network',
      'desktop',
      'container_size',
      'capability_directories',
      'setup_commands',
      'env',
    ]) {
      expect(environment).not.toHaveProperty(localField);
    }

    const stored = storedEnvironmentConfig(harness, environment.id);
    expect(stored.config.packages).toEqual({ python: ['numpy'], system: [], npm: [] });
    expect(stored.config.setup_commands).toEqual([]);
    expect(stored.config.env).toEqual({});
    expect(stored.raw).not.toContain('make setup --secret-value');
    expect(stored.raw).not.toContain('environment-secret-value');

    const header = response.headers.get('x-platform-degraded') ?? '';
    for (const reason of [
      'packages_not_installed',
      'setup_commands_not_executed',
      'network_policy_not_enforced',
      'desktop_not_provisioned',
      'container_size_not_enforced',
      'vaults_not_attached',
    ]) {
      expect(header).toContain(reason);
    }

    const fetched = await request(harness.app, 'GET', `/agents/environments/${environment.id}`);
    expect(fetched.headers.get('x-platform-degraded')).toContain('setup_commands_not_executed');
  });

  test('copies an environment template before applying inline overrides', async () => {
    const templateResponse = await request(harness.app, 'POST', '/agents/environments/templates', {
      name: 'Base',
      packages: { python: ['numpy'] },
      network: { access: 'restricted', allowed_domains: ['example.test'] },
    });
    expect(templateResponse.status).toBe(201);
    const template = (await templateResponse.json()) as { id: string };

    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', environment_template_id: template.id },
    });
    expect(response.status).toBe(201);
    const environment = await body<EnvironmentBody>(response);
    expect(environment).not.toHaveProperty('packages');
    expect(environment).not.toHaveProperty('network');
    expect(storedEnvironmentConfig(harness, environment.id).config.packages).toEqual({
      python: ['numpy'],
      system: [],
      npm: [],
    });
    expect(storedEnvironmentConfig(harness, environment.id).config.network).toMatchObject({
      access: 'restricted',
      allowed_domains: ['example.test'],
    });
    expect(response.headers.get('x-platform-degraded')).toContain('packages_not_installed');

    const missing = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', environment_template_id: 'template_missing' },
    });
    expect(missing.status).toBe(404);

    const aliased = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'local', template_id: template.id },
    });
    expect(aliased.status).toBe(201);
    const aliasedEnvironment = await body<EnvironmentBody>(aliased);
    expect(storedEnvironmentConfig(harness, aliasedEnvironment.id).config.packages).toEqual({
      python: ['numpy'],
      system: [],
      npm: [],
    });
  });

  test('materializes inline files supplied at creation', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: {
        type: 'local',
        files: [{ type: 'inline', data: base64('seed content'), path: 'seed/notes.txt' }],
      },
    });
    expect(response.status).toBe(201);
    const environment = await body<EnvironmentBody>(response);
    expect(environment.files).toEqual([
      {
        type: 'inline',
        id: expect.any(String),
        path: '/workspace/seed/notes.txt',
        size_bytes: 'seed content'.length,
      },
    ]);

    const stored = storedFileRow(harness.db, environment.id, 'seed/notes.txt');
    expect(
      Buffer.from(openSealedFileContent(stored?.content_ciphertext as Uint8Array)).toString('utf8'),
    ).toBe('seed content');
  });

  test('paginates environments with ID cursors and filters by type', async () => {
    for (const name of ['one', 'two', 'three']) {
      const created = await request(harness.app, 'POST', '/agents/environments', {
        environment: { type: 'local' },
        name,
      });
      expect(created.status).toBe(201);
    }
    const none = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'none' },
    });
    expect(none.status).toBe(201);

    const firstPage = await request(harness.app, 'GET', '/agents/environments?limit=2&order=asc');
    const first = await body<EnvironmentListBody>(firstPage);
    expect(first.data).toHaveLength(2);
    expect(first.has_more).toBe(true);
    expect(first.last_id).not.toBeNull();

    const secondPage = await request(
      harness.app,
      'GET',
      `/agents/environments?limit=2&order=asc&after=${first.last_id}`,
    );
    const second = await body<EnvironmentListBody>(secondPage);
    expect(second.data).toHaveLength(2);
    expect(second.has_more).toBe(false);

    const filtered = await request(harness.app, 'GET', '/agents/environments?type=none');
    const filteredBody = await body<EnvironmentListBody>(filtered);
    expect(filteredBody.data).toHaveLength(1);
    expect(filteredBody.data[0]?.type).toBe('none');

    const mappedFilter = await request(harness.app, 'GET', '/agents/environments?type=remote_managed');
    const mappedBody = await body<EnvironmentListBody>(mappedFilter);
    expect(mappedBody.data.every((item) => item.type === 'local')).toBe(true);
    expect(mappedFilter.headers.get('x-platform-degraded')).toContain(
      'environment_type_filter_mapped_to_local',
    );
  });
});
