import { afterEach, describe, expect, test } from 'bun:test';
import { createElement } from 'react';
import { installDom } from '../../../test/dom';
import type { TimelineWorkItem } from '../types';

installDom();

const { cleanup, fireEvent, render } = await import('@testing-library/react');
const { ReasoningRow } = await import('./ReasoningRow');
const { WorkGroupRow } = await import('./WorkGroupRow');

function reasoning(status: TimelineWorkItem['status']): TimelineWorkItem {
  return {
    kind: 'reasoning', id: 'r1', entryId: 'turn:1:a', blockId: 'r1',
    status, label: '思考', text: '先检查一下',
    createdAt: null, completedAt: null, failed: status === 'failed',
  };
}

function toggleButton(): HTMLButtonElement {
  return document.querySelector('[data-slot="peri-reasoning-row"] button') as HTMLButtonElement;
}

afterEach(() => cleanup());

describe('ReasoningRow', () => {
  test('运行中展开，结束后自动收起', () => {
    const { rerender } = render(createElement(ReasoningRow, { item: reasoning('streaming') }));
    expect(toggleButton().getAttribute('aria-expanded')).toBe('true');

    rerender(createElement(ReasoningRow, { item: reasoning('completed') }));
    expect(toggleButton().getAttribute('aria-expanded')).toBe('false');
  });

  test('非运行中的思考保持收起', () => {
    render(createElement(ReasoningRow, { item: reasoning('failed') }));
    expect(toggleButton().getAttribute('aria-expanded')).toBe('false');
  });

  test('收起后展示预览，点击可展开或收起', () => {
    render(createElement(ReasoningRow, { item: reasoning('completed') }));
    expect(document.body.textContent).toContain('先检查一下');

    fireEvent.click(toggleButton());
    expect(toggleButton().getAttribute('aria-expanded')).toBe('true');

    fireEvent.click(toggleButton());
    expect(toggleButton().getAttribute('aria-expanded')).toBe('false');
  });

  test('rail=false 时不画左侧竖线', () => {
    render(createElement(ReasoningRow, { item: reasoning('streaming'), rail: false }));
    const text = document.querySelector('[data-slot="peri-reasoning-row"] p');
    expect(text?.className ?? '').not.toContain('border-l-2');
  });

  test('bare work group 不画容器与思考的左侧竖线', () => {
    render(createElement(WorkGroupRow, {
      items: [reasoning('streaming')], active: false, failed: false,
      defaultExpanded: true, bare: true,
    }));

    const group = document.querySelector('[data-slot="peri-work-group"]')!;
    expect(group.getAttribute('data-bare')).toBe('true');
    expect(group.className).not.toContain('border-l');
    expect(group.className).not.toContain('border-b');

    const text = document.querySelector('[data-slot="peri-reasoning-row"] p');
    expect(text?.className ?? '').not.toContain('border-l-2');
  });
});
