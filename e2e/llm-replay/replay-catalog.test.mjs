import assert from 'node:assert/strict';
import { test } from 'node:test';
import { REPLAY_CASE_IDS, REPLAY_SLOW_IDS, replayProfile, validateReplayResult } from '../scripts/replay-catalog.mjs';

// Synthetic reporter documents exercise the gate; they are not real business execution receipts.
function reporter(ids = REPLAY_CASE_IDS) {
  const assertions = ids.map(id => id === 'I03'
    ? { title: '同后端双会话取消隔离', ancestorTitles: ['I03 — cancellation isolation'], status: 'passed' }
    : { title: `${id} explicit business case`, ancestorTitles: ['release replay catalog'], status: 'passed' });
  return recount({ success: true, numFailedTestSuites: 0, numTodoTests: 0,
    testResults: [{ assertionResults: assertions }] });
}
function assertions(result) { return result.testResults[0].assertionResults; }
function recount(result) {
  const all = result.testResults.flatMap(suite => suite.assertionResults);
  return Object.assign(result, { numTotalTests: all.length, numPassedTests: all.filter(test => test.status === 'passed').length,
    numFailedTests: all.filter(test => test.status === 'failed').length, numPendingTests: all.filter(test => test.status === 'skipped').length });
}

test('release catalog freezes 94 unique cases and precisely two slow cases', () => {
  assert.equal(REPLAY_CASE_IDS.length, 94);
  assert.equal(new Set(REPLAY_CASE_IDS).size, 94);
  assert.deepEqual(REPLAY_SLOW_IDS, ['B10', 'M09']);
  const verdict = validateReplayResult(reporter(), 'full');
  assert.equal(verdict.passed, 94);
  assert.equal(verdict.skipped, 0);
});

test('full rejects a missing case even when every loaded test passed', () => {
  assert.throws(() => validateReplayResult(reporter(REPLAY_CASE_IDS.filter(id => id !== 'J01')), 'full'), /missing J01/);
});

test('full rejects a skipped business case despite Vitest success', () => {
  const result = reporter(); assertions(result).find(test => test.title.startsWith('J01 ')).status = 'skipped';
  assert.throws(() => validateReplayResult(recount(result), 'full'), /J01=passed, received skipped/);
});

test('duplicate catalog IDs cannot substitute for omitted coverage', () => {
  const result = reporter(); assertions(result).push(structuredClone(assertions(result)[0]));
  assert.throws(() => validateReplayResult(recount(result), 'full'), /duplicate replay case: A01/);
});

test('quick requires all 94 discovered, exactly 92 passed and only B10/M09 skipped', () => {
  const result = reporter();
  for (const assertion of assertions(result)) if (REPLAY_SLOW_IDS.some(id => assertion.title.startsWith(`${id} `))) assertion.status = 'skipped';
  const verdict = validateReplayResult(recount(result), 'quick');
  assert.equal(verdict.passed, 92); assert.equal(verdict.skipped, 2);
  const wrong = structuredClone(result);
  assertions(wrong).find(test => test.title.startsWith('B10 ')).status = 'passed';
  assertions(wrong).find(test => test.title.startsWith('J01 ')).status = 'skipped';
  assert.throws(() => validateReplayResult(recount(wrong), 'quick'), /quick requires/);
  assert.throws(() => validateReplayResult(reporter(REPLAY_CASE_IDS.filter(id => !REPLAY_SLOW_IDS.includes(id))), 'quick'), /missing B10,M09/);
});

test('aggregate counters must agree with actual assertion status', () => {
  const result = reporter(); result.numPassedTests = 93;
  assert.throws(() => validateReplayResult(result, 'full'), /numPassedTests disagrees/);
});

test('case identity accepts explicit ancestry but rejects prose, ranges and conflicts', () => {
  assert.equal(validateReplayResult(reporter(['I03']), 'filtered').passed, 1);
  for (const assertion of [
    { title: 'mentions A01 in prose', ancestorTitles: [] },
    { title: 'unnamed case', ancestorTitles: ['A01-A06 family'] },
    { title: 'A02 wrong case', ancestorTitles: ['A01 explicit ancestor'] },
  ]) {
    const result = reporter(['A01']); Object.assign(assertions(result)[0], assertion);
    assert.throws(() => validateReplayResult(result, 'filtered'), /exactly one catalog case/);
  }
});

test('explicit file and option filters are labeled filtered; scheduling flags preserve gate', () => {
  assert.equal(replayProfile([], false), 'full');
  assert.equal(replayProfile([], true), 'quick');
  assert.equal(replayProfile(['--maxWorkers', '2', '--testTimeout', '300000'], false), 'full');
  assert.equal(replayProfile(['--maxWorkers=2', '--no-file-parallelism'], true), 'quick');
  for (const args of [['tests/replay-v3/cross-lifecycle.test.ts'], ['-t', 'J01 '], ['--exclude', 'tests/replay-v2/*'],
    ['--project', 'debug'], ['--changed'], ['--shard', '1/2']]) {
    assert.equal(replayProfile(args, false), 'filtered');
    assert.equal(replayProfile(args, true), 'filtered');
  }
});

test('filtered remains nonempty and failure-free but does not claim full coverage', () => {
  const result = reporter(['J01', 'J02']); assertions(result)[1].status = 'skipped';
  assert.equal(validateReplayResult(recount(result), 'filtered').passed, 1);
  assert.throws(() => validateReplayResult(reporter([]), 'filtered'), /no replay case actually passed/);
  assertions(result)[0].status = 'failed'; result.success = false;
  assert.throws(() => validateReplayResult(recount(result), 'filtered'), /no replay case actually passed|reported failures/);
});
