import { createEffect, createSignal, onMount, Show, type JSX } from 'solid-js';
import { authInvalidated, clearUiSession, connectWithCookie, installPrincipalRole } from '../store';
import { parsePrincipal } from '../lib/auth-role';
import { Button } from '../../ui/Button';
import { TextField } from '../../ui/Field';

type AuthState = 'checking' | 'signed-out' | 'signed-in';

export function AuthGate(props: { children: JSX.Element }) {
  const [state, setState] = createSignal<AuthState>('checking');
  const [token, setToken] = createSignal('');
  const [error, setError] = createSignal('');
  const [submitting, setSubmitting] = createSignal(false);

  async function status() {
    try {
      const res = await fetch('/api/auth/session', { credentials: 'same-origin', cache: 'no-store' });
      if (!res.ok) { installPrincipalRole(null); return setState('signed-out'); }
      installPrincipalRole(parsePrincipal(await res.json()));
      setState('signed-in');
      connectWithCookie();
    } catch {
      setError('无法连接 acp-hub server。');
      setState('signed-out');
    }
  }

  async function signIn(e: SubmitEvent) {
    e.preventDefault();
    setSubmitting(true);
    setError('');
    try {
      const res = await fetch('/api/auth/session', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ token: token().trim() }),
      });
      if (!res.ok) throw new Error('认证失败，请检查 token 是否有效。');
      installPrincipalRole(parsePrincipal(await res.json()));
      setToken('');
      setState('signed-in');
      connectWithCookie();
    } catch (e) {
      setError(e instanceof Error ? e.message : '认证失败。');
    } finally {
      setSubmitting(false);
    }
  }

  onMount(status);
  createEffect(() => { if (authInvalidated() > 0) setState('signed-out'); });

  async function logout() {
    clearUiSession();
    installPrincipalRole(null);
    setState('signed-out');
    await fetch('/api/auth/session', { method: 'DELETE', credentials: 'same-origin' });
  }

  return (
    <Show when={state() === 'signed-in'} fallback={
      <main class="auth-page">
        <section class="auth-card" aria-labelledby="auth-title">
          <div class="auth-brand">acp-hub</div>
          <h1 id="auth-title">继续你的工作</h1>
          <p>使用 server 签发的 full token 建立安全浏览器会话。凭证只提交一次，不会保存在浏览器存储中。</p>
          <Show when={state() === 'checking'} fallback={
            <form onSubmit={signIn} class="auth-form">
              <TextField label="访问令牌" type="password" value={token()} onInput={(e) => setToken(e.currentTarget.value)} autocomplete="off" autofocus />
              <Show when={error()}><p class="ui-error" role="alert">{error()}</p></Show>
              <Button variant="primary" type="submit" busy={submitting()} disabled={!token().trim()}>登录</Button>
            </form>
          }>
            <span class="ui-spinner" aria-label="正在检查登录状态" />
          </Show>
        </section>
      </main>
    }><div class="authenticated-app">{props.children}<button class="logout-button" onClick={logout}>退出登录</button></div></Show>
  );
}
