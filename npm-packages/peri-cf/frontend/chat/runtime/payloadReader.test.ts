import { describe, expect, test } from 'bun:test';
import { PayloadReadError } from '@peri-code/sdk/view';
import { chatPayloadReader, loadPayloadValue, type PayloadSource } from './payloadReader';

function source(read: (id: string) => Promise<{ id: string; bytes: number; json: string }>): PayloadSource {
  return { readPayloadById: (id) => read(id) };
}

const ref = { id: 'ref-1', bytes: 2 };

describe('HTTP payload reader', () => {
  test('passes the reference id to the controller and returns the payload', async () => {
    const reader = chatPayloadReader(() => source(async (id) => ({ id, bytes: 2, json: '{}' })));
    expect(await reader({ id: 'ref-1' })).toEqual({ id: 'ref-1', bytes: 2, json: '{}' });
  });

  test('reports unavailable when no chat is selected', async () => {
    const reader = chatPayloadReader(() => undefined);
    await expect(reader({ id: 'ref-1' })).rejects.toMatchObject({
      name: 'PayloadReadError', code: 'unavailable',
    });
  });

  test('propagates SDK payload error codes from the HTTP client', async () => {
    for (const code of ['too_large', 'integrity_error', 'aborted', 'transport_error'] as const) {
      const reader = chatPayloadReader(() => source(async () => { throw new PayloadReadError(code, 'reason'); }));
      await expect(reader({ id: 'ref-1' })).rejects.toMatchObject({ code });
    }
  });

  test('a missing replica reference fails explicitly instead of guessing bytes', async () => {
    const reader = chatPayloadReader(() => ({
      readPayloadById: async () => { throw new PayloadReadError('unavailable', '全文引用已不在当前会话中。'); },
    }));
    await expect(reader({ id: 'lost' })).rejects.toMatchObject({ code: 'unavailable' });
  });
});

describe('payload value helpers', () => {
  test('validates JSON and byte count through the SDK helper', async () => {
    const reader = chatPayloadReader(() => source(async () => ({ id: 'ref-1', bytes: 2, json: '{}' })));
    expect(await loadPayloadValue(ref, reader)).toEqual({ ok: true, value: {} });
  });

  test('reports an integrity error when the server body does not match the reference', async () => {
    const reader = chatPayloadReader(() => source(async () => ({ id: 'ref-1', bytes: 999, json: '{}' })));
    const result = await loadPayloadValue(ref, reader);
    expect(result).toMatchObject({ ok: false });
    if (!result.ok) expect(result.error).toContain('字节数');
  });
});
