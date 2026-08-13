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
