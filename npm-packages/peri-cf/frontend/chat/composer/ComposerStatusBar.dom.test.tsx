import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { act, cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { createElement } = await import('react');
const { ComposerStatusBar } = await import('./ComposerStatusBar');
const { MAX_COMPOSER_CHARS, composerLimitState } = await import('./composerLimits');

afterEach(() => cleanup());

const model = {
  id: 'model', label: '模型', currentValue: 'fast', currentLabel: 'Fast',
  options: [
    { value: 'fast', name: 'Fast' },
    { value: 'quality', name: 'Quality', description: '更慢但更强' },
  ],
};
const mode = {
  id: 'mode', label: '模式', currentValue: 'default', currentLabel: 'Default',
  options: [{ value: 'default', name: 'Default' }, { value: 'auto', name: 'Auto' }],
};
const usage = { used: 50, max: 200, percent: 25 };

describe('ComposerStatusBar', () => {
  test('renders read-only meta and context usage without fake buttons', () => {
    render(createElement(ComposerStatusBar, {
      meta: { model, mode },
      usage,
      limit: composerLimitState('x'.repeat(MAX_COMPOSER_CHARS - 2)),
      hint: 'Enter 发送 · Shift+Enter 换行',
      actions: createElement('button', { type: 'button', 'aria-label': '发送' }, '发送'),
    }));

    expect(screen.getByText('Fast')).toBeTruthy();
    expect(screen.getByText('Default')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /模型：Fast/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /模式：Default/ })).toBeNull();
    expect(screen.getByRole('button', { name: /上下文用量 25%/ })).toBeTruthy();
    expect(screen.getByText(`${MAX_COMPOSER_CHARS - 2} / ${MAX_COMPOSER_CHARS}`)).toBeTruthy();
    expect(screen.getByRole('button', { name: '发送' })).toBeTruthy();
  });

  test('renders selectors and sends the selected config option', async () => {
    const calls: Array<[string, string]> = [];
    render(createElement(ComposerStatusBar, {
      meta: { model, mode },
      onSelectOption: async (configId: string, value: string) => { calls.push([configId, value]); },
    }));

    fireEvent.click(screen.getByRole('button', { name: /模型：Fast/ }));
    expect(screen.getByRole('listbox', { name: '模型选项' })).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByText('Quality'));
      await Promise.resolve();
    });
    expect(calls).toEqual([['model', 'quality']]);
    expect(screen.getByRole('button', { name: /模型：Quality/ })).toBeTruthy();
  });

  test('rolls back the optimistic label and shows an error when the write fails', async () => {
    render(createElement(ComposerStatusBar, {
      meta: { model },
      onSelectOption: async () => { throw new Error('后端拒绝了配置'); },
    }));

    fireEvent.click(screen.getByRole('button', { name: /模型：Fast/ }));
    await act(async () => {
      fireEvent.click(screen.getByText('Quality'));
      await Promise.resolve();
    });
    expect(screen.getByRole('alert').textContent).toContain('后端拒绝了配置');
    expect(screen.getByRole('button', { name: /模型：Fast/ })).toBeTruthy();
  });

  test('hides the context meter when no usage is available', () => {
    render(createElement(ComposerStatusBar, { meta: { modelLabel: 'fallback-model' } }));
    expect(screen.getByText('fallback-model')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /上下文用量/ })).toBeNull();
  });
});
