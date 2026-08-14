import { createContext, createEffect, createSignal, onMount, Show, useContext, type JSX } from 'solid-js';
import { connectWithCookie, resetAuthenticatedSession } from '../store';
import { parsePrincipal } from '../lib/auth-role';
import { authFeedback } from '../lib/auth-feedback.mjs';
import { authInvalidation, clearAuthInvalidation, installPrincipalRole } from '../lib/auth-state';
import { parseAuthSetup, type AuthSetup } from '../lib/auth-setup';
import { Button, CopyButton, TextField } from '../../ui';

type AuthState = 'checking' | 'signed-out' | 'signed-in';
type AuthProblem = ReturnType<typeof authFeedback>;
const AuthActionsContext = createContext<{ logout: () => void }>();
export const useAuthActions = () => useContext(AuthActionsContext);

export function AuthGate(props: { children: JSX.Element }) {
  const [state, setState] = createSignal<AuthState>('checking');
  const [token, setToken] = createSignal('');
  const [problem, setProblem] = createSignal<AuthProblem>(null);
  const [submitting, setSubmitting] = createSignal(false);
  const [setup, setSetup] = createSignal<AuthSetup | null>(null);
  let requestEpoch = 0;

  async function authPayload(res: Response): Promise<{ payload: unknown; setup: AuthSetup | null }> {
    try {
      const payload: unknown = await res.json();
      return { payload, setup: parseAuthSetup(payload) };
    } catch {
      return { payload: null, setup: null };
    }
  }

  async function status() {
    const epoch = ++requestEpoch;
    setProblem(null);
    try {
      const res = await fetch('/api/auth/session', { credentials: 'same-origin', cache: 'no-store' });
      if (epoch !== requestEpoch) return;
      const parsed = await authPayload(res);
      if (epoch !== requestEpoch) return;
      if (parsed.setup) setSetup(parsed.setup);
      if (!res.ok) {
        resetAuthenticatedSession();
        setProblem(authFeedback(res.status, 'status'));
        return setState('signed-out');
      }
      const role = parsePrincipal(parsed.payload);
      if (!role) {
        resetAuthenticatedSession();
        setProblem({ kind: 'server', message: 'server 返回了无法识别的访问角色，已阻止进入应用。', retryable: true });
        return setState('signed-out');
      }
      resetAuthenticatedSession();
      installPrincipalRole(role);
      clearAuthInvalidation();
      setState('signed-in');
      connectWithCookie();
    } catch {
      if (epoch === requestEpoch) {
        resetAuthenticatedSession();
        setProblem(authFeedback(0, 'status'));
        setState('signed-out');
      }
    }
  }

  async function signIn(e: SubmitEvent) {
    e.preventDefault();
    const epoch = ++requestEpoch;
    setSubmitting(true);
    setProblem(null);
    try {
      const res = await fetch('/api/auth/session', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ token: token().trim() }),
      });
      if (epoch !== requestEpoch) return;
      const parsed = await authPayload(res);
      if (epoch !== requestEpoch) return;
      if (parsed.setup) setSetup(parsed.setup);
      if (!res.ok) {
        resetAuthenticatedSession();
        setProblem(authFeedback(res.status, 'login'));
        return;
      }
      const role = parsePrincipal(parsed.payload);
      if (!role) {
        resetAuthenticatedSession();
        setProblem({ kind: 'server', message: 'server 返回了无法识别的访问角色，已阻止登录。', retryable: true });
        return;
      }
      resetAuthenticatedSession();
      installPrincipalRole(role);
      clearAuthInvalidation();
      setToken('');
      setState('signed-in');
      connectWithCookie();
    } catch {
      if (epoch === requestEpoch) {
        resetAuthenticatedSession();
        setProblem(authFeedback(0, 'login'));
      }
    } finally {
      if (epoch === requestEpoch) setSubmitting(false);
    }
  }

  onMount(status);
  createEffect(() => {
    const event = authInvalidation();
    if (!event) return;
    requestEpoch += 1;
    resetAuthenticatedSession();
    setSubmitting(false);
    setProblem({ kind: 'credential', message: event.reason, retryable: false });
    setState('signed-out');
  });

  async function logout() {
    requestEpoch += 1;
    resetAuthenticatedSession();
    clearAuthInvalidation();
    installPrincipalRole(null);
    setState('signed-out');
    try {
      await fetch('/api/auth/session', { method: 'DELETE', credentials: 'same-origin' });
    } catch {
      setProblem({ kind: 'network', message: '本地界面已退出，但 server 未确认注销。恢复连接后请重新检查登录状态。', retryable: true });
    }
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
                  <Show when={setup()} fallback={<p>server 未提供配置路径。已有令牌请从启动 server 所使用的 <code>tokens.toml</code> 中复制 <code>role = "full"</code> 同一段的 token 值。</p>}>
                    {(hint) => <><p>此 server 正在从以下文件读取令牌：</p><code class="auth-command">{hint().tokenFile}</code></>}
                  </Show>
                  <p>没有 full token 时，在运行 server 的机器上执行：</p>
                  <code class="auth-command">{setup()?.generateCommand ?? 'acp-hub-server token generate --name web --role full'}</code>
                  <CopyButton label="复制生成命令" copiedLabel="生成命令已复制" text={setup()?.generateCommand ?? 'acp-hub-server token generate --name web --role full'} size="compact" />
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
