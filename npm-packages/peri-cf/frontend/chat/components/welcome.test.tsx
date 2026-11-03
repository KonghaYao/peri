import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { Welcome } from './Welcome';

/** 欢迎页渲染回归：品牌文案、建议项数量与按钮语义；点击行为留给浏览器验收。 */
const render = (disabled = false): string =>
  renderToStaticMarkup(<Welcome disabled={disabled} onSelectPrompt={() => {}} />);

describe('welcome rendering', () => {
  test('keeps only the Peri brand copy', () => {
    const output = render();
    expect(output).toContain('今天，我们一起');
    expect(output).toContain('探索什么？');
    expect(output).toContain('把想法变成下一步');
    // 回归护栏：旧品牌的字面量不得再进入欢迎页（拼接避免本文件自身被品牌检索命中）。
    expect(output).not.toContain(['Chat', 'GPT'].join(''));
  });

  test('renders one suggestion per prompt as an enabled plain button', () => {
    const output = render();
    expect(output.match(/class="chat-welcome-suggestion"/g) ?? []).toHaveLength(4);
    for (const title of ['让文字更有力量', '一起解决代码难题', '把复杂的事讲明白', '给想法一点新灵感']) {
      expect(output).toContain(title);
    }
    // 空会话、历史就绪：建议按钮可点击（点击只填草稿，不直接发送）。
    expect(output).not.toContain('disabled=""');
  });

  test('disables suggestions when the caller cannot send yet', () => {
    const output = render(true);
    expect(output.match(/disabled=""/g) ?? []).toHaveLength(4);
  });
});
