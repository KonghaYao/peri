import { Show } from 'solid-js';
import { connectionProblem, reconnect } from '../store';
import { Button } from '../../ui';

export function ConnectionProblem() {
  return <Show when={connectionProblem()}>{(problem) =>
    <section class="connection-problem" role="alert">
      <div class="connection-problem__mark" aria-hidden="true">!</div>
      <div class="connection-problem__body"><strong>{problem().title}</strong><p>{problem().detail}</p></div>
      <Show when={problem().action === 'reconnect'}><Button variant="primary" onClick={reconnect}>重新连接</Button></Show>
    </section>
  }</Show>;
}
