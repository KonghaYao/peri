/**
 * 端到端验收：身份、好友、房间、消息、交付闭环、订阅与重连。
 *
 * 只使用官方 `@modelcontextprotocol/client` 的 2026-07-28 模式；
 * 通过 AGENTNET_URL 可对本地 wrangler dev 或部署后的 Cloudflare 服务运行。
 */

import type { McpSubscription } from '@modelcontextprotocol/client';

import {
  baseUrl,
  callTool,
  check,
  checkEqual,
  checksPassed,
  connectClient,
  expectToolError,
  newRequestId,
  newToken,
  notificationCollector,
  openSubscription,
  rawRpc,
  readResourceJson,
  sleep,
  subscriptionIdOf,
  waitUntil,
  type McpConnection,
} from './lib/harness';

interface Identity {
  actor_id: string;
  kind: string;
  name: string;
  pending_count: number;
  resources: string[];
}

interface Delivery {
  delivery_id: string;
  recipient_id: string;
  message_id: string;
  room_id: string;
  state: 'pending' | 'claimed' | 'completed' | 'failed' | 'cancelled';
  attempt: number;
  claim_id: string | null;
  result_message_id: string | null;
}

interface InboxPage {
  items: Delivery[];
  next_cursor: string | null;
  has_more: boolean;
  pending_count: number;
  claimed_delivery_id: string | null;
}

interface MessageView {
  message_id: string;
  sender_id: string;
  created_seq: string;
  preview: string;
  body_bytes: number;
  reply_to: string | null;
  root_id: string | null;
  hop: number;
  delivery_states: Record<string, number>;
}

interface HistoryPage {
  items: MessageView[];
  next_cursor: string | null;
  has_more: boolean;
  snapshot: string;
  head_seq: string;
}

interface SendResult {
  message: MessageView;
  delivery_ids: string[];
}

interface RoomDetail {
  room: {
    room_id: string;
    kind: string;
    owner_id: string | null;
    head_seq: string;
    member_count: number;
    my_role: string;
    last_message: { message_id: string; preview: string } | null;
  };
  members: { actor_id: string; role: string; kind: string }[];
}

interface ReadChunk {
  chunk: string;
  next_cursor: string | null;
  eof: boolean;
  body_bytes: number;
}

interface LoginResult {
  actor_id: string;
  online: true;
  logged_in_at: string;
  pending_count: number;
  inbox_head_seq: string;
}

async function registerIdentity(
  connection: McpConnection,
  input: { kind: 'human' | 'agent'; name: string; sessionId?: string },
): Promise<Identity> {
  return callTool<Identity>(connection.client, 'network_register', {
    request_id: newRequestId('reg'),
    kind: input.kind,
    name: input.name,
    ...(input.sessionId ? { agent_session_id: input.sessionId } : {}),
  });
}

async function sendMessage(
  connection: McpConnection,
  roomId: string,
  body: string,
  options: { invoke?: string[]; clientMessageId?: string; requestId?: string } = {},
): Promise<SendResult> {
  return callTool<SendResult>(connection.client, 'network_message_send', {
    request_id: options.requestId ?? newRequestId('send'),
    client_message_id: options.clientMessageId ?? `cm_${crypto.randomUUID()}`,
    room_id: roomId,
    body,
    ...(options.invoke ? { invoke: options.invoke } : {}),
  });
}

async function inbox(
  connection: McpConnection,
  state: 'pending' | 'claimed' | 'completed' | 'cancelled' | 'all' = 'all',
): Promise<InboxPage> {
  return callTool<InboxPage>(connection.client, 'network_inbox_list', { state });
}

async function readWholeMessage(
  connection: McpConnection,
  messageId: string,
): Promise<string> {
  let cursor: string | null = null;
  let text = '';
  for (let round = 0; round < 20; round += 1) {
    const chunk: ReadChunk = await callTool<ReadChunk>(connection.client, 'network_message_read', {
      message_id: messageId,
      ...(cursor ? { chunk_cursor: cursor } : {}),
    });
    text += chunk.chunk;
    if (!chunk.next_cursor) {
      check(`分块读取在第 ${round + 1} 块结束`, chunk.eof, chunk);
      return text;
    }
    cursor = chunk.next_cursor;
  }
  throw new Error('消息分块读取超过 20 轮');
}

async function waitClosed(subscription: McpSubscription, timeoutMs = 15_000): Promise<string> {
  return Promise.race([
    subscription.closed,
    sleep(timeoutMs).then(() => 'timeout' as const),
  ]);
}

