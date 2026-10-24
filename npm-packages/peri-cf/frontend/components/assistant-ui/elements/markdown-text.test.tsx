import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  AssistantRuntimeProvider,
  TextMessagePartProvider,
  useExternalStoreRuntime,
  type ThreadMessage,
} from '@assistant-ui/react';
import { MarkdownText } from './markdown-text';

/**
 * Markdown 视觉契约的 DOM 回归：组件只负责语义类名，具体字号/颜色由
 * frontend/styles/chat-markdown.css 承担。这里锁定代码块、表格、图片与安全策略
 * 的接线，避免后续替换 registry 源码时丢失样式钩子。
 */
function render(text: string): string {
  function Harness() {
    const runtime = useExternalStoreRuntime<ThreadMessage>({
      messages: [],
      onNew: async () => {},
    });
    return (
      <AssistantRuntimeProvider runtime={runtime}>
        <TextMessagePartProvider text={text}>
          <MarkdownText />
        </TextMessagePartProvider>
      </AssistantRuntimeProvider>
    );
  }
  return renderToStaticMarkup(<Harness />);
}

describe('markdown text', () => {
  test('keeps the typography, code block, table and image hooks', () => {
    const output = render([
      '# 标题',
      '',
      '正文 **加粗** 和 `inline`。',
      '',
      '```ts',
      'const answer = 42;',
      '```',
      '',
      '| 列一 | 列二 |',
      '| --- | --- |',
      '| a | b |',
      '',
      '![示意图](https://example.com/a.png)',
    ].join('\n'));

    expect(output).toContain('aui-md-h1');
    expect(output).toContain('aui-md-strong');
    expect(output).toContain('aui-md-inline-code');
    expect(output).toContain('aui-code-header-root');
    expect(output).toContain('aui-code-header-language');
    expect(output).toContain('aui-md-pre');
    expect(output).toContain('aui-md-table-wrapper');
    expect(output).toContain('aui-md-th');
    expect(output).toContain('aui-md-td');
    expect(output).toContain('aui-md-img');
    expect(output).toContain('src="https://example.com/a.png"');
  });

  test('escapes raw HTML and drops unsafe link protocols', () => {
    const output = render('<script>alert(1)</script>\n\n[点我](javascript:alert(1))');

    expect(output).not.toContain('<script>');
    expect(output).not.toContain('javascript:');
  });
});
