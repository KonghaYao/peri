import { parseCLI } from 'vitest/node';

// Changing the release gate requires an explicit catalog edit, not fewer files being discovered.
const groups = { A: 6, B: 12, D: 12, H: 12, I: 3, J: 2, L: 14, M: 12, S: 6, T: 6, U: 9 };
export const REPLAY_CASE_IDS = Object.freeze(Object.entries(groups).flatMap(([prefix, count]) =>
  Array.from({ length: count }, (_, index) => `${prefix}${String(index + 1).padStart(2, '0')}`)));
export const REPLAY_SLOW_IDS = Object.freeze(['B10', 'M09']);
const catalog = new Set(REPLAY_CASE_IDS);
const slow = new Set(REPLAY_SLOW_IDS);

export function replayProfile(args, quick = false) {
  // Reuse Vitest's option arity, so --maxWorkers 2 is never mistaken for a file filter.
  const { filter, options } = parseCLI(['vitest', 'run', ...args]);
  const selectors = ['testNamePattern', 'exclude', 'include', 'project', 'changed', 'related', 'shard', 'tags'];
  const selected = selectors.some(key => options[key] !== undefined && options[key] !== false
    && (!Array.isArray(options[key]) || options[key].length > 0));
  return filter.length || selected ? 'filtered' : quick ? 'quick' : 'full';
}

function caseId(assertion) {
  if (typeof assertion.title !== 'string' || !Array.isArray(assertion.ancestorTitles)) throw new Error('test has no explicit title ancestry');
  const ids = new Set();
  for (const component of [...assertion.ancestorTitles, assertion.title]) {
    // A describe range such as S01-S06 is not a single scenario identifier.
    const match = /^([A-Z][0-9]{2})(?:\s|$|[—:]\s)/u.exec(component);
    if (match) ids.add(match[1]);
  }
  if (ids.size !== 1) throw new Error(`test must identify exactly one catalog case: ${assertion.title}`);
  return [...ids][0];
}

/** Validate actual assertions and aggregate counters independently of Vitest's process exit code. */
export function validateReplayResult(result, profile) {
  if (!['full', 'quick', 'filtered'].includes(profile)) throw new Error(`unknown replay profile: ${profile}`);
  if (!Array.isArray(result.testResults)) throw new Error('missing Vitest assertion results');
  const cases = new Map();
  for (const suite of result.testResults) {
    if (!Array.isArray(suite.assertionResults)) throw new Error('missing test suite assertions');
    for (const assertion of suite.assertionResults) {
      const id = caseId(assertion);
      if (!catalog.has(id)) throw new Error(`unknown replay case: ${id}`);
      if (cases.has(id)) throw new Error(`duplicate replay case: ${id}`);
      if (!['passed', 'failed', 'skipped'].includes(assertion.status)) throw new Error(`unsupported assertion status for ${id}: ${assertion.status}`);
      cases.set(id, assertion.status);
    }
  }
  const values = [...cases.values()];
  const passed = values.filter(value => value === 'passed').length;
  const failed = values.filter(value => value === 'failed').length;
  const skipped = values.filter(value => value === 'skipped').length;
  for (const [field, expected] of Object.entries({ numTotalTests: cases.size, numPassedTests: passed, numFailedTests: failed, numPendingTests: skipped, numTodoTests: 0 })) {
    if (result[field] !== expected) throw new Error(`Vitest ${field} disagrees with actual assertions: ${result[field]} != ${expected}`);
  }
  if (profile !== 'filtered') {
    const missing = REPLAY_CASE_IDS.filter(id => !cases.has(id));
    if (missing.length) throw new Error(`incomplete ${profile} replay catalog; missing ${missing.join(',')}`);
    for (const [id, status] of cases) {
      const expected = profile === 'quick' && slow.has(id) ? 'skipped' : 'passed';
      if (status !== expected) throw new Error(`${profile} requires ${id}=${expected}, received ${status}`);
    }
  }
  if (!passed) throw new Error('no replay case actually passed');
  if (failed || result.success !== true || result.numFailedTestSuites !== 0) throw new Error('Vitest reported failures outside a successful replay gate');
  return { status: 'passed', catalogVersion: 1, catalogCases: REPLAY_CASE_IDS.length,
    expectedPassed: profile === 'full' ? REPLAY_CASE_IDS.length : profile === 'quick' ? REPLAY_CASE_IDS.length - REPLAY_SLOW_IDS.length : null,
    executed: passed + failed, passed, failed, skipped,
    cases: [...cases].sort(([a], [b]) => a.localeCompare(b)).map(([id, status]) => ({ id, status })) };
}
