import { describe, expect, test } from 'bun:test';
import { ActorError } from '../src/errors.js';
import {
  parseInputId,
  parseResourceId,
  parseResourceScope,
  parseRootSessionId,
  parseRuntimeId,
} from '../src/identity.js';

describe('identity', () => {
  test('accepts opaque printable values', () => {
    expect<string>(parseRuntimeId('rt_01HXYZ')).toBe('rt_01HXYZ');
  });

  test('preserves the exact value', () => {
    expect<string>(parseResourceScope('tenant:alpha')).toBe('tenant:alpha');
    expect<string>(parseRootSessionId('session_123')).toBe('session_123');
  });

  test('rejects empty, blank and control-character values', () => {
    for (const value of ['', ' ', 'a b', 'a\tb', 'a\nb', '\u0000']) {
      expect(() => parseResourceId(value)).toThrow(ActorError);
      expect(() => parseRuntimeId(value)).toThrow(ActorError);
    }
  });

  test('rejects overlong values', () => {
    expect(() => parseInputId('x'.repeat(201))).toThrow(ActorError);
  });

  test('supports Unicode identifiers without whitespace', () => {
    expect<string>(parseRuntimeId('运行_一')).toBe('运行_一');
    expect<string>(parseResourceScope('租户:甲')).toBe('租户:甲');
  });
});
