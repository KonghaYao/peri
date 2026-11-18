import assert from 'node:assert/strict';
import test from 'node:test';
import { DEFAULT_PORT } from '../src/constants.js';
import { parseArgs, USAGE } from '../src/args.js';

test('uses the default loopback port when --port is absent', () => {
  assert.deepEqual(parseArgs(['/tmp/db.sqlite']), { kind: 'run', databasePath: '/tmp/db.sqlite', port: DEFAULT_PORT });
  assert.equal(DEFAULT_PORT, 31339);
});

test('accepts ephemeral and explicit port forms', () => {
  assert.deepEqual(parseArgs(['db.sqlite', '--port', '0']), { kind: 'run', databasePath: 'db.sqlite', port: 0 });
  assert.deepEqual(parseArgs(['db.sqlite', '--port=65535']), { kind: 'run', databasePath: 'db.sqlite', port: 65535 });
});

test('help wins and prints usage without a database', () => {
  assert.deepEqual(parseArgs(['--help']), { kind: 'help' });
  assert.deepEqual(parseArgs(['db.sqlite', '-h']), { kind: 'help' });
  assert.match(USAGE, /sqlite-mcp <数据库路径> \[--port <端口>\]/);
  assert.match(USAGE, /127\.0\.0\.1/);
  assert.match(USAGE, /WAL/);
  assert.match(USAGE, /可信本机/);
});

test('rejects missing path, extra paths, unknown options and invalid ports', () => {
  assert.equal(parseArgs([]).kind, 'error');
  assert.equal(parseArgs(['a.sqlite', 'b.sqlite']).kind, 'error');
  assert.equal(parseArgs(['db.sqlite', '--port']).kind, 'error');
  assert.equal(parseArgs(['db.sqlite', '--port', '65536']).kind, 'error');
  assert.equal(parseArgs(['db.sqlite', '--port', '1e3']).kind, 'error');
  assert.equal(parseArgs(['db.sqlite', '--unknown']).kind, 'error');
});

test('supports -- before a dash-prefixed database path', () => {
  assert.deepEqual(parseArgs(['--', '-local.sqlite']), {
    kind: 'run',
    databasePath: '-local.sqlite',
    port: DEFAULT_PORT,
  });
});
