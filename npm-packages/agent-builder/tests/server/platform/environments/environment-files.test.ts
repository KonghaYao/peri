import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { symlinkSync } from 'node:fs';
import path from 'node:path';
import {
  MAX_ENVIRONMENT_FILE_BYTES,
  openSealedFileContent,
} from '../../../../server/platform/environments/content';
import {
  base64,
  createEnvironment,
  type EnvironmentHarness,
  request,
  setupHarness,
  storedFileRow,
} from './helpers';

interface EnvironmentFileBody {
  readonly object: string;
  readonly environment_id: string;
  readonly path: string;
  readonly size_bytes: number;
}

interface FilePageBody {
  readonly object: string;
  readonly data: EnvironmentFileBody[];
  readonly next: string | null;
  readonly has_more: boolean;
}

interface ErrorBody {
  readonly error: { readonly code: string; readonly param: string | null };
}

async function body<T>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

describe('platform environment files', () => {
  let harness: EnvironmentHarness;
  let environmentId: string;

  beforeEach(async () => {
    harness = setupHarness();
    const environment = await createEnvironment(harness.app, harness.workspace);
    environmentId = environment.id as string;
  });

  afterEach(() => {
    harness.cleanup();
  });

  test('writes, lists and persists inline content', async () => {
    const content = 'hello environment';
    const created = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'inline',
      data: base64(content),
      path: 'notes/hello.txt',
    });
    expect(created.status).toBe(201);
    expect(await body<EnvironmentFileBody>(created)).toEqual({
      object: 'agent.environment.file',
      environment_id: environmentId,
      path: '/workspace/notes/hello.txt',
      size_bytes: content.length,
    });

    const list = await request(harness.app, 'GET', `/agents/environments/${environmentId}/files`);
    expect(list.status).toBe(200);
    const page = await body<FilePageBody>(list);
    expect(page.object).toBe('page');
    expect(page.data).toEqual([
      {
        object: 'agent.environment.file',
        environment_id: environmentId,
        path: '/workspace/notes/hello.txt',
        size_bytes: content.length,
      },
    ]);
    expect(page.next).toBeNull();
    expect(page.has_more).toBe(false);

    const stored = storedFileRow(harness.db, environmentId, 'notes/hello.txt');
    expect(stored).not.toBeNull();
    expect(stored?.size_bytes).toBe(content.length);
    const plaintext = openSealedFileContent(stored?.content_ciphertext as Uint8Array);
    expect(Buffer.from(plaintext).toString('utf8')).toBe(content);
  });

  test('replaces content for an existing path instead of duplicating it', async () => {
    const send = (text: string) =>
      request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
        type: 'inline',
        data: base64(text),
        path: 'notes/hello.txt',
      });

    expect((await send('first')).status).toBe(201);
    expect((await send('second version')).status).toBe(201);

    const page = await body<FilePageBody>(
      await request(harness.app, 'GET', `/agents/environments/${environmentId}/files`),
    );
    expect(page.data).toHaveLength(1);
    expect(page.data[0]?.size_bytes).toBe('second version'.length);

    const stored = storedFileRow(harness.db, environmentId, 'notes/hello.txt');
    expect(
      Buffer.from(openSealedFileContent(stored?.content_ciphertext as Uint8Array)).toString('utf8'),
    ).toBe('second version');
  });

  test('rejects absolute paths, traversal, and empty paths', async () => {
    const cases: readonly { path: string; code: string }[] = [
      { path: '/etc/passwd', code: 'environment_path_invalid' },
      { path: '../escape.txt', code: 'environment_path_invalid' },
      { path: 'notes/../../escape.txt', code: 'environment_path_invalid' },
      { path: 'notes//hello.txt', code: 'environment_path_invalid' },
      { path: '', code: 'invalid_input' },
    ];

    for (const item of cases) {
      const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
        type: 'inline',
        data: base64('nope'),
        path: item.path,
      });
      expect({ path: item.path, status: response.status }).toEqual({
        path: item.path,
        status: 400,
      });
      expect((await body<ErrorBody>(response)).error.code).toBe(item.code);
    }
  });

  test('accepts the documented /workspace form and maps it to the workspace root', async () => {
    const content = 'workspace-rooted';
    const created = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'inline',
      data: base64(content),
      path: '/workspace/notes/rooted.txt',
    });
    expect(created.status).toBe(201);
    expect(await body<EnvironmentFileBody>(created)).toMatchObject({
      environment_id: environmentId,
      path: '/workspace/notes/rooted.txt',
      size_bytes: content.length,
    });
    expect(storedFileRow(harness.db, environmentId, 'notes/rooted.txt')).not.toBeNull();

    const replaced = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'inline',
      data: base64('replaced'),
      path: 'notes/rooted.txt',
    });
    expect(replaced.status).toBe(201);
    const page = await body<FilePageBody>(
      await request(harness.app, 'GET', `/agents/environments/${environmentId}/files?path=/workspace/notes`),
    );
    expect(page.data.map((item) => item.path)).toEqual(['/workspace/notes/rooted.txt']);
    expect(page.data[0]?.size_bytes).toBe('replaced'.length);
  });

  test('rejects traversal and escapes below the /workspace form', async () => {
    for (const filePath of ['/workspace/../escape.txt', '/workspace/notes/../../escape.txt', '/workspace/']) {
      const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
        type: 'inline',
        data: base64('nope'),
        path: filePath,
      });
      expect({ filePath, status: response.status }).toEqual({ filePath, status: 400 });
      expect((await body<ErrorBody>(response)).error.code).toBe('environment_path_invalid');
    }
  });

  test('rejects symbolic link targets inside the workspace', async () => {
    symlinkSync(path.join(harness.workspace, 'target.txt'), path.join(harness.workspace, 'link.txt'));

    const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'inline',
      data: base64('nope'),
      path: 'link.txt',
    });
    expect(response.status).toBe(400);
    expect((await body<ErrorBody>(response)).error.code).toBe('environment_path_symlink');
  });

  test('rejects content above the local size ceiling', async () => {
    const oversized = Buffer.alloc(MAX_ENVIRONMENT_FILE_BYTES + 1, 0x61).toString('base64');
    const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'inline',
      data: oversized,
      path: 'large.bin',
    });
    expect(response.status).toBe(400);
    expect((await body<ErrorBody>(response)).error.code).toBe('environment_file_too_large');
  });

  test('rejects file references that cannot be resolved locally', async () => {
    const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
      type: 'file_id',
      file_id: 'file_123',
      path: 'notes/hello.txt',
    });
    expect(response.status).toBe(400);
    expect((await body<ErrorBody>(response)).error.code).toBe('environment_file_reference_unsupported');
  });

  test('paginates by path and filters by directory', async () => {
    for (const filePath of ['alpha/one.txt', 'alpha/two.txt', 'gamma/three.txt']) {
      const response = await request(harness.app, 'POST', `/agents/environments/${environmentId}/files`, {
        type: 'inline',
        data: base64(filePath),
        path: filePath,
      });
      expect(response.status).toBe(201);
    }

    const firstPage = await body<FilePageBody>(
      await request(harness.app, 'GET', `/agents/environments/${environmentId}/files?limit=2&order=asc`),
    );
    expect(firstPage.data.map((item) => item.path)).toEqual([
      '/workspace/alpha/one.txt',
      '/workspace/alpha/two.txt',
    ]);
    expect(firstPage.has_more).toBe(true);
    expect(firstPage.next).not.toBeNull();

    const secondPage = await body<FilePageBody>(
      await request(
        harness.app,
        'GET',
        `/agents/environments/${environmentId}/files?limit=2&order=asc&page=${firstPage.next ?? ''}`,
      ),
    );
    expect(secondPage.data.map((item) => item.path)).toEqual(['/workspace/gamma/three.txt']);
    expect(secondPage.has_more).toBe(false);

    const filtered = await body<FilePageBody>(
      await request(harness.app, 'GET', `/agents/environments/${environmentId}/files?path=alpha`),
    );
    expect(filtered.data.map((item) => item.path)).toEqual([
      '/workspace/alpha/two.txt',
      '/workspace/alpha/one.txt',
    ]);

    const absolute = encodeURIComponent(harness.workspace);
    const workspaceFiltered = await body<FilePageBody>(
      await request(harness.app, 'GET', `/agents/environments/${environmentId}/files?path=${absolute}`),
    );
    expect(workspaceFiltered.data).toHaveLength(3);

    const outside = await request(
      harness.app,
      'GET',
      `/agents/environments/${environmentId}/files?path=${encodeURIComponent(path.dirname(harness.workspace))}`,
    );
    expect(outside.status).toBe(400);
    expect((await body<ErrorBody>(outside)).error.code).toBe('workspace_not_allowed');
  });

  test('returns 404 for an unknown environment', async () => {
    const response = await request(harness.app, 'GET', '/agents/environments/missing/files');
    expect(response.status).toBe(404);
    expect((await body<ErrorBody>(response)).error.code).toBe('not_found');
  });
});
