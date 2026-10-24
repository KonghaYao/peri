import { BusinessError } from '../contracts/errors';
import { LIMITS } from '../contracts/limits';
import { isActiveMember } from '../storage/queries';

const INBOX_PREFIX = 'agentnet://actors/';
const ROOM_PREFIX = 'agentnet://rooms/';

function addTopic(topics: Map<string, Set<string>>, topic: string, uri: string): void {
  const set = topics.get(topic) ?? new Set<string>();
  set.add(uri);
  topics.set(topic, set);
}

/**
 * 校验一次 `subscriptions/listen` 的 URI 集合，并映射为内部 topic。
 *
 * 权限在建立流之前检查；未知、越权或超过 4 条都整体拒绝，不做静默裁剪。
 */
export async function resolveSubscriptionTopics(
  db: D1Database,
  actor: string,
  uris: string[],
): Promise<Map<string, Set<string>>> {
  const unique = [...new Set(uris)];
  if (unique.length > LIMITS.subscriptionUrisMax) {
    throw new BusinessError('INVALID_ARGUMENT', '每条订阅最多 4 个 resource URI', {
      limit: LIMITS.subscriptionUrisMax,
    });
  }

  const topics = new Map<string, Set<string>>();
  for (const uri of unique) {
    if (uri === 'agentnet://me') {
      addTopic(topics, `inbox:${actor}`, uri);
      continue;
    }
    if (uri.startsWith(INBOX_PREFIX) && uri.endsWith('/inbox')) {
      const target = uri.slice(INBOX_PREFIX.length, -'/inbox'.length);
      if (target !== actor) {
        throw new BusinessError('NOT_MEMBER', '不能订阅其他 actor 的 inbox', { uri });
      }
      addTopic(topics, `inbox:${target}`, uri);
      continue;
    }
    if (uri.startsWith(ROOM_PREFIX) && uri.endsWith('/head')) {
      const roomId = uri.slice(ROOM_PREFIX.length, -'/head'.length);
      if (!roomId || !(await isActiveMember(db, roomId, actor))) {
        throw new BusinessError('NOT_MEMBER', '只能订阅自己有效参与的房间', { uri });
      }
      addTopic(topics, `room:${roomId}`, uri);
      continue;
    }
    throw new BusinessError('INVALID_ARGUMENT', '不支持的资源 URI', { uri });
  }
  return topics;
}
