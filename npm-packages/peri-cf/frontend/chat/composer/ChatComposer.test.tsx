import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  ChatComposer, isSubmitKey, nextDraftAfterSend, type ChatComposerProps,
} from './ChatComposer';
import { MAX_COMPOSER_CHARS } from './composerLimits';

function render(overrides: Partial<ChatComposerProps> = {}): string {
  return renderToStaticMarkup(
    <ChatComposer
      draft="草稿"
      onDraftChange={() => {}}
      canSend
      canStop={false}
      stopping={false}
      onSend={async () => ({ delivered: true, sessionId: 'chat-test' })}
      onStop={async () => true}
      focusSignal={0}
      {...overrides}
    />,
  );
}

describe('ChatComposer keyboard and submit semantics', () => {
  test('Enter submits, Shift+Enter and IME composition do not', () => {
    expect(isSubmitKey({ key: 'Enter', shiftKey: false, isComposing: false, keyCode: 13 })).toBe(true);
    expect(isSubmitKey({ key: 'Enter', shiftKey: true, isComposing: false, keyCode: 13 })).toBe(false);
    expect(isSubmitKey({ key: 'Enter', shiftKey: false, isComposing: true, keyCode: 229 })).toBe(false);
    expect(isSubmitKey({ key: 'Enter', shiftKey: false, isComposing: false, keyCode: 229 })).toBe(false);
    expect(isSubmitKey({ key: 'a', shiftKey: false, isComposing: false, keyCode: 65 })).toBe(false);
  });

  test('a failed send keeps the draft; a successful send clears only the submitted text', () => {
    expect(nextDraftAfterSend('你好', '你好', false)).toBeNull();
    expect(nextDraftAfterSend('你好', '你好', true)).toBe('');
    expect(nextDraftAfterSend('你好，补充', '你好', true)).toBeNull();
  });

  test('shows the stop action while a task is running', () => {
    const running = render({ canStop: true, canSend: false });
    expect(running).toContain('aria-label="停止"');
    expect(running).toContain('任务运行中，可先起草消息');
    expect(running).not.toContain('aria-label="发送"');
  });

  test('shows the send action when idle and keeps the draft controlled', () => {
    const idle = render();
    expect(idle).toContain('aria-label="发送"');
    expect(idle).toContain('Enter 发送 · Shift+Enter 换行');
    expect(idle).toContain('>草稿</textarea>');
  });

  test('over-limit drafts render an alert and disable send', () => {
    const over = render({ draft: 'x'.repeat(MAX_COMPOSER_CHARS + 1) });
    expect(over).toContain('role="alert"');
    expect(over).toContain('已超出 1 字');
    expect(over).toContain('disabled=""');
  });

  test('renders read-only status meta and context usage', () => {
    const meta = {
      model: {
        id: 'model', label: '模型', currentValue: 'fast', currentLabel: 'Fast',
        options: [{ value: 'fast', name: 'Fast' }, { value: 'quality', name: 'Quality' }],
      },
      modeLabel: 'default',
    };
    const markup = render({ meta, usage: { used: 50, max: 100, percent: 50 } });
    expect(markup).toContain('Fast');
    expect(markup).toContain('default');
    expect(markup).toContain('上下文用量 50%');
    expect(markup).not.toContain('aria-haspopup="listbox"');
  });
});
