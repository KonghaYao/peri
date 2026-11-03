import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { StreamdownText } from './StreamdownText';

function render(text: string, streaming = false): string {
  return renderToStaticMarkup(<StreamdownText text={text} streaming={streaming} />);
}

describe('StreamdownText', () => {
  test('renders code language and GFM tables', () => {
    const output = render([
      '```ts',
      'const answer = 42;',
      '```',
      '',
      '| 列一 | 列二 |',
      '| --- | --- |',
      '| a | b |',
    ].join('\n'));

    expect(output).toContain('data-slot="peri-markdown"');
    expect(output).toContain('data-streamdown="code-block"');
    expect(output).toContain('>ts<');
    expect(output).toContain('data-streamdown="table"');
    expect(output).toContain('<th');
    expect(output).toContain('<td');
  });

  test('does not execute raw HTML', () => {
    const output = render('<script>alert(1)</script>');
    expect(output).not.toContain('<script>');
    expect(output).not.toContain('alert(1)');
  });

  test('does not leak the node prop into the DOM', () => {
    const output = render('# 标题\n\n正文\n\n- 一\n- 二\n\n[链接](https://example.com)');
    expect(output).not.toMatch(/\snode=/);
  });

  test('blocks javascript: links', () => {
    const output = render('[点我](javascript:alert(1))');
    expect(output).not.toContain('javascript:');
  });

  test('does not render images with an empty or unsafe src', () => {
    expect(render('![空]()')).not.toContain('<img');
    expect(render('![不安全](javascript:alert(1))')).not.toContain('<img');
    expect(render('![正常](https://example.com/a.png)')).toContain('src="https://example.com/a.png"');
  });

  test('completes an unterminated fenced block while streaming', () => {
    const output = render('```ts\nconst answer = 42;', true);
    expect(output).toContain('data-streamdown="code-block"');
    expect(output).toContain('const answer = 42;');
  });
});
