import { getServerByName } from 'partyserver';

import { BusinessError } from '../contracts/errors';
import type { Principal } from '../domain/principal';
import type { Env } from '../env';
import { inboxTopic } from '../storage/events';

export interface LoginView {
  actor_id: string;
  online: true;
  logged_in_at: string;
  pending_count: number;
  inbox_head_seq: string;
}

/**
 * Agent 显式上线。
 *
 * 上线状态保存在 inbox topic 的 Durable Object 中：订阅连接断开时由 DO
 * 自动置为离线，因此这里只负责表达“开始接收投递”的意图。上线前排队在
 * D1 的 delivery 不会被删除；DO 会用当前 inbox_head_seq 补发一条提示，
 * 由 Agent 随后调用 `network_inbox_list` 读取。
 */
export async function login(env: Env, principal: Principal): Promise<LoginView> {
  const actor = principal.actor;
  if (!actor) {
    throw new BusinessError('CREDENTIAL_INVALID', '当前凭据尚未注册；先调用 network_register');
  }
  if (actor.kind !== 'agent') {
    throw new BusinessError('INVALID_ARGUMENT', '只有 Agent 需要上线；人类身份通过网页或订阅直接读取', {
      kind: actor.kind,
    });
  }

  const channel = await getServerByName(env.NOTIFICATION_CHANNELS, inboxTopic(actor.actor_id));
  const presence = await channel.login(actor.inbox_head_seq);

  return {
    actor_id: actor.actor_id,
    online: true,
    logged_in_at: String(presence.since),
    pending_count: actor.pending_count,
    inbox_head_seq: String(actor.inbox_head_seq),
  };
}
