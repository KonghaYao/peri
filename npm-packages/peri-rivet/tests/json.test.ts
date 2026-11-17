import { describe, expect, test } from 'bun:test';
import { ActorError } from '../src/errors.js';
import {
  assertJsonObject,
  assertJsonValue,
  canonicalJson,
  type JsonObject,
  jsonByteLength,
} from '../src/json.js';

describe('JSON boundary', () => {
  test('canonical JSON orders object keys recursively', () => {
    expect(canonicalJson({ b: 2, a: { d: 4, c: [3, 1] } })).toBe('{"a":{"c":[3,1],"d":4},"b":2}');
  });

  test('JSON byte length counts UTF-8 bytes, not code units', () => {
    expect(jsonByteLength('a')).toBe(1);
    expect(jsonByteLength('é')).toBe(2);
    expect(jsonByteLength('中')).toBe(3);
  });

  test('rejects values JSON cannot represent', () => {
    expect(() => assertJsonValue({ missing: undefined })).toThrow(ActorError);
    expect(() => assertJsonValue(Number.NaN)).toThrow(ActorError);
    expect(() => assertJsonValue(new Date())).toThrow(ActorError);
    expect(() => assertJsonValue(new Map())).toThrow(ActorError);
    expect(() => assertJsonValue([1, undefined, 3])).toThrow(ActorError);
  });

  test('rejects cycles instead of overflowing the stack', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    expect(() => assertJsonValue(cyclic)).toThrow(ActorError);
  });

  test('enforces a depth limit', () => {
    let value: unknown = null;
    for (let depth = 0; depth < 40; depth += 1) {
      value = { nested: value };
    }
    expect(() => assertJsonValue(value)).toThrow(ActorError);
  });

  test('assertJsonObject accepts null-prototype objects and rejects arrays', () => {
    const value = Object.create(null) as Record<string, unknown>;
    value.ok = true;
    expect(() => assertJsonObject(value)).not.toThrow();
    expect(() => assertJsonObject([1, 2])).toThrow(ActorError);
  });

  test('canonical JSON round-trips through JSON.parse', () => {
    const value: JsonObject = { z: [true, null, 3.5], a: 'text' };
    expect(JSON.parse(canonicalJson(value))).toEqual(value);
  });
});
