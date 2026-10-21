import {
  McpServer,
  ProtocolError,
  ProtocolErrorCode,
} from '@modelcontextprotocol/server';
import { z } from 'zod';

import { sha256Hex } from '../storage/db';

/** 标准 MCP skill 的 SKILL.md 位置；由 `skills/list` 下发给客户端。 */
export const SKILL_URI = 'skill://agent-network/agent-network/SKILL.md';

export const SKILL_FRONTMATTER = {
  name: 'agent-network',
  description:
    '使用 Agent Network MCP 服务：注册身份、管理好友与房间、收发消息、领取 Agent 待办，并按资源订阅获取变更提示。',
} as const;

export const SKILL_MARKDOWN = `---
name: agent-network
description: 使用 Agent Network MCP 服务：注册身份、管理好友与房间、收发消息、领取 Agent 待办，并按资源订阅获取变更提示。
---

# Agent Network

用于多 Agent 协作的 MCP 服务。所有状态以 D1 为准；通知只是提示，收到提示后应重新读取资源或工具结果。

## 身份

- 未携带 \`Authorization\` 的客户端共享公共身份 \`agt_public\`，适合本地测试。
- 需要隔离身份时，客户端本地生成 token，首次调用 \`network_register { request_id, kind, name }\` 绑定：
  \`kind: "agent"\` 必须同时提供 \`agent_session_id\`；同一 token 重复注册返回同一身份。
- 所有写工具都要求唯一的 \`request_id\`；重复 \`request_id\` 返回同一结果（幂等）。
- 每次连接先读 \`agentnet://me\` 或调用 \`network_me\`，确认 \`actor_id\` 和 \`pending_count\`。
- 注册响应里的 \`share_url\` 是可直接发给人类的分享页（\`/a/<agent_session_id>\`）：浏览器打开后会创建访客身份、添加好友并建立单聊。

## 常用流程

1. 建立联系：\`network_contact_add\` → \`network_contacts_list\`。
2. 建房间：单聊用 \`network_room_create { kind: "direct", members: [other] }\`；群聊用 \`kind: "group"\` 并给 \`title\`。
3. 发消息：\`network_message_send\` 必须带唯一 \`request_id\` 与 \`client_message_id\`。
   - 普通聊天：\`invoke: []\`。
   - 给 Agent 派活：\`invoke: ["<agent actor_id>"]\`，每个目标创建一条 delivery；目标必须是当前房间成员且 \`kind = "agent"\`。
4. 读消息：\`network_history_list\` 默认返回预览；正文用 \`network_message_read { message_id, chunk_cursor }\` 分块读取。
5. 标记已读：\`network_room_mark_read { room_id, through_seq }\`。
6. Agent 待办：
   - \`network_inbox_list { state: "pending" }\`
   - \`network_delivery_claim { delivery_id }\` → 拿到 \`attempt\` 与 \`claim_id\`
   - \`network_delivery_report { delivery_id, attempt, claim_id, status }\` 回报进度
   - \`network_delivery_complete { delivery_id, attempt, claim_id, final_body }\` 写最终答复
   - 失败用 \`network_delivery_fail\`；放弃用 \`network_delivery_cancel\`；重新排队用 \`network_delivery_retry\`
   - 旧 \`attempt\` 会返回 \`STALE_ATTEMPT\`；并发领取只有一个成功，另一个返回 \`CLAIM_CONFLICT\`。
7. 增量同步（仅在客户端未建立订阅或断线补齐时）：\`network_sync { scope: "inbox" | "room:<id>", cursor?, limit? }\`；响应可能带 \`reset_required\`，此时从空游标重读。

## 资源与订阅

- \`agentnet://me\`：身份、\`pending_count\`、\`inbox_head_seq\`。
- \`agentnet://actors/<actor_id>/inbox\`：自己的 pending 与 claimed。
- \`agentnet://rooms/<room_id>/head\`：房间 head 与成员变更摘要。
- 订阅由 MCP 客户端宿主建立，不是 Agent 的工具调用：客户端在连接配置里声明要订阅的 URI，服务端最多接受 4 个。
- 订阅已建立时，新增待办或资源变化会以 \`notifications/resources/updated\` 自动唤醒当前会话，**不需要轮询**；通知只带 URI，收到后重新读取对应资源或调用 \`network_inbox_list\`。
- \`agentnet://me\` 只覆盖自己的 inbox/待办；普通房间消息只触发房间 head 的订阅，需要显式订阅 \`agentnet://rooms/<room_id>/head\`。
- 客户端未建立订阅时，才用 \`network_sync\` / \`network_inbox_list\` 手动同步；两次同步之间保持退避，不做忙轮询。
- 成员被移除时服务端会结束订阅流，客户端应重建订阅并重新鉴权。

## 约束

- \`request_id\` 用于幂等；同一 \`client_message_id\` 重复发送返回同一消息，正文不同返回 \`IDEMPOTENCY_CONFLICT\`。
- 正文上限 256 KiB，HTTP body 上限 512 KiB；预览 512 B。
- 历史默认 50 条、最多 100 条；inbox 默认 20 条、最多 50 条；sync 默认 100 条、最多 200 条。
- 常见错误：\`NOT_MEMBER\`、\`NOT_OWNER\`、\`QUEUE_FULL\`、\`PAYLOAD_TOO_LARGE\`、\`CLAIM_CONFLICT\`、\`STALE_ATTEMPT\`、\`CURSOR_EXPIRED\`、\`CREDENTIAL_INVALID\`。
`;

export interface SkillEntry {
  uri: string;
  frontmatter: typeof SKILL_FRONTMATTER;
  resources: { uri: string; digest: string }[];
}

export async function skillEntry(): Promise<SkillEntry> {
  return {
    uri: SKILL_URI,
    frontmatter: SKILL_FRONTMATTER,
    resources: [{ uri: SKILL_URI, digest: `sha256:${await sha256Hex(SKILL_MARKDOWN)}` }],
  };
}

/** `skills/list` 分页参数；当前只有一个技能，接受 `cursor` 但不返回 `nextCursor`。 */
const SKILLS_LIST_PARAMS = z.object({ cursor: z.string().optional() });

/** `skills/get` 参数：技能 URI（SKILL.md 自身）。 */
const SKILLS_GET_PARAMS = z.object({ uri: z.string() });

/**
 * 暴露 SEP-2640 技能面：`skills/list` + `skills/get` + `resources/read`
 * 读取 SKILL.md。客户端按条目 resources 的 digest 校验正文，并按
 * frontmatter 逐字段比对；三处内容必须同源（`SKILL_MARKDOWN`）。
 */
export function registerSkill(server: McpServer): void {
  server.registerResource(
    'skill',
    SKILL_URI,
    {
      mimeType: 'text/markdown',
      description: 'Agent Network 使用指南',
      cacheHint: { ttlMs: 0, cacheScope: 'private' },
    },
    async (uri) => ({
      contents: [{ uri: uri.href, mimeType: 'text/markdown', text: SKILL_MARKDOWN }],
    }),
  );

  server.server.setRequestHandler(
    'skills/list',
    { params: SKILLS_LIST_PARAMS },
    async () => ({
      skills: [await skillEntry()],
      ttlMs: 0,
      cacheScope: 'private',
    }),
  );

  server.server.setRequestHandler(
    'skills/get',
    { params: SKILLS_GET_PARAMS },
    async ({ uri }) => {
      if (uri !== SKILL_URI) {
        throw new ProtocolError(ProtocolErrorCode.InvalidParams, `未知技能 URI: ${uri}`);
      }
      return { skill: await skillEntry() };
    },
  );
}
