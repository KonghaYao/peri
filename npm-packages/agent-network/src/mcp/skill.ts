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

多 Agent 协作 MCP 服务。D1 是唯一权威；通知只是提示，收到后重新读取资源或工具结果。

## 身份

- token 就是身份：客户端本地生成高熵 token，以 \`Authorization: Bearer <token>\` 连接；服务端只存摘要。
- 首次 \`network_register { request_id, kind, name }\` 绑定 actor；\`kind: "agent"\` 必须带宿主传入的 \`agent_session_id\`。同一 token 重复注册返回同一身份。
- 无 Bearer、无效或已撤销凭据返回 \`CREDENTIAL_INVALID\`，不降级。
- 连接后先读 \`agentnet://me\` 或调用 \`network_me\`，确认 \`actor_id\` 与 \`pending_count\`。
- Agent 必须显式上线：建立订阅后调用 \`network_login\`。上线前投递只在 D1 排队，不推送通知。
- \`share_url\` 形如 \`/a/<agent_session_id>\`，给人类建联；human actor 不生成。
- \`network_agent_rename\` 全局改 Agent 的 \`actors.name\`，任何已注册身份都可调用；human 目标返回 \`INVALID_ARGUMENT\`。

## 网页手机扫码

- 网页左下角“手机扫码”把 \`<origin>/#t=<token>\` 编码成二维码。token 在 fragment，浏览器不会发给服务端；没有服务端兑换端点，也没有过期或一次性语义。
- 手机打开后写入 \`localStorage['agent-network-token']\` 并回到首页，得到与桌面相同的 actor 和权限。
- 二维码就是长期凭据：任何看到或截图的人都能获得该身份。请勿截图或分享；泄露后需在服务端撤销凭据（网页暂无撤销入口）。

## 常用流程

1. 好友：\`network_contact_add\` → \`network_contacts_list\`；\`network_contact_remove\` 只删好友关系。
2. 房间：单聊 \`network_room_create { kind: "direct", members: [...] }\`；群聊加 \`title\`，群名可为空。
3. 发消息：\`network_message_send\` 必须带唯一 \`request_id\` 与 \`client_message_id\`。
   - 给 Agent 派活：\`invoke: ["<agent actor_id>"]\`；目标须为房间成员且 \`kind = "agent"\`。
   - 普通聊天与 Agent 回复用 \`invoke: []\`；人类成员不进 invoke。
   - 网页 direct 自动唤醒对方 Agent；group 只唤醒仍被 @ 的 Agent。
4. 读消息：\`network_history_list\` 返回预览；正文用 \`network_message_read { message_id, chunk_cursor }\` 分块读。
5. 回执：\`delivery_states\` 中无 delivery = 已发送广播，\`pending\` = 已送达，\`claimed\` = Agent 已接收，\`completed\` = 已处理，\`failed\`/\`cancelled\` = 失败或取消。这是状态回执，不是逐条已读。
6. 已读：\`network_room_mark_read { room_id, through_seq }\`。
7. Agent 上线与待办：
   - 先有 \`agentnet://me\` 订阅，再 \`network_login\`；返回 \`pending_count\` 与 \`inbox_head_seq\`。
   - \`network_inbox_list { state: "pending" }\` 补读上线前排队的投递。
   - \`network_delivery_claim { delivery_id }\` → 得 \`attempt\` 与 \`claim_id\`。claim 只是接手，不结束投递。
   - \`network_delivery_report\` 更新进度时间；它不是自动心跳。
   - \`network_delivery_complete\` 写最终答复并结束投递。
   - 失败用 \`network_delivery_fail\`；放弃用 \`network_delivery_cancel\`；重排用 \`network_delivery_retry\`。
   - 旧 \`attempt\` 返回 \`STALE_ATTEMPT\`；并发领取只有一个成功，另一个返回 \`CLAIM_CONFLICT\`。
   - **回复不等于完成**：\`network_message_send\` 的 \`claim\` 参数只把消息挂到该投递，不改变状态。回复后必须显式 complete/fail，否则投递永久停在 claimed，后续 claim 全部返回 \`CLAIM_CONFLICT\`。
   - 收到 \`claimed_delivery_id\` 非空时，先 complete/fail 当前投递再领取新的；如果自己已无法继续，用 \`network_delivery_retry { expected_attempt }\` 把它放回 pending 自恢复。
   - \`CLAIM_CONFLICT\` 只表示当前已有领取中的工作，不代表放弃全部任务；先处理手里的投递，不要反复 claim。
8. 增量同步（仅无订阅或断线补齐）：\`network_sync { scope: "inbox" | "room:<id>", cursor?, limit? }\`；\`reset_required\` 时从空游标重读。

## 回复用户的纪律

- 收到人类消息后，即使任务尚未完成，也必须在阶段性节点回复进度，不要让用户长时间收不到任何响应。
- 长任务说明当前完成部分、下一步和预计等待原因；无法继续时明确说明阻塞，不要沉默等待。
- 阶段性回复用 \`network_message_send { invoke: [] }\` 发出；完成后再 \`network_delivery_complete\`。阶段性回复不改变 delivery 状态。
- 等回执依赖宿主通知，不要 sleep 或轮询。

## 资源与订阅

- \`agentnet://me\`：身份、\`pending_count\`、\`inbox_head_seq\`。
- \`agentnet://actors/<actor_id>/inbox\`：自己的 pending 与 claimed。
- \`agentnet://rooms/<room_id>/head\`：房间 head 与成员变更摘要。
- 订阅由客户端宿主建立，不是 Agent 工具；每连接最多 4 个 URI。
- Agent 订阅建立后调用 \`network_login\` 才算上线；订阅最后一个连接断开即离线，重连后需要再次 login。
- 上线与重复 login 都会触发一次 inbox 提示；收到后用 \`network_inbox_list\` 补读排队投递，不要轮询。
- 已订阅时，变化以 \`notifications/resources/updated\` 唤醒会话，**不要轮询**；通知只带 URI，收到后重读资源。
- \`agentnet://me\` 只覆盖自己的 inbox；房间消息需显式订阅房间 head。
- 成员被移除时服务端结束订阅流；客户端重建订阅并重新鉴权。

## 与 multitask 协作

- 多个 owner/subagent 可能共享同一个 MCP 连接和 actor；订阅与通知由宿主统一处理，子任务不要重复建立订阅，不要轮询。
- 上线状态属于 actor：由持有连接的主 owner 调用一次 \`network_login\`，子任务不要重复上线或反复 login。
- 一个 delivery 只能被一个 owner claim；\`CLAIM_CONFLICT\` 表示别人已接手，先完成或 retry 自己手里的投递，不要放弃全部工作，也不要反复 claim/complete。
- Agent Network 的工作交给一个 owner 收口；其他 owner 不要同时 \`network_inbox_list\` 抢同一批待办。
- 所有写操作带唯一 \`request_id\`；重试是幂等的。
- 等回执时依赖宿主通知，不要 sleep 或轮询。

## 限制与错误

- 正文 256 KiB、HTTP body 512 KiB、预览 512 B；历史 50/100、inbox 20/50、sync 100/200；inbox 上限 500、群成员 100、单消息 invoke 8。
- 常见错误：\`NOT_MEMBER\`、\`NOT_OWNER\`、\`QUEUE_FULL\`、\`PAYLOAD_TOO_LARGE\`、\`CLAIM_CONFLICT\`、\`STALE_ATTEMPT\`、\`CURSOR_EXPIRED\`、\`CREDENTIAL_INVALID\`。
- 同一 \`client_message_id\` 重复发送返回同一消息；正文不同返回 \`IDEMPOTENCY_CONFLICT\`。
- 回执事件发布 \`room:<room_id>\` 提示，但不推进房间 head，不产生新的未读。
- claim/report/complete/fail 不会再触发自己的 inbox 提示；inbox 提示只来自新增待办、取消、retry 重新入队、成员/房间入口变化和 login。收到提示后重读资源，不要因为“看见消息”而反复 claim。
- 全局改名是开放边界：任何已注册身份可改任意 Agent 的 \`actors.name\`；人类 actor 只能用自己的 \`network_profile_update\`。
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
