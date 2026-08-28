import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const INTERFACE_NAME = 'peri:turn-policy/policy@0.1.0';
const EXPECTED_ASSERTIONS = 28;
const PACKAGE_DIR = fileURLToPath(new URL('.', import.meta.url));
const EXPECTED_MODULE_PATH = path.join(PACKAGE_DIR, 'target', 'wasi-p2-node', 'turn-policy.js');
const EXPECTED_CWD = path.dirname(EXPECTED_MODULE_PATH);
const ALLOWED_ENV_KEYS = new Set(['PATH', 'SystemRoot', 'TEMP', 'TMP', 'WINDIR']);
if (process.platform === 'darwin') {
  ALLOWED_ENV_KEYS.add('__CF_USER_TEXT_ENCODING');
  assert.match(process.env.__CF_USER_TEXT_ENCODING, /^0x[0-9A-Fa-f]+:0x[0-9A-Fa-f]+:0x[0-9A-Fa-f]+$/u);
}

assert.deepEqual(Object.keys(process.env).sort(), [...Object.keys(process.env)].filter((key) => ALLOWED_ENV_KEYS.has(key)).sort());
assert.equal(path.resolve(process.cwd()), EXPECTED_CWD);
assert.equal(process.argv.length, 3);

const moduleUrl = new URL(process.argv[2]);
assert.equal(moduleUrl.protocol, 'file:');
assert.equal(fileURLToPath(moduleUrl), EXPECTED_MODULE_PATH);
assert.equal(moduleUrl.href, pathToFileURL(EXPECTED_MODULE_PATH).href);

const generated = await import(moduleUrl.href);
let assertionCount = 0;

function check(actual, expected) {
  assert.deepEqual(actual, expected);
  assertionCount += 1;
}

function checkError(call, expectedPayload) {
  let thrown;
  try {
    call();
  } catch (error) {
    thrown = error;
  }
  assert.notEqual(thrown, undefined, `expected ComponentError(${expectedPayload})`);
  assert.equal(thrown.constructor.name, 'ComponentError');
  assert.equal(thrown.name, 'Error');
  assert.equal(thrown.message, expectedPayload);
  assert.equal(thrown.payload, expectedPayload);
  assert.deepEqual(Object.keys(thrown), []);
  assert.equal(Object.getOwnPropertyDescriptor(thrown, 'payload').enumerable, false);
  assertionCount += 1;
}

check(Object.keys(generated), ['_util', INTERFACE_NAME, 'policy']);
const policy = generated[INTERFACE_NAME];
check(policy === generated.policy, true);
check(Object.keys(policy), ['classifyContent', 'selectCompact']);

check(policy.classifyContent({ tag: 'text', val: '' }), 'empty');
check(policy.classifyContent({ tag: 'text', val: '   ' }), 'non-empty');
check(policy.classifyContent({ tag: 'blocks', val: 0 }), 'empty');
check(policy.classifyContent({ tag: 'blocks', val: 1 }), 'non-empty');
check(policy.classifyContent({ tag: 'raw', val: 0 }), 'empty');
check(policy.classifyContent({ tag: 'raw', val: 1 }), 'non-empty');

check(policy.selectCompact(0, 1), 'skip');
check(policy.selectCompact(1, 0), 'micro');
check(policy.selectCompact(0, 0), 'micro');
check(policy.selectCompact(1, 1), 'micro');
check(policy.selectCompact(0.5, 0.5), 'micro');
check(policy.selectCompact(0.499, 0.5), 'skip');
check(policy.selectCompact(0.501, 0.5), 'micro');

checkError(() => policy.selectCompact(Number.NaN, 0.5), 'budget-not-finite');
checkError(() => policy.selectCompact(Number.POSITIVE_INFINITY, 0.5), 'budget-not-finite');
checkError(() => policy.selectCompact(Number.NEGATIVE_INFINITY, 0.5), 'budget-not-finite');
checkError(() => policy.selectCompact(-Number.EPSILON, 0.5), 'budget-out-of-range');
checkError(() => policy.selectCompact(1 + Number.EPSILON, 0.5), 'budget-out-of-range');
checkError(() => policy.selectCompact(0.5, Number.NaN), 'micro-threshold-not-finite');
checkError(() => policy.selectCompact(0.5, Number.POSITIVE_INFINITY), 'micro-threshold-not-finite');
checkError(() => policy.selectCompact(0.5, Number.NEGATIVE_INFINITY), 'micro-threshold-not-finite');
checkError(() => policy.selectCompact(0.5, -Number.EPSILON), 'micro-threshold-out-of-range');
checkError(() => policy.selectCompact(0.5, 1 + Number.EPSILON), 'micro-threshold-out-of-range');
checkError(() => policy.selectCompact(Number.NaN, 2), 'budget-not-finite');
checkError(() => policy.selectCompact(-Number.EPSILON, Number.NaN), 'budget-out-of-range');

assert.equal(assertionCount, EXPECTED_ASSERTIONS);
process.stdout.write(`WASI_P2_ASSERTIONS=${assertionCount}\n`);
