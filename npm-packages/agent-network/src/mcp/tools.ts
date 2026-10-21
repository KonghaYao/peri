import type { McpServer, StandardSchemaWithJSON } from '@modelcontextprotocol/server';
import { z } from 'zod';

import { isBusinessError } from '../contracts/errors';
import {
  agentRenameSchema,
  contactAddSchema,
  contactRemoveSchema,
  contactsListSchema,
  deliveryCancelSchema,
  deliveryClaimSchema,
  deliveryCompleteSchema,
  deliveryFailSchema,
  deliveryReportSchema,
  deliveryRetrySchema,
  historyListSchema,
  inboxListSchema,
  loginSchema,
  meSchema,
  messageReadSchema,
  messageSendSchema,
  profileUpdateSchema,
  recoveryCreateSchema,
  recoveryRedeemSchema,
  registerSchema,
  roomCreateSchema,
  roomGetSchema,
  roomLeaveSchema,
  roomMarkReadSchema,
  roomMembersUpdateSchema,
  roomsListSchema,
  syncSchema,
} from '../contracts/schemas';
import { contactAdd, contactRemove, contactsList } from '../domain/contacts';
import {
  deliveryCancel,
  deliveryClaim,
  deliveryComplete,
  deliveryFail,
  deliveryReport,
  deliveryRetry,
  inboxList,
} from '../domain/deliveries';
import {
  me,
  recoveryCreate,
  recoveryRedeem,
  register as registerActor,
  renameAgent,
  updateProfile,
} from '../domain/identity';
import {
  historyList,
  messageRead,
  messageSend,
  roomMarkRead,
} from '../domain/messages';
import {
  roomCreate,
  roomGet,
  roomLeave,
  roomMembersUpdate,
  roomsList,
} from '../domain/rooms';
import { sync } from '../domain/sync';
import type { Env } from '../env';
import { login } from '../realtime/presence';
import { requireActor } from './auth';
import {
  agentRenameResultSchema,
  completeResultSchema,
  contactPageSchema,
  contactSchema,
  deliverySchema,
  historyPageSchema,
  identitySchema,
  inboxPageSchema,
  leaveResultSchema,
  loginResultSchema,
  markReadResultSchema,
  progressResultSchema,
  readChunkSchema,
  recoveredIdentitySchema,
  recoveryTicketSchema,
  removedResultSchema,
  roomDetailSchema,
  roomPageSchema,
  sendResultSchema,
  syncPageSchema,
} from './outputs';
import type { Principal } from '../domain/principal';

type ToolReturn = {
  content: { type: 'text'; text: string }[];
  structuredContent?: Record<string, unknown>;
  isError?: boolean;
};

/**
 * SDK 的 registerTool 重载在泛型封装下无法正确选择 Standard Schema 分支；
 * 运行时行为不变，这里只收敛类型断言。
 */
type RegisterToolCall = (
  name: string,
  config: Record<string, unknown>,
  callback: (args: unknown) => Promise<ToolReturn>,
) => unknown;

function failure(error: unknown): ToolReturn {
  if (isBusinessError(error)) {
    return {
      isError: true,
      content: [{ type: 'text', text: JSON.stringify(error.toBody()) }],
    };
  }
  console.error('agent-network tool failed', error);
  return {
    isError: true,
    content: [
      {
        type: 'text',
        text: JSON.stringify({ code: 'INTERNAL_ERROR', message: '内部错误', retryable: false }),
      },
    ],
  };
}

function register<
  I extends StandardSchemaWithJSON,
  O extends StandardSchemaWithJSON,
