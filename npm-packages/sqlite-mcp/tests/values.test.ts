import assert from 'node:assert/strict';
import test from 'node:test';
import { toJsonCell, toJsonRow } from '../src/values.js';

test('keeps ordinary JSON scalars', () => {
  assert.equal(toJsonCell(null), null);
  assert.equal(toJsonCell('text'), 'text');
  assert.equal(toJsonCell(true), true);
  assert.equal(toJsonCell(42), 42);
  assert.equal(toJsonCell(1.5), 1.5);
});

test('keeps safe integers and tags larger integers without precision loss', () => {
  assert.equal(toJsonCell(9007199254740991n), 9007199254740991);
  assert.deepEqual(toJsonCell(9007199254740993n), { type: 'integer', value: '9007199254740993' });
  assert.deepEqual(toJsonCell(-9007199254740993n), { type: 'integer', value: '-9007199254740993' });
});

test('encodes blobs as base64 with their byte length', () => {
  assert.deepEqual(toJsonCell(new Uint8Array([1, 2, 3])), { type: 'blob', base64: 'AQID', byteLength: 3 });
});

test('tags non-finite reals and unsupported values instead of emitting invalid JSON', () => {
  assert.deepEqual(toJsonCell(Number.POSITIVE_INFINITY), { type: 'real', value: 'Infinity' });
  assert.deepEqual(toJsonCell({ hidden: 'x' }), { type: 'unsupported', valueType: 'object' });
});

test('maps full rows by position', () => {
  assert.deepEqual(toJsonRow([1n, new Uint8Array([255]), null]), [
    1,
    { type: 'blob', base64: '/w==', byteLength: 1 },
    null,
  ]);
});
