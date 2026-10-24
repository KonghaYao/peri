import { describe, expect, test } from 'bun:test';
import type { Transport } from '@peri-code/sdk/portable';
import { initializeTransport } from './bootstrap';

function fakeTransport(response: unknown = { protocolVersion: 1 }) {
  const calls: { method: string; params?: unknown }[] = [];
  const transport = {
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params });
      return response;
    },
  } as unknown as Transport;
  return { transport, calls };
}

describe('initializeTransport', () => {
  test('declares history pagination so session/load replays the effective set', async () => {
    const { transport, calls } = fakeTransport();
    await initializeTransport(transport);

    expect(calls).toHaveLength(1);
    expect(calls[0]!.method).toBe('initialize');
    const params = calls[0]!.params as {
      clientCapabilities: { _meta: Record<string, unknown> };
    };
    expect(params.clientCapabilities._meta['peri.historyPagination']).toBe(true);
    expect(params.clientCapabilities._meta['peri.replay']).toBe(true);
  });

  test('rejects an unsupported protocol version', async () => {
    const { transport } = fakeTransport({ protocolVersion: 2 });
    await expect(initializeTransport(transport)).rejects.toThrow('Unsupported ACP protocol version');
  });
});