>(
  server: McpServer,
  name: string,
  description: string,
  inputSchema: I,
  outputSchema: O,
  handler: (
    args: StandardSchemaWithJSON.InferInput<I>,
  ) => Promise<StandardSchemaWithJSON.InferOutput<O>>,
): void {
  const call = server.registerTool as unknown as RegisterToolCall;
  call.call(server, name, { description, inputSchema, outputSchema }, async (args) => {
    try {
      const value = (await handler(args as StandardSchemaWithJSON.InferInput<I>)) as Record<
        string,
        unknown
      >;
      return {
        content: [{ type: 'text' as const, text: JSON.stringify(value) }],
        structuredContent: value,
      };
    } catch (error) {
      return failure(error);
    }
  });
}

export function registerTools(
  server: McpServer,
  env: Env,
  principal: Principal,
): void {
  const db = env.DB;
  const origin = env.PUBLIC_ORIGIN;

  register(
    server,
    'network_register',
    '注册 human 或 agent actor，并原子绑定当前 Bearer 凭据。重复调用返回同一身份。',
    registerSchema,
    identitySchema,
    async (input) => registerActor(db, principal, input, origin),
  );

  register(server, 'network_me', '读取当前身份、资源入口与限制。', meSchema, identitySchema, async () => {
    requireActor(principal);
    return me(db, principal, origin);
  });

  register(
    server,
    'network_login',
    'Agent 显式上线，开始接收 inbox 投递通知；未上线时投递只在 D1 排队。重复调用幂等，返回待办数量。',
    loginSchema,
    loginResultSchema,
    async () => login(env, principal),
  );

  register(
    server,
    'network_profile_update',
    '更新自己的展示名称与小体积 metadata。',
    profileUpdateSchema,
    identitySchema,
    async (input) => {
      requireActor(principal);
      return updateProfile(db, principal, input, origin);
    },
  );

  register(
    server,
    'network_agent_rename',
    '全局重命名一个 Agent actor；任何已注册身份都可调用，人类 actor 不能被此工具改名。',
    agentRenameSchema,
    agentRenameResultSchema,
    async (input) => {
      requireActor(principal);
      return renameAgent(db, principal, input);
    },
  );

  register(
    server,
    'network_recovery_create',
    '创建 3 分钟有效、一次使用的跨设备恢复 ticket。',
    recoveryCreateSchema,
    recoveryTicketSchema,
    async (input) => {
      requireActor(principal);
      return recoveryCreate(db, principal, input);
    },
  );

  register(
    server,
    'network_recovery_redeem',
    '用恢复 ticket 把当前新凭据绑定到原 actor。',
    recoveryRedeemSchema,
    recoveredIdentitySchema,
    async (input) => recoveryRedeem(db, principal, input, origin),
  );

  register(
    server,
    'network_contacts_list',
    '按创建时间倒序分页读取自己的好友。',
    contactsListSchema,
    contactPageSchema,
    async (input) => {
      requireActor(principal);
      return contactsList(db, principal, input);
    },
  );

  register(
    server,
    'network_contact_add',
    '按 actor_id 或 agent_session_id 幂等添加好友。',
    contactAddSchema,
    z.object({ contact: contactSchema }),
    async (input) => {
      requireActor(principal);
      return contactAdd(db, principal, input);
    },
  );

  register(
    server,
    'network_contact_remove',
    '删除自己的好友关系；不删除历史消息或群成员身份。',
    contactRemoveSchema,
    removedResultSchema,
    async (input) => {
      requireActor(principal);
      return contactRemove(db, principal, input);
    },
  );

  register(
    server,
    'network_room_create',
    '创建单聊或群聊。单聊按 actor 对唯一收敛；群聊创建者成为 owner。',
    roomCreateSchema,
    roomDetailSchema,
    async (input) => {
      requireActor(principal);
      return roomCreate(db, principal, input);
    },
  );

  register(
    server,
    'network_rooms_list',
    '分页读取自己参与的活跃房间摘要、head 与已读位置。',
    roomsListSchema,
    roomPageSchema,
    async (input) => {
      requireActor(principal);
      return roomsList(db, principal, input);
    },
  );

  register(
    server,
    'network_room_get',
    '读取房间摘要与当前有效成员。',
    roomGetSchema,
    roomDetailSchema,
    async (input) => {
      requireActor(principal);
      return roomGet(db, principal, input.room_id);
    },
  );

  register(
    server,
    'network_room_members_update',
    '群 owner 添加/移除成员或转移 owner。移除会取消该成员在房间内未完成的工作。',
    roomMembersUpdateSchema,
    roomDetailSchema,
    async (input) => {
      requireActor(principal);
      return roomMembersUpdate(db, principal, input);
    },
  );

  register(
    server,
    'network_room_leave',
    '离开群聊；owner 必须先转移 owner。',
    roomLeaveSchema,
    leaveResultSchema,
    async (input) => {
      requireActor(principal);
      return roomLeave(db, principal, input);
    },
  );

  register(
    server,
    'network_message_send',
    '向房间发送消息，并为 invoke 中每个 Agent 成员创建一条 delivery。普通回复不要填写 invoke。',
    messageSendSchema,
    sendResultSchema,
    async (input) => {
      requireActor(principal);
      return messageSend(db, principal, input);
    },
  );

  register(
    server,
    'network_history_list',
    '按固定快照上界和 keyset 游标分页读取房间历史预览；正文用 network_message_read。',
    historyListSchema,
    historyPageSchema,
    async (input) => {
      requireActor(principal);
      return historyList(db, principal, input);
    },
  );

  register(
    server,
    'network_message_read',
    '按 UTF-8 字节分块读取消息正文。',
    messageReadSchema,
    readChunkSchema,
    async (input) => {
      requireActor(principal);
      return messageRead(db, principal, input);
    },
  );

  register(
    server,
    'network_room_mark_read',
    '单调推进自己在房间内的已读位置。',
    roomMarkReadSchema,
    markReadResultSchema,
    async (input) => {
      requireActor(principal);
      return roomMarkRead(db, principal, input);
    },
  );

  register(
    server,
    'network_sync',
    '读取已授权 scope（inbox 或 room:<room_id>）的变更引用与下一游标。',
    syncSchema,
    syncPageSchema,
    async (input) => {
      requireActor(principal);
      return sync(db, principal, input);
    },
  );

  register(
    server,
    'network_inbox_list',
    '分页读取自己的待办与领取记录；默认返回 pending 和 claimed。',
    inboxListSchema,
    inboxPageSchema,
    async (input) => {
      requireActor(principal);
      return inboxList(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_claim',
    '原子领取一条交付；每个 Agent 同时最多一条 claimed。重试同一 request_id 返回同一 claim_id。',
    deliveryClaimSchema,
    deliverySchema,
    async (input) => {
      requireActor(principal);
      return deliveryClaim(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_report',
    '上报当前 attempt 的阶段状态；这不是自动心跳。',
    deliveryReportSchema,
    progressResultSchema,
    async (input) => {
      requireActor(principal);
      return deliveryReport(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_complete',
    '用有效 claim 原子完成交付，可同时发布最终答复消息。',
    deliveryCompleteSchema,
    completeResultSchema,
    async (input) => {
      requireActor(principal);
      return deliveryComplete(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_fail',
    '用有效 claim 显式标记交付失败。',
    deliveryFailSchema,
    deliverySchema,
    async (input) => {
      requireActor(principal);
      return deliveryFail(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_cancel',
    '请求者、房间 owner 或接收者取消未完成的交付。',
    deliveryCancelSchema,
    deliverySchema,
    async (input) => {
      requireActor(principal);
      return deliveryCancel(db, principal, input);
    },
  );

  register(
    server,
    'network_delivery_retry',
    '请求者或房间 owner 显式创建下一次尝试；expected_attempt 必须匹配。旧的 claim 随即失效。',
    deliveryRetrySchema,
    deliverySchema,
    async (input) => {
      requireActor(principal);
      return deliveryRetry(db, principal, input);
    },
  );
}
