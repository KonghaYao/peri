import { describe, expect, test } from 'bun:test';
import { PayloadReadError } from '@peri-code/sdk/view';
import { commandPayloadReader, loadPayloadText, type PayloadCommandSource } from './payloadReader';

/** 载荷读取端点：命令通道映射、失败码与展示格式化（自 messageStatus.test.tsx 迁移，逻辑保持原样）。 */
describe('payload full-text entry point', () => {
  const encoder = new TextEncoder();
  const json = JSON.stringify({ contentText: '全文' });
  const bytes = encoder.encode(json).byteLength;

  function source(readPayload: PayloadCommandSource['readPayload']): PayloadCommandSource { return { readPayload }; }

  test('command reader reads the payload through the session command channel', async () => {
    const refs: string[] = [];
    const reader = commandPayloadReader(() => source(async (refId) => {
      refs.push(refId);
      return { ok: true, payload: { id: refId, bytes, json } };
    }));
    await expect(reader({ id: 'gen:1' })).resolves.toEqual({ id: 'gen:1', bytes, json });
    expect(refs).toEqual(['gen:1']);
  });

  test('command reader maps rejected failure codes onto the SDK contract', async () => {
    for (const code of ['unauthorized', 'unavailable', 'too_large', 'integrity_error', 'aborted', 'transport_error'] as const) {
      const reader = commandPayloadReader(() => source(async () => ({ ok: false, disposition: 'rejected', code, message: 'reason' })));
      const failure = await reader({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
      expect(failure.code, code).toBe(code);
      expect(failure.message).toBe('reason');
    }
  });

  test('unknown rejected codes and missing payloads are contract mismatches, not transport failures', async () => {
    const unknown = commandPayloadReader(() => source(async () => ({ ok: false, disposition: 'rejected', code: 'busy', message: 'reason' })));
    const failure = await unknown({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string };
    expect(failure.code).toBe('integrity_error');
    const empty = commandPayloadReader(() => source(async () => ({ ok: true })));
    expect((await empty({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string }).code).toBe('integrity_error');
  });

  test('command disposition takes precedence over the failure code', async () => {
    const notSent = commandPayloadReader(() => source(async () => ({ ok: false, disposition: 'not_sent', code: 'too_large', message: '未提交' })));
    const notSentFailure = await notSent({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
    expect(notSentFailure.code).toBe('unavailable');
    expect(notSentFailure.message).toBe('未提交');

    const unknown = commandPayloadReader(() => source(async () => ({ ok: false, disposition: 'unknown', code: 'unavailable', message: '状态未知' })));
    const unknownFailure = await unknown({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
    expect(unknownFailure.code).toBe('transport_error');
    expect(unknownFailure.message).toBe('状态未知');
  });

  test('an unsynced session reports unavailable instead of fabricating content', async () => {
    const reader = commandPayloadReader(() => undefined);
    const failure = await reader({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
    expect(failure.code).toBe('unavailable');
    expect(failure.message).toContain('会话尚未就绪');
  });

  test('readPayload surfaces client-side command failures through the SDK helper', async () => {
    const reader = commandPayloadReader(() => source(async () => ({ ok: false, disposition: 'unknown', code: 'transport_error', message: '载荷读取失败' })));
    const result = await loadPayloadText({ id: 'gen:1', bytes }, reader);
    expect(result).toEqual({ ok: false, error: '载荷读取失败' });
  });

  test('formats the validated public JSON for display', async () => {
    const json = JSON.stringify({ contentText: '完整内容' });
    const ref = { id: 'p1', bytes: new TextEncoder().encode(json).byteLength };
    const result = await loadPayloadText(ref, async () => ({ id: 'p1', bytes: ref.bytes, json }));
    expect(result).toEqual({ ok: true, text: JSON.stringify({ contentText: '完整内容' }, null, 2) });
  });

  test('surfaces the P3 placeholder reason without fabricating content', async () => {
    const result = await loadPayloadText({ id: 'p1', bytes: 4 }, async () => {
      throw new PayloadReadError('unavailable', '全文读取端点尚未提供（等待存储方案批准，P3）。');
    });
    expect(result).toEqual({ ok: false, error: '全文读取端点尚未提供（等待存储方案批准，P3）。' });
  });

  test('maps size and integrity failures to explicit reasons', async () => {
    const tooLarge = await loadPayloadText({ id: 'p1', bytes: 2_000_000 }, async () => ({ id: 'p1', bytes: 2, json: '{}' }));
    expect(tooLarge.ok).toBe(false);
    const broken = await loadPayloadText({ id: 'p1', bytes: 2 }, async () => ({ id: 'p1', bytes: 5, json: '{}' }));
    expect(broken).toEqual({ ok: false, error: expect.stringContaining('字节数') });
  });
});
