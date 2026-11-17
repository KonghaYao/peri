import { describe, expect, test } from 'bun:test';
import { ActorError } from '../src/errors.js';
import { EvidenceRecorder } from '../src/evidence/index.js';
import { captureActorError } from './helpers.js';

describe('evidence recorder', () => {
  test('summarizes passed, failed and skipped checks with environment metadata', async () => {
    let now = 1_000;
    const clock = (): number => {
      now += 5;
      return now;
    };
    const recorder = new EvidenceRecorder({
      runId: 'run_test',
      now: clock,
      environment: { node: 'v22.0.0', platform: 'test-os', arch: 'test-arch' },
    });

    const passed = await recorder.check('check_passed', 'Passing check', async () => ({ value: 1 }));
    expect(passed).toMatchObject({
      id: 'check_passed',
      title: 'Passing check',
      status: 'passed',
      durationMs: 5,
      details: { value: 1 },
    });

    const failed = await recorder.check('check_failed', 'Failing check', async () => {
      throw new Error('expected failure');
    });
    expect(failed.status).toBe('failed');
    expect(failed.error).toBe('expected failure');
    expect(failed.details).toBeUndefined();

    const skipped = recorder.skip('check_skipped', 'Skipped check', 'not supported here');
    expect(skipped).toMatchObject({ status: 'skipped', durationMs: 0, note: 'not supported here' });

    const report = recorder.report();
    expect(report.runId).toBe('run_test');
    expect(report.environment).toEqual({ node: 'v22.0.0', platform: 'test-os', arch: 'test-arch' });
    expect(report.summary).toEqual({ passed: 1, failed: 1, skipped: 1 });
    expect(report.checks.map((check) => check.id)).toEqual(['check_passed', 'check_failed', 'check_skipped']);
    expect(report.durationMs).toBe(report.finishedAt - report.startedAt);
  });

  test('an empty report has a zero summary', () => {
    const recorder = new EvidenceRecorder({ runId: 'run_empty', now: () => 7 });
    const report = recorder.report();

    expect(report.summary).toEqual({ passed: 0, failed: 0, skipped: 0 });
    expect(report.checks).toEqual([]);
    expect(report.durationMs).toBe(0);
  });

  test('duplicate check ids are refused', async () => {
    const recorder = new EvidenceRecorder({ runId: 'run_duplicates', now: () => 0 });
    await recorder.check('check_duplicate', 'First', async () => undefined);

    const rejected = await captureActorError(
      recorder.check('check_duplicate', 'Second', async () => undefined),
    );
    expect(rejected.code).toBe('invalid_input');
    expect(() => recorder.skip('check_duplicate', 'Duplicate', 'reason')).toThrow(ActorError);
  });
});
