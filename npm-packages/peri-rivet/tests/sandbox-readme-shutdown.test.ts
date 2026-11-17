import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

/**
 * The README is the copy users paste into their host process. Its shutdown block must carry the
 * same failure semantics as `examples/sandbox-mcp.mjs`: an unconfirmed owner close defers the
 * runtime sweep and turns into a failure exit, and client/registry cleanup always runs.
 */

type ReadmeFinally = (...values: unknown[]) => Promise<void>;

type Mode = 'owner-incomplete' | 'owner-throws' | 'runtime-throws' | 'confirmed' | 'release-fails';

interface Observed {
  readonly outcome: 'resolved' | 'rejected';
  readonly exitCode: number;
  readonly calls: readonly string[];
  readonly messages: readonly string[];
}

const README_SOURCE = readFileSync(new URL('../README.md', import.meta.url), 'utf8');

function sandboxSnippet(): string {
  const snippets = [...README_SOURCE.matchAll(/```ts\n([\s\S]*?)\n```/g)].map((match) => match[1]);
  const snippet = snippets.find((source) => source?.includes('const sbx = client.sandbox.getOrCreate'));
  if (snippet === undefined) {
    throw new Error('README no longer contains the runnable Sandbox MCP host snippet');
  }
  return snippet;
}

function readmeFinally(): ReadmeFinally {
  const parts = sandboxSnippet().split('} finally {\n');
  const tail = parts[1]?.trimEnd();
  if (parts.length !== 2 || tail === undefined || !tail.endsWith('}')) {
    throw new Error('the README Sandbox MCP finally block shape changed');
  }
  // The block is TypeScript in the README; transpile it instead of maintaining a JS copy here.
  const body = new Bun.Transpiler({ loader: 'ts' }).transformSync(tail.slice(0, -1));
  const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
    ...args: string[]
  ) => ReadmeFinally;
  return new AsyncFunction(
    'held',
    'client',
    'holder',
    'token',
    'workspace',
    'owner',
    'runtime',
    'registry',
    'process',
    'console',
    body,
  );
}

async function runCleanup(mode: Mode): Promise<Observed> {
  const cleanup = readmeFinally();
  const calls: string[] = [];
  const messages: string[] = [];
  const processStub = { exitCode: 0 };
  const client = {
    sandbox: {
      getOrCreate: () => ({
        release: async () => {
          calls.push('release');
          if (mode === 'release-fails') throw new Error('synthetic release failure');
          return { ok: true };
        },
      }),
    },
    dispose: async () => {
      calls.push('client');
    },
  };
  const owner = {
    beginShutdown: () => {
      calls.push('shutdown');
    },
    closeAll: async () => {
      calls.push('owner');
      if (mode === 'owner-throws') throw new Error('synthetic owner close failure');
      return { complete: mode !== 'owner-incomplete', pending: mode === 'owner-incomplete' ? 1 : 0 };
    },
  };
  const runtime = {
    closeAll: async () => {
      calls.push('runtime');
      if (mode === 'runtime-throws') throw new Error('synthetic runtime close failure');
    },
  };
  const registry = {
    shutdown: async () => {
      calls.push('registry');
    },
  };
  const outcome = await cleanup(
    true,
    client,
    'sandbox-mcp:host',
    'synthetic-token',
    'synthetic-workspace',
    owner,
    runtime,
    registry,
    processStub,
    { error: (message: unknown) => messages.push(String(message)) },
  ).then(
    () => 'resolved' as const,
    () => 'rejected' as const,
  );
  return { outcome, exitCode: processStub.exitCode, calls, messages };
}

describe('README Sandbox MCP shutdown contract', () => {
  test('an unconfirmed owner close defers runtime.closeAll and exits non-zero', async () => {
    const observed = await runCleanup('owner-incomplete');
    expect(observed.outcome).toBe('resolved');
    expect(observed.calls).toEqual(['release', 'shutdown', 'owner', 'client', 'registry']);
    expect(observed.exitCode).toBe(1);
    expect(observed.messages).toContain('host resources incomplete: 1');
    expect(observed.messages).toContain('runtime closeAll deferred: host resources are not confirmed');
  });

  test('a throwing owner close still disposes the client and shuts down the registry', async () => {
    const observed = await runCleanup('owner-throws');
    expect(observed.outcome).toBe('resolved');
    expect(observed.calls).toEqual(['release', 'shutdown', 'owner', 'client', 'registry']);
    expect(observed.exitCode).toBe(1);
    expect(observed.messages).toContain('host close failed: Error');
  });

  test('a throwing runtime.closeAll is a failure but never skips the final cleanup', async () => {
    const observed = await runCleanup('runtime-throws');
    expect(observed.outcome).toBe('resolved');
    expect(observed.calls).toEqual(['release', 'shutdown', 'owner', 'runtime', 'client', 'registry']);
    expect(observed.exitCode).toBe(1);
    expect(observed.messages).toContain('runtime closeAll failed: Error');
  });

  test('a confirmed owner close runs the runtime sweep and exits zero', async () => {
    const observed = await runCleanup('confirmed');
    expect(observed.outcome).toBe('resolved');
    expect(observed.calls).toEqual(['release', 'shutdown', 'owner', 'runtime', 'client', 'registry']);
    expect(observed.exitCode).toBe(0);
    expect(observed.messages).toHaveLength(0);
  });

  test('a failing release is visible without blocking the confirmed host close', async () => {
    const observed = await runCleanup('release-fails');
    expect(observed.outcome).toBe('resolved');
    expect(observed.calls).toEqual(['release', 'shutdown', 'owner', 'runtime', 'client', 'registry']);
    expect(observed.exitCode).toBe(1);
    expect(observed.messages).toContain('release failed: Error');
  });
});
