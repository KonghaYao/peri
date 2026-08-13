import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { authInvalidation, clearAuthInvalidation, installPrincipalRole, publishAuthInvalidation } from '../lib/auth-state';

const transport = vi.hoisted(() => ({
  clearUiSession: vi.fn(),
  connectWithCookie: vi.fn(),
}));

vi.mock('../store', () => transport);

import { AuthGate } from './AuthGate';

afterEach(() => {
  vi.unstubAllGlobals();
  transport.clearUiSession.mockReset();
  transport.connectWithCookie.mockReset();
  installPrincipalRole(null);
  clearAuthInvalidation();
});

describe('AuthGate invalidation recovery', () => {
  it('does not re-apply a consumed invalidation after a successful login', async () => {
    const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') return { ok: true, json: async () => ({ role: 'full' }) };
      return { ok: false, status: 401 };
    });
    vi.stubGlobal('fetch', fetch);

    render(() => <AuthGate><div>authenticated workspace</div></AuthGate>);
    await screen.findByLabelText('访问令牌');

    publishAuthInvalidation('token revoked by administrator');
    expect(await screen.findByRole('alert')).toHaveTextContent('token revoked by administrator');

    fireEvent.input(screen.getByLabelText('访问令牌'), { target: { value: 'new-token' } });
    fireEvent.click(screen.getByRole('button', { name: '登录' }));

    expect(await screen.findByText('authenticated workspace')).toBeInTheDocument();
    await waitFor(() => expect(authInvalidation()).toBeNull());
    expect(screen.queryByLabelText('访问令牌')).not.toBeInTheDocument();
    expect(transport.connectWithCookie).toHaveBeenCalledOnce();
  });

  it('ignores a stale status success that resolves after websocket invalidation', async () => {
    let resolveStatus!: (value: { ok: boolean; status: number; json: () => Promise<{ role: string }> }) => void;
    vi.stubGlobal('fetch', vi.fn(() => new Promise((resolve) => { resolveStatus = resolve; })));

    render(() => <AuthGate><div>authenticated workspace</div></AuthGate>);
    publishAuthInvalidation('cookie revoked while checking');
    expect(await screen.findByRole('alert')).toHaveTextContent('cookie revoked while checking');

    resolveStatus({ ok: true, status: 200, json: async () => ({ role: 'full' }) });
    await Promise.resolve();
    await Promise.resolve();

    expect(screen.getByLabelText('访问令牌')).toBeInTheDocument();
    expect(screen.queryByText('authenticated workspace')).not.toBeInTheDocument();
    expect(transport.connectWithCookie).not.toHaveBeenCalled();
  });

  it('fails closed when a successful response carries an unknown role', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ role: 'instance' }) })));

    render(() => <AuthGate><div>authenticated workspace</div></AuthGate>);

    expect(await screen.findByRole('alert')).toHaveTextContent('无法识别的访问角色');
    expect(screen.getByLabelText('访问令牌')).toBeInTheDocument();
    expect(screen.queryByText('authenticated workspace')).not.toBeInTheDocument();
    expect(transport.connectWithCookie).not.toHaveBeenCalled();
  });
});