async function main(): Promise<void> {
  console.log(`端到端验收: ${baseUrl()}`);

  const agentA = await connectClient(newToken());
  const agentB = await connectClient(newToken());
  const humanC = await connectClient(newToken());

  try {
    const a = await registerIdentity(agentA, {
      kind: 'agent',
      name: '验收 Agent A',
      sessionId: `sess_${crypto.randomUUID()}`,
    });
    const b = await registerIdentity(agentB, {
      kind: 'agent',
      name: '验收 Agent B',
      sessionId: `sess_${crypto.randomUUID()}`,
    });
    const c = await registerIdentity(humanC, { kind: 'human', name: '验收人类 C' });
    check('三个身份注册成功', Boolean(a.actor_id && b.actor_id && c.actor_id), { a: a.actor_id, b: b.actor_id, c: c.actor_id });
    check(
      '资源入口包含 me 与 inbox',
      a.resources.includes('agentnet://me') &&
        a.resources.some((uri) => uri.endsWith('/inbox')),
      a.resources,
    );

    const me = await callTool<Identity>(agentA.client, 'network_me');
    checkEqual('network_me 返回当前身份', me.actor_id, a.actor_id);
    const profile = await callTool<Identity>(agentA.client, 'network_profile_update', {
      request_id: newRequestId('profile'),
      name: '验收 Agent A+',
    });
    checkEqual('profile_update 更新名称', profile.name, '验收 Agent A+');

    const renameRequest = newRequestId('rename');
    const renamed = await callTool<{ actor_id: string; name: string }>(
      humanC.client,
      'network_agent_rename',
      {
        request_id: renameRequest,
        actor_id: a.actor_id,
        name: '  验收 Agent A 全局名  ',
      },
    );
    checkEqual('任何已注册身份可全局重命名 Agent', renamed.name, '验收 Agent A 全局名');
    const renamedReplay = await callTool<{ actor_id: string; name: string }>(
      humanC.client,
      'network_agent_rename',
      {
        request_id: renameRequest,
        actor_id: a.actor_id,
        name: '  验收 Agent A 全局名  ',
      },
    );
    checkEqual('rename 幂等重放返回同一名称', renamedReplay.name, renamed.name);
    const renamedMe = await callTool<Identity>(agentA.client, 'network_me');
    checkEqual('rename 对所有人全局生效', renamedMe.name, '验收 Agent A 全局名');
    await expectToolError(
      'rename 未知 actor 返回 NOT_FOUND',
      callTool(agentA.client, 'network_agent_rename', {
        request_id: newRequestId('rename'),
        actor_id: 'agt_missing_actor',
        name: '不存在',
      }),
      'NOT_FOUND',
    );
    await expectToolError(
      'rename 目标为人类时返回 INVALID_ARGUMENT',
      callTool(agentA.client, 'network_agent_rename', {
        request_id: newRequestId('rename'),
        actor_id: c.actor_id,
        name: '人类不能这样改',
      }),
      'INVALID_ARGUMENT',
    );
    await expectToolError(
      '同一 request_id 改不同名称返回 IDEMPOTENCY_CONFLICT',
      callTool(humanC.client, 'network_agent_rename', {
        request_id: renameRequest,
        actor_id: a.actor_id,
        name: '另一个名称',
      }),
      'IDEMPOTENCY_CONFLICT',
    );

    await callTool(agentA.client, 'network_contact_add', {
      request_id: newRequestId('contact'),
      actor_id: b.actor_id,
    });
    await callTool(agentB.client, 'network_contact_add', {
      request_id: newRequestId('contact'),
      actor_id: a.actor_id,
    });
    const contacts = await callTool<{ items: { actor_id: string }[] }>(
      agentA.client,
      'network_contacts_list',
      {},
    );
    check('A 的好友列表包含 B', contacts.items.some((item) => item.actor_id === b.actor_id), contacts.items);

    await expectToolError(
      '非好友不能加入群聊',
      callTool(humanC.client, 'network_room_create', {
        request_id: newRequestId('room'),
        kind: 'group',
        members: [a.actor_id],
        title: '越权群',
      }),
      'FRIEND_REQUIRED',
    );

    const directA = await callTool<RoomDetail>(agentA.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'direct',
      members: [b.actor_id],
    });
    const directB = await callTool<RoomDetail>(agentB.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'direct',
      members: [a.actor_id],
    });
    const roomId = directA.room.room_id;
    checkEqual('双方创建单聊收敛到同一 room_id', directB.room.room_id, roomId);
    checkEqual('单聊没有 owner', directA.room.owner_id, null);
    checkEqual('单聊参与者为两人', directA.members.length, 2);

    await expectToolError(
      '非成员读取房间返回 NOT_MEMBER',
      callTool(humanC.client, 'network_room_get', { room_id: roomId }),
      'NOT_MEMBER',
    );
    await expectToolError(
      '非成员发送消息返回 NOT_MEMBER',
      callTool(humanC.client, 'network_message_send', {
        request_id: newRequestId('send'),
        client_message_id: `cm_${crypto.randomUUID()}`,
        room_id: roomId,
        body: 'should fail',
      }),
      'NOT_MEMBER',
    );

    const hello = await sendMessage(agentA, roomId, 'hello B');
    checkEqual('普通消息不产生 delivery', hello.delivery_ids.length, 0);
    const firstHistory = await callTool<HistoryPage>(agentB.client, 'network_history_list', {
      room_id: roomId,
    });
    check(
      'B 历史包含 A 的消息',
      firstHistory.items.some((item) => item.message_id === hello.message.message_id),
      firstHistory.items,
    );

    const longBody = 'x'.repeat(70_000);
    const long = await sendMessage(agentA, roomId, longBody);
    checkEqual('长消息 body_bytes 记录正确', long.message.body_bytes, 70_000);
    check('长消息预览被截断', long.message.preview.length < 600, long.message.preview.length);
    const whole = await readWholeMessage(agentB, long.message.message_id);
    checkEqual('分块读取可还原完整正文', whole.length, longBody.length);

    const p1 = await sendMessage(agentA, roomId, 'page-1');
    const p2 = await sendMessage(agentA, roomId, 'page-2');
    const p3 = await sendMessage(agentA, roomId, 'page-3');
    const pageOne = await callTool<HistoryPage>(agentB.client, 'network_history_list', {
      room_id: roomId,
      limit: 2,
    });
    checkEqual('历史分页 limit 生效', pageOne.items.length, 2);
    check('历史分页 has_more', pageOne.has_more, pageOne);
    check('历史分页返回 next_cursor', Boolean(pageOne.next_cursor), pageOne.next_cursor);
    const pageTwo = await callTool<HistoryPage>(agentB.client, 'network_history_list', {
      room_id: roomId,
      limit: 100,
      cursor: pageOne.next_cursor as string,
    });
    const seen = new Set([...pageOne.items, ...pageTwo.items].map((item) => item.message_id));
    check(
      '两次分页覆盖 p1..p3 且没有重复',
      [p1, p2, p3].every((item) => seen.has(item.message.message_id)) && seen.size === pageOne.items.length + pageTwo.items.length,
      { seen: seen.size },
    );
    const tailStart = await callTool<HistoryPage>(agentB.client, 'network_history_list', {
      room_id: roomId,
      direction: 'after',
    });
    checkEqual('after 模式初始页为空', tailStart.items.length, 0);
    check('after 模式返回 resume cursor', Boolean(tailStart.next_cursor), tailStart.next_cursor);
    const tailMessage = await sendMessage(agentA, roomId, 'tail-1');
    const tailPage = await callTool<HistoryPage>(agentB.client, 'network_history_list', {
      room_id: roomId,
      direction: 'after',
      cursor: tailStart.next_cursor as string,
    });
    check(
      'after 模式只返回新增消息',
      tailPage.items.some((item) => item.message_id === tailMessage.message.message_id),
      {
        tailStart: { cursor: tailStart.next_cursor, head: tailStart.head_seq, snapshot: tailStart.snapshot },
        newMessage: { id: tailMessage.message.message_id, seq: tailMessage.message.created_seq },
        tailPage,
      },
    );

    const mark = await callTool<{ room_id: string; last_read_seq: string }>(
      agentB.client,
      'network_room_mark_read',
      { room_id: roomId, through_seq: tailMessage.message.created_seq },
    );
    checkEqual('mark_read 返回房间 id', mark.room_id, roomId);
    const rooms = await callTool<{ items: { room_id: string; last_message: { message_id: string } | null }[] }>(
      agentB.client,
      'network_rooms_list',
      {},
    );
    const listedRoom = rooms.items.find((item) => item.room_id === roomId);
    check('B 的房间列表包含单聊', Boolean(listedRoom), rooms.items);
    checkEqual('房间摘要指向最新消息', listedRoom?.last_message?.message_id, tailMessage.message.message_id);

    const collector = notificationCollector(agentB.client);
    const subscription = await openSubscription(agentB.client, [
      'agentnet://me',
      `agentnet://rooms/${roomId}/head`,
    ]);
    check(
      '订阅 ack 覆盖请求的 URI',
      (subscription.honoredFilter.resourceSubscriptions ?? []).length === 2,
      subscription.honoredFilter,
    );
    const login = await callTool<LoginResult>(agentB.client, 'network_login');
    checkEqual('Agent 显式上线成功', login.online, true);
    checkEqual('上线返回自己的 actor_id', login.actor_id, b.actor_id);
    await collector.waitFor(
      (event) => event.params.uri === 'agentnet://me',
      '等待上线提示',
    );
    collector.reset();
    const notified = await sendMessage(agentA, roomId, 'notify-1');
    await collector.waitFor(
      (event) => event.params.uri === `agentnet://rooms/${roomId}/head`,
      '等待房间 head 资源更新',
    );
    check('通知与真实写入一致', notified.message.message_id.length > 0);

    // 普通消息只变更房间；inbox（待办入口）仅在产生新的待办时提示。
    const invoke = await sendMessage(agentA, roomId, 'task-1', { invoke: [b.actor_id] });
    const meEvent = await collector.waitFor(
      (event) => event.params.uri === 'agentnet://me',
      '等待 inbox 资源更新',
    );
    check('inbox 通知携带 subscriptionId', Boolean(subscriptionIdOf(meEvent.params)), meEvent.params);
    checkEqual('invoke 创建一条 delivery', invoke.delivery_ids.length, 1);

    const senderCollector = notificationCollector(agentA.client);
    const senderRoomUri = `agentnet://rooms/${roomId}/head`;
    const senderRoomSubscription = await openSubscription(agentA.client, [senderRoomUri]);
    await waitUntil('等待发送方房间订阅建立', () =>
      Boolean(senderRoomSubscription.honoredFilter.resourceSubscriptions?.length),
    );

    await callTool(agentA.client, 'network_contact_add', {
      request_id: newRequestId('contact'),
      actor_id: c.actor_id,
    });
    const humanRoom = await callTool<RoomDetail>(agentA.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'group',
      members: [c.actor_id],
      title: '人类目标校验',
    });
    await expectToolError(
      'invoke 指向人类成员返回 INVALID_ARGUMENT',
      sendMessage(agentA, humanRoom.room.room_id, 'human-target', { invoke: [c.actor_id] }),
      'INVALID_ARGUMENT',
    );
    const deliveryId = invoke.delivery_ids[0] as string;

    const pendingInbox = await inbox(agentB, 'pending');
    check('pending 列表包含新 delivery', pendingInbox.items.some((item) => item.delivery_id === deliveryId), pendingInbox.items);
    check('pending_count 与列表一致', pendingInbox.pending_count >= 1, pendingInbox.pending_count);

    const claimRequest = newRequestId('claim');
    const claimed = await callTool<Delivery>(agentB.client, 'network_delivery_claim', {
      request_id: claimRequest,
      delivery_id: deliveryId,
    });
    checkEqual('claim 后状态为 claimed', claimed.state, 'claimed');
    checkEqual('claim 后 attempt 为 1', claimed.attempt, 1);
    check('claim 返回 claim_id', Boolean(claimed.claim_id), claimed.claim_id);
    const replayClaim = await callTool<Delivery>(agentB.client, 'network_delivery_claim', {
      request_id: claimRequest,
      delivery_id: deliveryId,
    });
    checkEqual('相同 request_id 重放 claim 返回同一 claim_id', replayClaim.claim_id, claimed.claim_id);

    const claimedHistory = await callTool<HistoryPage>(agentA.client, 'network_history_list', {
      room_id: roomId,
      limit: 20,
    });
    const claimedRow = claimedHistory.items.find((item) => item.message_id === invoke.message.message_id);
    checkEqual('claim 后发送方 history 显示 claimed 回执', claimedRow?.delivery_states.claimed, 1);
    await senderCollector.waitFor(
      (event) => event.params.uri === senderRoomUri,
      '等待 claim 回执房间通知',
    );
    check('claim 回执通过房间订阅推送', senderCollector.events.length > 0);

    const report = await callTool<{ delivery_id: string; attempt: number; progress_at: string }>(
      agentB.client,
      'network_delivery_report',
      {
        request_id: newRequestId('report'),
        delivery_id: deliveryId,
        attempt: claimed.attempt,
        claim_id: claimed.claim_id,
        status: '正在处理',
      },
    );
    checkEqual('report 记录当前 attempt', report.attempt, 1);
    await sleep(500);
    const notificationsBeforeComplete = senderCollector.events.length;

    const inboxResource = await readResourceJson<{ pending: unknown[]; claimed: { delivery_id: string } | null }>(
      agentB.client,
      `agentnet://actors/${b.actor_id}/inbox`,
    );
    checkEqual('inbox 资源显示当前 claimed', inboxResource.claimed?.delivery_id, deliveryId);

    const completed = await callTool<{ delivery: Delivery; message: MessageView | null }>(
      agentB.client,
      'network_delivery_complete',
      {
        request_id: newRequestId('complete'),
        delivery_id: deliveryId,
        attempt: claimed.attempt,
        claim_id: claimed.claim_id,
        final_body: 'done-1',
      },
    );
    checkEqual('complete 后 delivery 状态为 completed', completed.delivery.state, 'completed');
    check('complete 返回最终消息', Boolean(completed.message?.message_id), completed.message);

    const completedHistory = await callTool<HistoryPage>(agentA.client, 'network_history_list', {
      room_id: roomId,
      limit: 20,
    });
    const completedRow = completedHistory.items.find(
      (item) => item.message_id === invoke.message.message_id,
    );
    checkEqual('complete 后发送方 history 显示 completed 回执', completedRow?.delivery_states.completed, 1);
    await waitUntil(
      '等待 complete 回执房间通知',
      () => senderCollector.events.length > notificationsBeforeComplete,
    );
    await senderRoomSubscription.close();

    const afterCompleteHistory = await callTool<HistoryPage>(agentA.client, 'network_history_list', {
      room_id: roomId,
      limit: 5,
    });
    check(
      'A 能看到 B 的最终答复',
      afterCompleteHistory.items.some(
        (item) => item.message_id === completed.message?.message_id && item.sender_id === b.actor_id,
      ),
      afterCompleteHistory.items,
    );
    checkEqual('最终答复的 reply_to 指向任务消息', completed.message?.reply_to, invoke.message.message_id);

    const staleComplete = await expectToolError(
      '重复 complete 返回 STALE_ATTEMPT',
      callTool(agentB.client, 'network_delivery_complete', {
        request_id: newRequestId('complete'),
        delivery_id: deliveryId,
        attempt: claimed.attempt,
        claim_id: claimed.claim_id,
      }),
      'STALE_ATTEMPT',
    );
    check('STALE_ATTEMPT 语义清晰', staleComplete.body.message !== undefined, staleComplete.body);

    const completedInbox = await inbox(agentB, 'completed');
    check(
      'completed 过滤能看到该 delivery',
      completedInbox.items.some((item) => item.delivery_id === deliveryId),
      completedInbox.items,
    );

    const concurrentA = await sendMessage(agentA, roomId, 'task-race-1', { invoke: [b.actor_id] });
    const concurrentB = await sendMessage(agentA, roomId, 'task-race-2', { invoke: [b.actor_id] });
    const raceDeliveryA = concurrentA.delivery_ids[0] as string;
    const raceDeliveryB = concurrentB.delivery_ids[0] as string;
    const raceResults = await Promise.allSettled([
      callTool<Delivery>(agentB.client, 'network_delivery_claim', {
        request_id: newRequestId('claim'),
        delivery_id: raceDeliveryA,
      }),
      callTool<Delivery>(agentB.client, 'network_delivery_claim', {
        request_id: newRequestId('claim'),
        delivery_id: raceDeliveryB,
      }),
    ]);
    const fulfilled = raceResults.filter((result) => result.status === 'fulfilled');
    checkEqual('并发 claim 恰好一个成功', fulfilled.length, 1);
    const rejected = raceResults.find((result) => result.status === 'rejected') as PromiseRejectedResult;
    checkEqual('另一个并发 claim 返回 CLAIM_CONFLICT', (rejected.reason as { code?: string }).code, 'CLAIM_CONFLICT');
    const racedClaim = (fulfilled[0] as PromiseFulfilledResult<Delivery>).value;
    const racedDelivery = racedClaim.delivery_id;
    const idleDelivery = racedDelivery === raceDeliveryA ? raceDeliveryB : raceDeliveryA;

    await callTool(agentA.client, 'network_delivery_retry', {
      request_id: newRequestId('retry'),
      delivery_id: racedDelivery,
      expected_attempt: racedClaim.attempt,
    });
    const retried = await inbox(agentB, 'all');
    const retriedView = retried.items.find((item) => item.delivery_id === racedDelivery);
    checkEqual('retry 后回到 pending', retriedView?.state, 'pending');
    checkEqual('retry 不改变 attempt', retriedView?.attempt, racedClaim.attempt);
    await expectToolError(
      '旧 claim 在 retry 后失效',
      callTool(agentB.client, 'network_delivery_complete', {
        request_id: newRequestId('complete'),
        delivery_id: racedDelivery,
        attempt: racedClaim.attempt,
        claim_id: racedClaim.claim_id,
      }),
      'STALE_ATTEMPT',
    );

    const reclaim = await callTool<Delivery>(agentB.client, 'network_delivery_claim', {
      request_id: newRequestId('claim'),
      delivery_id: racedDelivery,
    });
    checkEqual('重新 claim 后 attempt 递增', reclaim.attempt, racedClaim.attempt + 1);
    const failed = await callTool<Delivery>(agentB.client, 'network_delivery_fail', {
      request_id: newRequestId('fail'),
      delivery_id: racedDelivery,
      attempt: reclaim.attempt,
      claim_id: reclaim.claim_id,
      error: '模型暂时不可用',
    });
    checkEqual('fail 后状态为 failed', failed.state, 'failed');

    await callTool(agentA.client, 'network_delivery_retry', {
      request_id: newRequestId('retry'),
      delivery_id: racedDelivery,
      expected_attempt: reclaim.attempt,
    });
    const finalClaim = await callTool<Delivery>(agentB.client, 'network_delivery_claim', {
      request_id: newRequestId('claim'),
      delivery_id: racedDelivery,
    });
    checkEqual('retry 后再次 claim 的 attempt', finalClaim.attempt, reclaim.attempt + 1);
    const cancelled = await callTool<Delivery>(agentA.client, 'network_delivery_cancel', {
      request_id: newRequestId('cancel'),
      delivery_id: racedDelivery,
      reason: '任务已被撤销',
    });
    checkEqual('cancel 后状态为 cancelled', cancelled.state, 'cancelled');
    await expectToolError(
      '取消后旧 claim 不能 complete',
      callTool(agentB.client, 'network_delivery_complete', {
        request_id: newRequestId('complete'),
        delivery_id: racedDelivery,
        attempt: finalClaim.attempt,
        claim_id: finalClaim.claim_id,
      }),
      'STALE_ATTEMPT',
    );

    await callTool(agentA.client, 'network_delivery_cancel', {
      request_id: newRequestId('cancel'),
      delivery_id: idleDelivery,
      reason: '竞态测试结束',
    });
    await expectToolError(
      '取消 pending 后 claim 返回 CLAIM_CONFLICT',
      callTool(agentB.client, 'network_delivery_claim', {
        request_id: newRequestId('claim'),
        delivery_id: idleDelivery,
      }),
      'CLAIM_CONFLICT',
    );

    const idempotentClientId = `cm_${crypto.randomUUID()}`;
    const firstSend = await sendMessage(agentA, roomId, 'idempotent-body', {
      clientMessageId: idempotentClientId,
    });
    const secondSend = await sendMessage(agentA, roomId, 'idempotent-body', {
      clientMessageId: idempotentClientId,
    });
    checkEqual('相同 client_message_id 返回同一消息', secondSend.message.message_id, firstSend.message.message_id);
    checkEqual('相同 client_message_id 不新增 delivery', secondSend.delivery_ids.length, firstSend.delivery_ids.length);
    await expectToolError(
      '相同 client_message_id 携带不同正文返回 IDEMPOTENCY_CONFLICT',
      sendMessage(agentA, roomId, 'idempotent-body-changed', {
        clientMessageId: idempotentClientId,
      }),
      'IDEMPOTENCY_CONFLICT',
    );

    const syncInbox = await callTool<{
      items: { type: string }[];
      next_cursor: string;
      reset_required: boolean;
    }>(agentB.client, 'network_sync', { scope: 'inbox' });
    check('inbox sync 返回变更事件', syncInbox.items.length > 0, syncInbox.items.length);
    check('inbox sync 不需要重置', syncInbox.reset_required === false);
    check('inbox sync 返回 next_cursor', Boolean(syncInbox.next_cursor));

    const syncRoom = await callTool<{ items: { type: string }[]; scope: string }>(
      agentB.client,
      'network_sync',
      { scope: `room:${roomId}` },
    );
    check('room sync 返回变更事件', syncRoom.items.length > 0, syncRoom.items.length);
    checkEqual('room sync scope 保持原样', syncRoom.scope, `room:${roomId}`);

    const syncDenied = await expectToolError(
      '非成员读取房间 sync 返回 NOT_MEMBER',
      callTool(humanC.client, 'network_sync', { scope: `room:${roomId}` }),
      'NOT_MEMBER',
    );
    check('sync 越权错误可读', syncDenied.body.retryable === false, syncDenied.body);

    const crossInbox = await rawRpc(
      'subscriptions/listen',
      { notifications: { resourceSubscriptions: [`agentnet://actors/${a.actor_id}/inbox`] } },
      { token: agentB.token },
    );
    const crossError = crossInbox.body.error as Record<string, unknown> | undefined;
    checkEqual('订阅他人 inbox 被拒绝', crossError?.code, -32602);
    checkEqual('越权订阅错误码为 NOT_MEMBER', (crossError?.data as Record<string, unknown> | undefined)?.code, 'NOT_MEMBER');

    const group = await callTool<RoomDetail>(agentA.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'group',
      members: [b.actor_id],
      title: '成员变化验收群',
    });
    const groupId = group.room.room_id;
    checkEqual('群聊创建者为 owner', group.room.my_role, 'owner');
    const groupSubscription = await openSubscription(agentB.client, [
      `agentnet://rooms/${groupId}/head`,
    ]);
    await waitUntil('等待订阅建立', () => Boolean(groupSubscription.honoredFilter.resourceSubscriptions?.length));
    await callTool(agentA.client, 'network_room_members_update', {
      request_id: newRequestId('members'),
      room_id: groupId,
      remove: [b.actor_id],
    });
    const closedReason = await waitClosed(groupSubscription);
    // 服务端以 listen 结果优雅结束订阅流；客户端应重建并重新鉴权。
    checkEqual('成员被移除后服务端关闭订阅流', closedReason, 'graceful');
    await expectToolError(
      '被移除成员不能继续读房间',
      callTool(agentB.client, 'network_room_get', { room_id: groupId }),
      'NOT_MEMBER',
    );

    await subscription.close();
    checkEqual('主动关闭订阅标记为 local', await waitClosed(subscription, 5000), 'local');
    await sleep(1000);
    const offlineCollector = notificationCollector(agentB.client);
    const resubscribed = await openSubscription(agentB.client, ['agentnet://me']);
    checkEqual(
      '同一客户端可重新建立订阅',
      resubscribed.honoredFilter.resourceSubscriptions?.[0],
      'agentnet://me',
    );
    const offlineSend = await sendMessage(agentA, roomId, 'offline-task', {
      invoke: [b.actor_id],
    });
    await sleep(1500);
    check(
      '订阅建立但未上线时收不到 inbox 提示',
      offlineCollector.events.every((event) => event.params.uri !== 'agentnet://me'),
      offlineCollector.events,
    );
    await expectToolError(
      '人类调用 network_login 返回 INVALID_ARGUMENT',
      callTool(humanC.client, 'network_login'),
      'INVALID_ARGUMENT',
    );
    const relogin = await callTool<LoginResult>(agentB.client, 'network_login');
    checkEqual('断线后可重新上线', relogin.online, true);
    check('上线返回排队待办数量', relogin.pending_count >= 1, relogin);
    await offlineCollector.waitFor(
      (event) => event.params.uri === 'agentnet://me',
      '等待上线补发提示',
    );
    const queued = await inbox(agentB, 'pending');
    check(
      '上线后可读到离线期间排队的投递',
      queued.items.some((item) => item.delivery_id === offlineSend.delivery_ids[0]),
      queued.items,
    );
    const meEvents = offlineCollector.events.filter(
      (event) => event.params.uri === 'agentnet://me',
    ).length;
    await sendMessage(agentA, roomId, 'online-task', { invoke: [b.actor_id] });
    await waitUntil(
      '上线后收到新的 inbox 提示',
      () =>
        offlineCollector.events.filter((event) => event.params.uri === 'agentnet://me').length >
        meEvents,
    );
    await resubscribed.close();

    // 网页人类发送回归：页面把房间内 Agent 成员填入 invoke，人类成员不产生 delivery。
    const webCollector = notificationCollector(agentB.client);
    const webSubscription = await openSubscription(agentB.client, ['agentnet://me']);
    await waitUntil('等待网页回归订阅建立', () =>
      Boolean(webSubscription.honoredFilter.resourceSubscriptions?.length),
    );
    await callTool(agentB.client, 'network_login');
    await webCollector.waitFor(
      (event) => event.params.uri === 'agentnet://me',
      '等待网页回归上线提示',
    );
    webCollector.reset();
    const webRoom = await callTool<RoomDetail>(humanC.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'direct',
      members: [b.actor_id],
    });
    const webRoomId = webRoom.room.room_id;
    check(
      '网页人类单聊成员包含 human 与 agent 类型',
      webRoom.members.some((member) => member.actor_id === c.actor_id && member.kind === 'human') &&
        webRoom.members.some((member) => member.actor_id === b.actor_id && member.kind === 'agent'),
      webRoom.members,
    );
    const inboxBeforeWeb = await inbox(agentB, 'pending');
    const webSend = await sendMessage(humanC, webRoomId, 'web-human-hello', {
      invoke: [b.actor_id],
    });
    checkEqual('网页人类发送为 Agent 创建 delivery', webSend.delivery_ids.length, 1);
    const webEvent = await webCollector.waitFor(
      (event) => event.params.uri === 'agentnet://me',
      '等待网页消息触发 agentnet://me 通知',
    );
    check(
      '网页消息触发 agentnet://me 通知',
      Boolean(subscriptionIdOf(webEvent.params)),
      webEvent.params,
    );
    const inboxAfterWeb = await inbox(agentB, 'pending');
    check(
      'Agent inbox 出现网页消息 delivery',
      inboxAfterWeb.items.some((item) => item.delivery_id === webSend.delivery_ids[0]),
      inboxAfterWeb.items,
    );
    checkEqual(
      '网页消息使 Agent pending_count 增加 1',
      inboxAfterWeb.pending_count,
      inboxBeforeWeb.pending_count + 1,
    );
    const humanInbox = await inbox(humanC, 'all');
    check(
      '人类发送者没有给自己的 delivery',
      !humanInbox.items.some((item) => item.message_id === webSend.message.message_id),
      humanInbox.items,
    );
    await expectToolError(
      '网页人类发送不能 invoke 人类成员',
      sendMessage(humanC, webRoomId, 'web-human-target', { invoke: [c.actor_id] }),
      'INVALID_ARGUMENT',
    );
    await webSubscription.close();

    // 群聊：服务端按 invoke 显式创建 delivery；页面只把被 @ 的 Agent 填入 invoke。
    const webGroup = await callTool<RoomDetail>(agentA.client, 'network_room_create', {
      request_id: newRequestId('room'),
      kind: 'group',
      members: [b.actor_id, c.actor_id],
      title: '网页群聊回归',
    });
    const webGroupId = webGroup.room.room_id;
    const groupSend = await sendMessage(humanC, webGroupId, 'web-group-hello', {
      invoke: [a.actor_id, b.actor_id],
    });
    checkEqual('显式 invoke 多个 Agent 时各创建一条 delivery', new Set(groupSend.delivery_ids).size, 2);
    const groupInboxA = await inbox(agentA, 'pending');
    const groupInboxB = await inbox(agentB, 'pending');
    check(
      '显式 invoke 的目标都出现在 inbox',
      groupInboxA.items.some((item) => item.message_id === groupSend.message.message_id) &&
        groupInboxB.items.some((item) => item.message_id === groupSend.message.message_id),
      { a: groupInboxA.items, b: groupInboxB.items },
    );
    const groupHumanInbox = await inbox(humanC, 'all');
    check(
      '网页群聊不产生人类成员 delivery',
      !groupHumanInbox.items.some((item) => item.message_id === groupSend.message.message_id),
      groupHumanInbox.items,
    );

    const mentionSend = await sendMessage(
      humanC,
      webGroupId,
      '@验收 Agent A 全局名 请处理这条群消息',
      { invoke: [a.actor_id] },
    );
    checkEqual('群 @ 只为被 @ 的 Agent 创建 delivery', mentionSend.delivery_ids.length, 1);
    const mentionInboxA = await inbox(agentA, 'pending');
    const mentionInboxB = await inbox(agentB, 'pending');
    check(
      '被 @ 的 Agent A 收到群消息 delivery',
      mentionInboxA.items.some((item) => item.message_id === mentionSend.message.message_id),
      mentionInboxA.items,
    );
    check(
      '未 @ 的 Agent B 没有该群消息 delivery',
      !mentionInboxB.items.some((item) => item.message_id === mentionSend.message.message_id),
      mentionInboxB.items,
    );

    const plainGroupSend = await sendMessage(humanC, webGroupId, '没有 @ 的普通群消息');
    checkEqual('未 @ 的群消息不创建 delivery', plainGroupSend.delivery_ids.length, 0);
    const plainInboxA = await inbox(agentA, 'pending');
    const plainInboxB = await inbox(agentB, 'pending');
    check(
      '普通群消息不进入任何 Agent inbox',
      !plainInboxA.items.some((item) => item.message_id === plainGroupSend.message.message_id) &&
        !plainInboxB.items.some((item) => item.message_id === plainGroupSend.message.message_id),
      { a: plainInboxA.items, b: plainInboxB.items },
    );

    const resourceMe = await readResourceJson<{ actor_id: string }>(agentB.client, 'agentnet://me');
    checkEqual('resources/read agentnet://me 返回身份', resourceMe.actor_id, b.actor_id);

    const badScope = await expectToolError(
      '未知 sync scope 返回 INVALID_ARGUMENT',
      callTool(agentB.client, 'network_sync', { scope: 'everything' }),
      'INVALID_ARGUMENT',
    );
    check('未知 scope 错误可读', typeof badScope.body.message === 'string', badScope.body);

    console.log(`端到端验收通过: ${checksPassed()} 项`);
  } finally {
    await Promise.all([agentA.close(), agentB.close(), humanC.close()]);
  }
}

await main().catch((error) => {
  console.error('端到端验收失败:', error);
  process.exit(1);
});
