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
      environment: { type: 'self_hosted', workspace_directory: harness.workspace },
    });
    expect(created.status).toBe(201);
    const environment = await body<EnvironmentBody>(created);
    expect(environment).toMatchObject({
      object: 'agent.environment',
      type: 'self_hosted',
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

  test('rejects hosted environment types this deployment cannot provision', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'openai_hosted' },
    });
    expect(response.status).toBe(501);
    const error = await body<ErrorBody>(response);
    expect(error.error.type).toBe('not_implemented_error');
    expect(error.error.param).toBe('type');
  });

  test('rejects undocumented environment types instead of rewriting them', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'remote_managed' },
    });
    expect(response.status).toBe(400);
    const error = await body<ErrorBody>(response);
    expect(error.error.type).toBe('invalid_request_error');
    expect(error.error.param).toBe('type');
  });

  test('accepts the flat request shape and the executable environment types', async () => {
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
      environment: { type: 'self_hosted', workspace_directory: path.dirname(harness.workspace) },
    });
    expect(response.status).toBe(400);
    const error = await body<ErrorBody>(response);
    expect(error.error.code).toBe('workspace_not_allowed');
    expect(error.error.param).toBe('workspace_directory');
  });

  test('rejects capability directories that cannot be mounted', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted', capability_directories: [harness.workspace] },
    });
    expect(response.status).toBe(400);
    const error = await body<ErrorBody>(response);
    expect(error.error.code).toBe('environment_capability_unsupported');
    expect(error.error.param).toBe('capability_directories');
  });

  test('rejects unknown request fields', async () => {
    const nested = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted', unexpected_option: true },
    });
    expect(nested.status).toBe(400);
    expect((await body<ErrorBody>(nested)).error.code).toBe('invalid_input');

    const topLevel = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted' },
      unexpected_option: true,
    });
    expect(topLevel.status).toBe(400);
    expect((await body<ErrorBody>(topLevel)).error.code).toBe('invalid_input');
  });

  test('fails settings this deployment cannot execute instead of dropping them', async () => {
    const rejected: readonly (readonly [Record<string, unknown>, string])[] = [
      [{ packages: { python: ['numpy'] } }, 'packages'],
      [{ setup_commands: [{ command: 'make setup --secret-value' }] }, 'setup_commands'],
      [{ env: { SERVICE_TOKEN: 'environment-secret-value' } }, 'env'],
      [{ network: { access: 'enabled' } }, 'network.access'],
      [{ desktop: { enabled: true } }, 'desktop'],
      [{ container_size: 'large' }, 'container_size'],
      [{ skills: [{ type: 'skill_reference', skill_id: 'skill_one' }] }, 'skills'],
      [
        {
          plugins: [
            {
              type: 'inline',
              name: 'plugin_one',
              description: 'plugin',
              source: { type: 'base64', media_type: 'application/zip', data: base64('zip') },
            },
          ],
        },
        'plugins',
      ],
    ];

    for (const [environment, param] of rejected) {
      const response = await request(harness.app, 'POST', '/agents/environments', {
        environment: { type: 'self_hosted', ...environment },
      });
      expect(response.status).toBe(501);
      const error = await body<ErrorBody>(response);
      expect(error.error.type).toBe('not_implemented_error');
      expect(error.error.param).toBe(param);
      expect(error.error.message).not.toContain('secret-value');
    }

    const vault = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted' },
      vault_ids: ['vault_test'],
    });
    expect(vault.status).toBe(501);
    expect((await body<ErrorBody>(vault)).error.param).toBe('vault_ids');

    const rows = harness.db
      .query<{ count: number }, []>('SELECT COUNT(*) AS count FROM platform_environments')
      .get();
    expect(rows?.count).toBe(0);
  });

  test('applies executable environment templates and rejects template-declared capabilities', async () => {
    const templateResponse = await request(harness.app, 'POST', '/agents/environments/templates', {
      name: 'Base',
      packages: { python: ['numpy'] },
      network: { access: 'restricted', allowed_domains: ['example.test'] },
    });
    expect(templateResponse.status).toBe(201);
    const template = (await templateResponse.json()) as { id: string };

    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted', environment_template_id: template.id },
    });
    expect(response.status).toBe(501);
    const error = await body<ErrorBody>(response);
    expect(error.error.type).toBe('not_implemented_error');
    expect(error.error.param).toBe('packages');

    const missing = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted', environment_template_id: 'template_missing' },
    });
    expect(missing.status).toBe(404);

    const emptyTemplateResponse = await request(harness.app, 'POST', '/agents/environments/templates', {
      name: 'Empty',
    });
    expect(emptyTemplateResponse.status).toBe(201);
    const emptyTemplate = (await emptyTemplateResponse.json()) as { id: string };
    const aliased = await request(harness.app, 'POST', '/agents/environments', {
      environment: { type: 'self_hosted', template_id: emptyTemplate.id },
    });
    expect(aliased.status).toBe(201);
    const aliasedEnvironment = await body<EnvironmentBody>(aliased);
    expect(aliasedEnvironment).toMatchObject({ type: 'self_hosted', status: 'ready', files: [] });
    expect(storedEnvironmentConfig(harness, aliasedEnvironment.id).config.template_id).toBe(emptyTemplate.id);
  });

  test('materializes inline files supplied at creation', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments', {
      environment: {
        type: 'self_hosted',
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
        environment: { type: 'self_hosted' },
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

    const selfHostedFilter = await request(harness.app, 'GET', '/agents/environments?type=self_hosted');
    const selfHostedBody = await body<EnvironmentListBody>(selfHostedFilter);
    expect(selfHostedBody.data).toHaveLength(3);
    expect(selfHostedBody.data.every((item) => item.type === 'self_hosted')).toBe(true);

    const hostedFilter = await request(harness.app, 'GET', '/agents/environments?type=openai_hosted');
    expect(hostedFilter.status).toBe(200);
    expect((await body<EnvironmentListBody>(hostedFilter)).data).toEqual([]);

    const unknownFilter = await request(harness.app, 'GET', '/agents/environments?type=remote_managed');
    expect(unknownFilter.status).toBe(400);
    const unknownError = await body<ErrorBody>(unknownFilter);
    expect(unknownError.error.type).toBe('invalid_request_error');
    expect(unknownError.error.param).toBe('type');
  });
});
