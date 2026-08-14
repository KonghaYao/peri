import { describe, expect, it } from 'vitest';
import { CAP_PROMPT_DELIVERY_V2, parse, subscribe } from './protocol';

describe('downstream protocol envelope parsing', () => {
  it('accepts a structurally valid known frame', () => {
    expect(parse('{"t":"ready","projectionVersions":{}}')).toEqual({
      t: 'ready', projectionVersions: {},
    });
  });

  it('keeps unknown string tags for forward-compatible store dispatch', () => {
    expect(parse('{"t":"future.capability","version":2}')).toEqual({
      t: 'future.capability', version: 2,
    });
  });

  it.each([
    ['ack with a numeric command identity', '{"t":"action_ack","commandId":7,"status":"committed"}'],
    ['ack with an unknown terminal status', '{"t":"action_ack","commandId":"c","status":"done"}'],
    ['ack with a numeric chat identity', '{"t":"action_ack","commandId":"c","status":"committed","chatId":7}'],
    ['error with a string retry flag', '{"t":"action_error","commandId":"c","code":"INVALID_STATE","message":"safe","retryable":"false"}'],
    ['Yjs update with invalid base64', '{"t":"ysync.update","doc":"hub:registry","update":"***"}'],
    ['Yjs update with an arbitrary document id', '{"t":"ysync.update","doc":"other:secret","update":"AAAA"}'],
    ['ready with nonnumeric projection version', '{"t":"ready","projectionVersions":{"hub:registry":"2"}}'],
    ['ready with an arbitrary document id', '{"t":"ready","projectionVersions":{"other:secret":2}}'],
  ])('rejects malformed known frame: %s', (_label, source) => {
    expect(parse(source)).toBeNull();
  });

  it('accepts exact wire shapes and preserves additive optional fields', () => {
    expect(parse('{"t":"action_ack","commandId":"c","status":"duplicate","sessionId":"s","chatId":"chat","futureField":true}'))
      .toMatchObject({ t: 'action_ack', commandId: 'c', status: 'duplicate', sessionId: 's', chatId: 'chat', futureField: true });
    expect(parse('{"t":"ysync.update","doc":"hub:registry","update":"AAAA","projectionVersion":2}'))
      .toEqual({ t: 'ysync.update', doc: 'hub:registry', update: 'AAAA', projectionVersion: 2 });
  });

  it('declares prompt delivery capability and validates the negotiated echo', () => {
    expect(subscribe(['hub:registry'])).toEqual({
      t: 'ysync.subscribe',
      docs: ['hub:registry'],
      clientCapabilities: [CAP_PROMPT_DELIVERY_V2],
    });
    expect(parse('{"t":"ready","projectionVersions":{},"negotiatedCapabilities":["prompt-delivery-v2"]}'))
      .toMatchObject({ negotiatedCapabilities: [CAP_PROMPT_DELIVERY_V2] });
    expect(parse('{"t":"ready","projectionVersions":{},"negotiatedCapabilities":[7]}')).toBeNull();
  });

  it('decodes body-free prompt recovery evidence and rejects false runtime claims', () => {
    expect(parse('{"t":"prompt_status","commandId":"q1","sessionId":"s1","runtimeRestored":false,"truncated":false,"evidenceIncomplete":false,"prompts":[{"commandId":"p1","status":"delivery_unknown","createdAt":"2026-08-14T00:00:00Z","updatedAt":"2026-08-14T00:00:01Z"}]}'))
      .toMatchObject({ t: 'prompt_status', sessionId: 's1', runtimeRestored: false, prompts: [{ commandId: 'p1', status: 'delivery_unknown' }] });
    expect(parse('{"t":"prompt_status","commandId":"q1","sessionId":"s1","runtimeRestored":true,"truncated":false,"evidenceIncomplete":false,"prompts":[]}')).toBeNull();
    expect(parse('{"t":"prompt_status","commandId":"q1","sessionId":"s1","runtimeRestored":false,"truncated":false,"evidenceIncomplete":false,"prompts":[{"commandId":"p1","status":"completed","createdAt":"x","updatedAt":"x","message":"secret"}]}')).toBeNull();
    expect(parse('{"t":"prompt_status","commandId":"q1","sessionId":"s1","runtimeRestored":false,"truncated":false,"evidenceIncomplete":false,"prompts":[],"message":"secret"}')).toBeNull();
  });

  it.each([
    ['invalid JSON', '{'],
    ['null', 'null'],
    ['array', '[{"t":"ready"}]'],
    ['string primitive', '"ready"'],
    ['number primitive', '7'],
    ['missing tag', '{"projectionVersions":{}}'],
    ['non-string tag', '{"t":7}'],
    ['empty tag', '{"t":"  "}'],
  ])('rejects %s without throwing', (_label, source) => {
    expect(() => parse(source)).not.toThrow();
    expect(parse(source)).toBeNull();
  });
});
