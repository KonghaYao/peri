import { describe, expect, test } from 'bun:test';
import { toolFailureSummary } from './failureSummary';
import { toolResultPresentation } from './toolPresentation';
import {
  exitCodeOf, jsonExitCodeOf, rawOutputText, readableText, sectionsOf,
} from './toolText';

/** 折叠态文本与失败态全文都应归一到同一组内容行（顺序由各自策略决定）。 */
function contentLines(text: string): string[] {
  return sectionsOf(text).sections.flatMap((section) => section.lines).filter(Boolean);
}

describe('tool text extraction', () => {
  test('empty values have no text and no exit code', () => {
    expect(exitCodeOf(undefined)).toBeNull();
    expect(exitCodeOf(null)).toBeNull();
    expect(exitCodeOf({})).toBeNull();
    expect(rawOutputText(undefined)).toBe('');
    expect(readableText(undefined)).toBe('');
    expect(sectionsOf('')).toEqual({ sections: [], exitCodes: [] });
    expect(jsonExitCodeOf({})).toBeNull();
  });

  test('non-string values are flattened without dumping JSON', () => {
    expect(rawOutputText(42)).toBe('42');
    expect(rawOutputText(true)).toBe('true');
    expect(rawOutputText(['a', { stdout: 'b' }])).toBe('a\nb');
    expect(readableText({ message: 'boom', stdout: 'ignored' })).toBe('boom');
    expect(rawOutputText({ stdout: 'out', stderr: 'err' })).toBe('out\nerr');
    expect(exitCodeOf(1.5)).toBeNull();
    expect(jsonExitCodeOf({ exitCode: 1.5 })).toBeNull();
  });

  test('markers only route streams and exit codes', () => {
    const extracted = sectionsOf('[stderr]\ncommand not found\n[Exit code: 127]');
    expect(extracted.sections).toEqual([{ stream: 'stderr', lines: ['command not found'] }]);
    expect(extracted.exitCodes).toEqual(['127']);
    expect(exitCodeOf('[Exit code: 127]')).toBe('127');
    expect(jsonExitCodeOf({ code: ' 7 ' })).toBe('7');
  });

  test('structured rawOutput agrees on exit code and content in both states', () => {
    const rawOutput = { stdout: 'working', stderr: 'permission denied', exitCode: 1 };
    const collapsed = toolResultPresentation({ rawOutput });
    const failure = toolFailureSummary({ rawOutput });

    expect(collapsed.exitCode).toBe('1');
    expect(failure.exitCode).toBe('1');
    expect(contentLines(collapsed.text).sort()).toEqual(contentLines(failure.fullText).sort());
    // 折叠态按 stdout→stderr 拼接；失败态 stderr 优先，两者内容相同、顺序不同。
    expect(collapsed.text).toBe('working\npermission denied');
    expect(failure.fullText).toBe('permission denied\nworking');
  });

  test('marker text agrees on exit code and content after normalization', () => {
    const rawOutput = '[stdout]\nworking\n[stderr]\ncommand not found\n[Exit code: 127]';
    const collapsed = toolResultPresentation({ rawOutput });
    const failure = toolFailureSummary({ rawOutput });

    expect(collapsed.exitCode).toBe('127');
    expect(failure.exitCode).toBe('127');
    expect(contentLines(collapsed.text)).toEqual(['working', 'command not found']);
    expect(contentLines(collapsed.text)).toEqual(contentLines(failure.fullText));
  });

  test('numeric rawOutput agrees on text and exit code', () => {
    const collapsed = toolResultPresentation({ rawOutput: 42 });
    const failure = toolFailureSummary({ rawOutput: 42 });

    expect(collapsed.text).toBe('42');
    expect(failure.fullText).toBe('42');
    expect(collapsed.exitCode).toBe('42');
    expect(failure.exitCode).toBe('42');
  });

  test('empty rawOutput has no exit code in either state', () => {
    const collapsed = toolResultPresentation({ rawOutput: { stdout: '', stderr: '' } });
    const failure = toolFailureSummary({ rawOutput: { stdout: '', stderr: '' } });
    expect(collapsed.exitCode).toBeNull();
    expect(failure.exitCode).toBeNull();
    expect(failure.fullText).toBe('');
  });

  test('very long output keeps the tail exit code and full text in both states', () => {
    const payload = 'x'.repeat(50_000);
    const rawOutput = `[stdout]\n${payload}\n[Exit code: 3]`;
    const collapsed = toolResultPresentation({ rawOutput });
    const failure = toolFailureSummary({ rawOutput });

    expect(collapsed.exitCode).toBe('3');
    expect(failure.exitCode).toBe('3');
    expect(collapsed.text.length).toBeGreaterThan(payload.length);
    expect(failure.fullText).toBe(payload);
  });
});
