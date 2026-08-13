import { fireEvent, render, screen } from '@solidjs/testing-library';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { setBusy, setConnectionProblem } from '../store';

vi.mock('../store', async (original) => {
  const actual = await original<typeof import('../store')>();
  return { ...actual, reconnect: vi.fn() };
});

import { reconnect } from '../store';
import { ConnectionProblem } from './ConnectionProblem';

afterEach(() => {
  setBusy(false);
  setConnectionProblem(null);
  vi.mocked(reconnect).mockClear();
});

describe('ConnectionProblem', () => {
  it('prevents duplicate reconnect attempts and exposes progress', () => {
    setConnectionProblem({ code: 4501, title: '连接超时', detail: '会话仍然安全。', action: 'reconnect' });
    const view = render(() => <ConnectionProblem />);

    fireEvent.click(screen.getByRole('button', { name: '重新连接' }));
    expect(reconnect).toHaveBeenCalledOnce();

    setBusy(true);
    const button = screen.getByRole('button', { name: /正在连接/ });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute('aria-busy', 'true');
    fireEvent.click(button);
    expect(reconnect).toHaveBeenCalledOnce();
    view.unmount();
  });

  it('does not invent a reconnect action for login failures', () => {
    setConnectionProblem({ code: 4502, title: '登录失效', detail: '需要重新登录。', action: 'login' });
    render(() => <ConnectionProblem />);

    expect(screen.getByRole('alert')).toHaveTextContent('需要重新登录。');
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
});
