import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { base64, type EnvironmentHarness, request, setupHarness } from './helpers';

interface TemplateBody {
  readonly id: string;
  readonly name: string | null;
  readonly object: string;
  readonly created_at: number;
  readonly updated_at: number;
  readonly packages: { readonly python: string[]; readonly system: string[]; readonly npm: string[] };
  readonly network: { readonly access: string; readonly allowed_domains: string[] };
  readonly desktop: { readonly enabled: boolean };
  readonly capability_directories: string[];
  readonly skills: unknown[];
  readonly plugins: unknown[];
  readonly files: unknown[];
}

interface TemplateListBody {
  readonly object: string;
  readonly data: TemplateBody[];
  readonly first_id: string | null;
  readonly last_id: string | null;
  readonly has_more: boolean;
}

interface ErrorBody {
  readonly error: { readonly code: string; readonly param: string | null };
}

async function body<T>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

describe('platform environment templates', () => {
  let harness: EnvironmentHarness;

  beforeEach(() => {
    harness = setupHarness();
  });

  afterEach(() => {
    harness.cleanup();
  });

  test('creates, retrieves, updates and deletes a template', async () => {
    const created = await request(harness.app, 'POST', '/agents/environments/templates', {
      name: 'Base',
      packages: { python: ['numpy'] },
      setup_commands: [{ command: 'echo confidential' }],
      env: { SECRET_VALUE: 'not-returned' },
      network: { access: 'restricted', allowed_domains: ['example.test'] },
      desktop: { enabled: false },
      files: [{ type: 'inline', data: base64('abc'), path: 'notes/a.txt' }],
    });
    expect(created.status).toBe(201);
    const template = await body<TemplateBody>(created);
    expect(template).toMatchObject({
      name: 'Base',
      object: 'agent.environment.template',
      packages: { python: ['numpy'], system: [], npm: [] },
      network: { access: 'restricted', allowed_domains: ['example.test'] },
      desktop: { enabled: false },
      capability_directories: [],
      skills: [],
      plugins: [],
      files: [{ type: 'inline', path: '/workspace/notes/a.txt', size_bytes: 3 }],
    });
    expect(typeof template.created_at).toBe('number');
    expect(template).not.toHaveProperty('setup_commands');
    expect(template).not.toHaveProperty('env');

    const stored = harness.db
      .query<{ config: string }, [string]>('SELECT config FROM platform_environment_templates WHERE id = ?')
      .get(template.id);
    expect(stored?.config).not.toContain('echo confidential');
    expect(stored?.config).not.toContain('not-returned');
    // Storage stays workspace-relative; only the wire form is absolute.
    expect(stored?.config).toContain('"path":"notes/a.txt"');

    const fetched = await body<TemplateBody>(
      await request(harness.app, 'GET', `/agents/environments/templates/${template.id}`),
    );
    expect(fetched).toEqual(template);

    const updated = await body<TemplateBody>(
      await request(harness.app, 'POST', `/agents/environments/templates/${template.id}`, {
        name: null,
        packages: { python: ['pandas'] },
        desktop: { enabled: true },
      }),
    );
    expect(updated.name).toBeNull();
    expect(updated.packages.python).toEqual(['pandas']);
    expect(updated.network.access).toBe('restricted');
    expect(updated.desktop.enabled).toBe(true);

    const deleted = await request(harness.app, 'DELETE', `/agents/environments/templates/${template.id}`);
    expect(deleted.status).toBe(200);
    expect(await body(deleted)).toEqual({
      id: template.id,
      object: 'agent.environment.template.deleted',
      deleted: true,
    });

    const missing = await request(harness.app, 'GET', `/agents/environments/templates/${template.id}`);
    expect(missing.status).toBe(404);
    expect((await body<ErrorBody>(missing)).error.code).toBe('not_found');

    const missingDelete = await request(
      harness.app,
      'DELETE',
      `/agents/environments/templates/${template.id}`,
    );
    expect(missingDelete.status).toBe(404);
  });

  test('renders template file paths absolutely and rejects paths outside /workspace', async () => {
    const created = await request(harness.app, 'POST', '/agents/environments/templates', {
      files: [
        { type: 'inline', data: base64('abc'), path: '/workspace/docs/a.txt' },
        { type: 'file_id', file_id: 'file_123', path: '/workspace/docs/b.txt' },
      ],
    });
    expect(created.status).toBe(201);
    const template = await body<TemplateBody>(created);
    expect(template.files).toEqual([
      { type: 'inline', path: '/workspace/docs/a.txt', size_bytes: 3 },
      { type: 'file_id', file_id: 'file_123', path: '/workspace/docs/b.txt' },
    ]);

    const rejected = await request(harness.app, 'POST', '/agents/environments/templates', {
      files: [{ type: 'file_id', file_id: 'file_123', path: '/etc/passwd' }],
    });
    expect(rejected.status).toBe(400);
    expect((await body<ErrorBody>(rejected)).error.code).toBe('environment_path_invalid');
  });

  test('creates a template with defaults when the body is empty', async () => {
    const response = await request(harness.app, 'POST', '/agents/environments/templates', {});
    expect(response.status).toBe(201);
    expect(await body<TemplateBody>(response)).toMatchObject({
      name: null,
      packages: { python: [], system: [], npm: [] },
      network: { access: 'disabled', allowed_domains: [] },
      desktop: { enabled: false },
      capability_directories: [],
      skills: [],
      plugins: [],
      files: [],
    });
  });

  test('paginates templates with ID cursors', async () => {
    const ids: string[] = [];
    for (const name of ['one', 'two', 'three']) {
      const response = await request(harness.app, 'POST', '/agents/environments/templates', {
        name,
      });
      expect(response.status).toBe(201);
      ids.push(((await response.json()) as TemplateBody).id);
    }

    const firstPage = await body<TemplateListBody>(
      await request(harness.app, 'GET', '/agents/environments/templates?limit=2&order=asc'),
    );
    expect(firstPage.object).toBe('list');
    expect(firstPage.data.map((item) => item.id)).toEqual(ids.slice(0, 2));
    expect(firstPage.first_id).toBe(ids[0] as string);
    expect(firstPage.last_id).toBe(ids[1] as string);
    expect(firstPage.has_more).toBe(true);

    const secondPage = await body<TemplateListBody>(
      await request(
        harness.app,
        'GET',
        `/agents/environments/templates?limit=2&order=asc&after=${firstPage.last_id}`,
      ),
    );
    expect(secondPage.data.map((item) => item.id)).toEqual([ids[2] as string]);
    expect(secondPage.has_more).toBe(false);
  });

  test('rejects capability directories and unknown fields', async () => {
    const capability = await request(harness.app, 'POST', '/agents/environments/templates', {
      capability_directories: ['/opt/capabilities'],
    });
    expect(capability.status).toBe(400);
    const capabilityError = await body<ErrorBody>(capability);
    expect(capabilityError.error.code).toBe('environment_capability_unsupported');
    expect(capabilityError.error.param).toBe('capability_directories');

    const unknown = await request(harness.app, 'POST', '/agents/environments/templates', {
      unexpected_option: true,
    });
    expect(unknown.status).toBe(400);
    expect((await body<ErrorBody>(unknown)).error.code).toBe('invalid_input');
  });

  test('rejects template updates that add capability directories', async () => {
    const created = await body<TemplateBody>(
      await request(harness.app, 'POST', '/agents/environments/templates', { name: 'Base' }),
    );
    const response = await request(harness.app, 'POST', `/agents/environments/templates/${created.id}`, {
      capability_directories: ['/opt/capabilities'],
    });
    expect(response.status).toBe(400);
    expect((await body<ErrorBody>(response)).error.code).toBe('environment_capability_unsupported');
  });

  test('returns 404 for an unknown template', async () => {
    const response = await request(harness.app, 'GET', '/agents/environments/templates/missing');
    expect(response.status).toBe(404);
    expect((await body<ErrorBody>(response)).error.code).toBe('not_found');
  });
});
