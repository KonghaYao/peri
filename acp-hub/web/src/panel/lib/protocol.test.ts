import { describe, expect, it } from 'vitest';
import { parse } from './protocol';

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
