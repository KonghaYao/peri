import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { MarkdownText } from './MarkdownText';

function render(text: string): string {
  return renderToStaticMarkup(<MarkdownText text={text} />);
}

describe('MarkdownText', () => {
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
    expect(output).toContain('aui-code-header-root');
    expect(output).toContain('aui-code-header-language');
    expect(output).toContain('>ts<');
    expect(output).toContain('aui-md-table-wrapper');
    expect(output).toContain('<th');
    expect(output).toContain('<td');
  });

  test('escapes raw HTML instead of executing it', () => {
    const output = render('<script>alert(1)</script>');
    expect(output).not.toContain('<script>');
    expect(output).toContain('&lt;script&gt;');
  });

  test('does not leak the react-markdown node prop into the DOM', () => {
    const output = render('# 标题\n\n正文\n\n- 一\n- 二\n\n[链接](https://example.com)');
    expect(output).not.toContain('node="[object Object]"');
    expect(output).not.toMatch(/\snode=/);
  });

  test('keeps javascript: links inert as an empty href', () => {
    const output = render('[点我](javascript:alert(1))');
    expect(output).not.toContain('javascript:');
    expect(output).toContain('href=""');
  });

  test('does not render images with an empty or unsafe src', () => {
    expect(render('![空]()')).not.toContain('<img');
    expect(render('![不安全](javascript:alert(1))')).not.toContain('<img');
    expect(render('![正常](https://example.com/a.png)')).toContain('src="https://example.com/a.png"');
  });
});
