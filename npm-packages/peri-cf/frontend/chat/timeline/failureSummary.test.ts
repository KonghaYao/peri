import { describe, expect, test } from 'bun:test';
import { toolFailureSummary } from './failureSummary';

describe('toolFailureSummary', () => {
  test('stderr marker text keeps the real error and exit code, not the marker', () => {
    const summary = toolFailureSummary({
      contentText: '[stderr]\ncommand not found\n[Exit code: 127]',
    });

    expect(summary.detail).toBe('command not found');
    expect(summary.exitCode).toBe('127');
    expect(summary.fullText).toBe('command not found');
    expect(summary.detail).not.toContain('[stderr]');
  });

  test('JSON contentText prefers non-empty stderr over stdout', () => {
    const summary = toolFailureSummary({
      contentText: '{"stdout":"","stderr":"permission denied","exitCode":1}',
    });

    expect(summary.detail).toBe('permission denied');
    expect(summary.exitCode).toBe('1');
    expect(summary.fullText).toBe('permission denied');
    expect(summary.detail).not.toContain('{');
    expect(summary.detail).not.toContain('stdout');
  });

  test('JSON stderr wins when stdout also has content', () => {
    const summary = toolFailureSummary({
      contentText: '{"stdout":"working","stderr":"fatal: denied","exitCode":2}',
    });

    expect(summary.detail).toBe('fatal: denied');
    expect(summary.fullText).toBe('fatal: denied\nworking');
    expect(summary.exitCode).toBe('2');
  });

  test('stdout-only marker text still surfaces output and exit code', () => {
    const summary = toolFailureSummary({ contentText: '[stdout]\nready\n[Exit code: 0]' });

    expect(summary.detail).toBe('ready');
    expect(summary.exitCode).toBe('0');
    expect(summary.fullText).toBe('ready');
  });

  test('empty output has no detail and no exit code', () => {
    expect(toolFailureSummary({ contentText: '' })).toEqual({
      detail: null, exitCode: null, fullText: '',
    });
  });

  test('plain multiline text uses the first line as detail and keeps the rest in fullText', () => {
    const summary = toolFailureSummary({ contentText: 'permission denied\nsecond line' });

    expect(summary.detail).toBe('permission denied');
    expect(summary.fullText).toBe('permission denied\nsecond line');
  });

  test('rawOutput exit code wins and stderr stays ahead of stdout', () => {
    const summary = toolFailureSummary({
      contentText: '[stdout]\nworking\n[stderr]\ncommand not found',
      rawOutput: { exitCode: 127 },
    });

    expect(summary.detail).toBe('command not found');
    expect(summary.exitCode).toBe('127');
    expect(summary.fullText).toBe('working\ncommand not found');
  });

  test('rawOutput stdout/stderr keeps stderr priority when contentText is absent', () => {
    const summary = toolFailureSummary({
      rawOutput: { stdout: 'working', stderr: 'permission denied', exitCode: 1 },
    });

    expect(summary.detail).toBe('permission denied');
    expect(summary.exitCode).toBe('1');
    expect(summary.fullText).toBe('permission denied\nworking');
  });

  test('JSON without readable fields never becomes detail', () => {
    const summary = toolFailureSummary({ contentText: '{"foo":"bar"}' });

    expect(summary.detail).toBeNull();
    expect(summary.exitCode).toBeNull();
    expect(summary.fullText).toBe('{"foo":"bar"}');
  });

  test('a JSON line mixed into plain text is not used as detail', () => {
    const summary = toolFailureSummary({
      contentText: '{"stdout":"noise"}\npermission denied',
    });

    expect(summary.detail).toBe('permission denied');
  });
});
