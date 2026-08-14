import { For, Show } from 'solid-js';
import { dismissPersistentError, persistentErrors, retryPersistentAction } from '../store';
import { Button, CopyButton } from '../../ui';

export function ErrorCenter() {
  return <Show when={persistentErrors().length}>
    <section class="error-center" aria-label="操作错误">
      <For each={persistentErrors()}>{(error) => <article class="error-card" role="alert">
        <div><strong>{error.title}</strong><p>{error.detail}</p><Show when={error.commandId}><code>{error.commandId}</code></Show></div>
        <div class="error-card__actions"><CopyButton text={[error.title, error.detail, error.commandId].filter(Boolean).join('\n')} label="复制详情" /><Show when={(error.retryable || error.retrying) && error.commandId}><Button variant="primary" busy={error.retrying} disabled={error.retrying} onClick={() => retryPersistentAction(error.commandId!)}>使用原请求重新确认</Button></Show><Show when={!error.retryable && !error.retrying}><Button onClick={() => dismissPersistentError(error.id)}>关闭</Button></Show></div>
      </article>}</For>
    </section>
  </Show>;
}
