import { createContext, createEffect, createSignal, onMount, Show, useContext, type JSX } from 'solid-js';
import { authInvalidated, clearUiSession, connectWithCookie, installPrincipalRole } from '../store';
import { parsePrincipal } from '../lib/auth-role';
import { authFeedback } from '../lib/auth-feedback.mjs';
import { Button, TextField } from '../../ui';

type AuthState = 'checking' | 'signed-out' | 'signed-in';
type AuthProblem = ReturnType<typeof authFeedback>;
const AuthActionsContext = createContext<{ logout: () => void }>();
export const useAuthActions = () => useContext(AuthActionsContext);

export function AuthGate(props: { children: JSX.Element }) {
  const [state, setState] = createSignal<AuthState>('checking');
  const [token, setToken] = createSignal('');
  const [problem, setProblem] = createSignal<AuthProblem>(null);
  const [submitting, setSubmitting] = createSignal(false);

  async function status() {
    setProblem(null);
    try {
      const res = await fetch('/api/auth/session', { credentials: 'same-origin', cache: 'no-store' });
      if (!res.ok) {
        installPrincipalRole(null);
        setProblem(authFeedback(res.status, 'status'));
        return setState('signed-out');
      }
      installPrincipalRole(parsePrincipal(await res.json()));
      setState('signed-in');
      connectWithCookie();
    } catch {
      setProblem(authFeedback(0, 'status'));
      setState('signed-out');
    }
  }

  async function signIn(e: SubmitEvent) {
    e.preventDefault();
    setSubmitting(true);
    setProblem(null);
    try {
      const res = await fetch('/api/auth/session', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ token: token().trim() }),
      });
      if (!res.ok) {
        setProblem(authFeedback(res.status, 'login'));
        return;
      }
      installPrincipalRole(parsePrincipal(await res.json()));
      setToken('');
      setState('signed-in');
      connectWithCookie();
    } catch {
      setProblem(authFeedback(0, 'login'));
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
              <Show when={problem()}>{(item) => <div class="auth-problem" role="alert"><p>{item().message}</p><Show when={item().retryable}><Button type="button" variant="ghost" onClick={() => void status()}>重新检查连接</Button></Show></div>}</Show>
              <Button variant="primary" type="submit" busy={submitting()} disabled={!token().trim()}>登录</Button>
              <details class="auth-help">
                <summary>令牌在哪里？</summary>
                <div>
                  <p>默认记录在 <code>~/.config/acp-hub/tokens.toml</code>。已有令牌请复制 <code>role = "full"</code> 同一段中的 token 值。</p>
                  <p>没有 full token 时，在 acp-hub 目录执行：</p>
                  <code class="auth-command">cargo run -p acp-hub-server -- token generate --name web --role full</code>
                  <p>命令只会显示完整令牌一次。不要把它提交到代码、日志或聊天记录。</p>
                </div>
              </details>
            </form>
          }>
            <span class="ui-spinner" aria-label="正在检查登录状态" />
          </Show>
        </section>
      </main>
    }><AuthActionsContext.Provider value={{ logout: () => { void logout(); } }}><div class="authenticated-app">{props.children}</div></AuthActionsContext.Provider></Show>
  );
}
