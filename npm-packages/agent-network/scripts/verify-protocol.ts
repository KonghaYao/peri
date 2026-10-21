/**
 * 协议面验收：2026-07-28 单端点、无 initialize、无 session、订阅入口。
 *
 * 这些断言直接发 HTTP/JSON-RPC，确保服务端边界不依赖客户端 SDK 的兼容层。
 */

import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

import {
  baseUrl,
  callTool,
  check,
  checkEqual,
  checksPassed,
  connectClient,
  expectToolError,
  newToken,
  rawRpc,
} from './lib/harness';

interface SkillEntry {
  uri: string;
  frontmatter: { name: string; description: string };
  resources?: { uri: string; digest: string }[];
}

async function main(): Promise<void> {
  console.log(`协议验收: ${baseUrl()}`);

  const health = await fetch(`${baseUrl()}/health`);
  checkEqual('GET /health 返回 200', health.status, 200);

  const discover = await rawRpc('server/discover');
  checkEqual('server/discover HTTP 200', discover.status, 200);
  const discoverResult = discover.body.result as Record<string, unknown> | undefined;
  check('server/discover 返回 result', discoverResult !== undefined, discover.body);
  const versions = discoverResult?.supportedVersions as string[] | undefined;
  check('supportedVersions 仅包含 2026-07-28', Array.isArray(versions) && versions.length === 1 && versions[0] === '2026-07-28', versions);
  checkEqual('discover resultType 为 complete', discoverResult?.resultType, 'complete');
  checkEqual('discover ttlMs 为 0', discoverResult?.ttlMs, 0);
  checkEqual('discover cacheScope 为 private', discoverResult?.cacheScope, 'private');
  const discoverCapabilities = discoverResult?.capabilities as
    | { extensions?: Record<string, unknown> }
    | undefined;
  check(
    'server/discover 声明 Skills 扩展',
    discoverCapabilities?.extensions?.['io.modelcontextprotocol/skills'] !== undefined,
    discoverCapabilities?.extensions,
  );

  const inconsistent = await rawRpc('initialize', {
    protocolVersion: '2025-06-18',
    capabilities: {},
    clientInfo: { name: 'legacy-check', version: '0.1.0' },
  }, { protocolVersion: null });
  checkEqual('请求头与 body 版本不一致返回 400', inconsistent.status, 400);
  const inconsistentError = inconsistent.body.error as Record<string, unknown> | undefined;
  checkEqual('版本不一致错误码为 -32020', inconsistentError?.code, -32020);

  const legacy = await rawRpc('initialize', {
    protocolVersion: '2025-06-18',
    capabilities: {},
    clientInfo: { name: 'legacy-check', version: '0.1.0' },
  }, { protocolVersion: null, versionHeader: '2025-06-18' });
  checkEqual('legacy initialize 返回 400', legacy.status, 400);
  const legacyError = legacy.body.error as Record<string, unknown> | undefined;
  checkEqual('legacy initialize 错误码为 -32022', legacyError?.code, -32022);

  const oldHeader = await rawRpc('tools/list', {}, { versionHeader: '2025-06-18' });
  checkEqual('旧 MCP-Protocol-Version 头返回 400', oldHeader.status, 400);
  const oldHeaderError = oldHeader.body.error as Record<string, unknown> | undefined;
  checkEqual('旧协议头错误码为 -32020', oldHeaderError?.code, -32020);

  const list = await rawRpc('tools/list');
  const listed = list.body.result as { tools?: { name: string }[] } | undefined;
  const toolNames = listed?.tools?.map((tool) => tool.name) ?? [];
  check('tools/list 返回 25 个工具', toolNames.length === 25, toolNames.length);
  for (const required of [
    'network_register',
    'network_me',
    'network_message_send',
    'network_inbox_list',
    'network_delivery_claim',
    'network_delivery_complete',
    'network_delivery_report',
    'network_delivery_fail',
    'network_delivery_cancel',
    'network_delivery_retry',
    'network_sync',
  ]) {
    check(`工具 ${required} 已注册`, toolNames.includes(required), toolNames);
  }

  const skills = await rawRpc('skills/list');
  const skillEntries =
    (skills.body.result as { skills?: SkillEntry[] } | undefined)?.skills ?? [];
  checkEqual('skills/list 返回 1 个技能', skillEntries.length, 1);
  const skill = skillEntries[0];
  checkEqual('技能 frontmatter.name 为 agent-network', skill?.frontmatter.name, 'agent-network');
  check(
    '技能 frontmatter.description 为非空字符串',
    typeof skill?.frontmatter.description === 'string' && skill.frontmatter.description.length > 0,
    skill?.frontmatter,
  );
  checkEqual('技能 URI 最终段与 name 一致', skill?.uri, 'skill://agent-network/agent-network/SKILL.md');
  checkEqual(
    'skills/list 结果不含 _meta（rmcp 兼容）',
    (skills.body.result as Record<string, unknown> | undefined)?._meta,
    undefined,
  );
  checkEqual(
    'skills/list 结果保留 resultType',
    (skills.body.result as Record<string, unknown> | undefined)?.resultType,
    'complete',
  );
  const skillResource = skill?.resources?.find((resource) => resource.uri === skill?.uri);
  check('技能 resources 含 SKILL.md 自身条目', skillResource !== undefined, skill?.resources);
  check(
    '技能 digest 格式为 sha256:<64 位小写 hex>',
    typeof skillResource?.digest === 'string' && /^sha256:[0-9a-f]{64}$/u.test(skillResource.digest),
    skillResource?.digest,
  );

  const skillRead = await rawRpc(
    'resources/read',
    { uri: skill?.uri },
    { headers: { 'mcp-name': skill?.uri ?? '' } },
  );
  const readContents =
    (skillRead.body.result as
      | { contents?: { uri: string; mimeType?: string; text?: string }[] }
      | undefined)?.contents ?? [];
  const skillText = readContents[0]?.text ?? '';
  checkEqual('resources/read 返回技能正文', readContents[0]?.uri, skill?.uri);
  checkEqual('技能正文 mimeType 为 text/markdown', readContents[0]?.mimeType, 'text/markdown');
  const actualDigest = new Bun.CryptoHasher('sha256').update(skillText).digest('hex');
  checkEqual('技能正文 digest 与条目一致', `sha256:${actualDigest}`, skillResource?.digest);
  check(
    '技能正文含与条目一致的 frontmatter',
    skillText.startsWith(`---\nname: ${skill?.frontmatter.name}\ndescription: ${skill?.frontmatter.description}\n---\n`),
    skillText.slice(0, 160),
  );

  const skillGet = await rawRpc('skills/get', { uri: skill?.uri ?? '' });
  const gotSkill = (skillGet.body.result as { skill?: SkillEntry } | undefined)?.skill;
  checkEqual(
    'skills/get 结果不含 _meta（rmcp 兼容）',
    (skillGet.body.result as Record<string, unknown> | undefined)?._meta,
    undefined,
  );
  check(
    'skills/get 返回同一技能快照',
    JSON.stringify(gotSkill) === JSON.stringify(skill),
    { gotSkill, skill },
  );
  const missingSkill = await rawRpc('skills/get', { uri: 'skill://unknown/unknown/SKILL.md' });
  checkEqual(
    'skills/get 未知 URI 返回 -32602',
    (missingSkill.body.error as Record<string, unknown> | undefined)?.code,
    -32602,
  );

  const shareSession = `sess_share_${crypto.randomUUID().replaceAll('-', '')}`;
  const shareToken = newToken();
  const shareRegister = await rawRpc(
    'tools/call',
    {
      name: 'network_register',
      arguments: {
        request_id: `reg_${crypto.randomUUID().replaceAll('-', '')}`,
        kind: 'agent',
        name: '分享页检查',
        agent_session_id: shareSession,
      },
    },
    { token: shareToken },
  );
  const shareIdentity = (
    shareRegister.body.result as
      | { structuredContent?: { actor_id?: string; share_url?: string | null } }
      | undefined
  )?.structuredContent;
  check(
    '注册返回指向 /a/<session> 的 share_url',
    typeof shareIdentity?.share_url === 'string' &&
      shareIdentity.share_url.endsWith(`/a/${encodeURIComponent(shareSession)}`),
    shareIdentity?.share_url,
  );
  const sharePath = shareIdentity?.share_url
    ? new URL(shareIdentity.share_url).pathname
    : '/a/missing';
  const sharePage = await fetch(`${baseUrl()}${sharePath}`);
  checkEqual('分享页 GET 返回 200', sharePage.status, 200);
  check(
    '分享页 Content-Type 为 text/html',
    (sharePage.headers.get('content-type') ?? '').includes('text/html'),
    sharePage.headers.get('content-type'),
  );
  const shareHtml = await sharePage.text();
  check('分享页包含 actor_id', shareHtml.includes(shareIdentity?.actor_id ?? '\u0000'));
  check('分享页包含 agent_session_id', shareHtml.includes(shareSession));

  const missingShare = await fetch(
    `${baseUrl()}/a/${encodeURIComponent(`missing_${crypto.randomUUID().replaceAll('-', '')}`)}`,
  );
  checkEqual('未知分享 session 返回 404', missingShare.status, 404);
  check(
    '未知分享 session 返回 HTML',
    (missingShare.headers.get('content-type') ?? '').includes('text/html'),
    missingShare.headers.get('content-type'),
  );

  const home = await fetch(`${baseUrl()}/`);
  checkEqual('GET / 返回 200', home.status, 200);
  check(
    '首页 Content-Type 为 text/html',
    (home.headers.get('content-type') ?? '').includes('text/html'),
    home.headers.get('content-type'),
  );

  const browserToken = newToken();
  const browserRegister = await rawRpc(
    'tools/call',
    {
      name: 'network_register',
      arguments: {
        request_id: `web_reg_${crypto.randomUUID().replaceAll('-', '')}`,
        kind: 'human',
        name: '网页访客检查',
      },
    },
    { token: browserToken },
  );
  const browserIdentity = (
    browserRegister.body.result as
      | { structuredContent?: { actor_id?: string; share_url?: string | null } }
      | undefined
  )?.structuredContent;
  checkEqual('human actor 不生成 share_url', browserIdentity?.share_url, null);
  const browserContact = await rawRpc(
    'tools/call',
    {
      name: 'network_contact_add',
      arguments: {
        request_id: `web_contact_${crypto.randomUUID().replaceAll('-', '')}`,
        agent_session_id: shareSession,
      },
    },
    { token: browserToken },
  );
  const contactResult = (
    browserContact.body.result as
      | { structuredContent?: { contact?: { actor_id?: string } } }
      | undefined
  )?.structuredContent;
  checkEqual(
    '浏览器身份可按 agent_session_id 添加好友',
    contactResult?.contact?.actor_id,
    shareIdentity?.actor_id,
  );
  const browserRoom = await rawRpc(
    'tools/call',
    {
      name: 'network_room_create',
      arguments: {
        request_id: `web_room_${crypto.randomUUID().replaceAll('-', '')}`,
        kind: 'direct',
        members: [shareIdentity?.actor_id],
      },
    },
    { token: browserToken },
  );
  const roomResult = (
    browserRoom.body.result as { structuredContent?: { room?: { kind?: string } } } | undefined
  )?.structuredContent;
  checkEqual('浏览器身份可建立单聊', roomResult?.room?.kind, 'direct');

  const getResponse = await fetch(`${baseUrl()}/mcp`);
  checkEqual('GET /mcp 返回 405', getResponse.status, 405);
  check('GET /mcp 声明 Allow: POST', (getResponse.headers.get('allow') ?? '').includes('POST'), getResponse.headers.get('allow'));

  const deleteResponse = await fetch(`${baseUrl()}/mcp`, { method: 'DELETE' });
  checkEqual('DELETE /mcp 返回 405', deleteResponse.status, 405);

  const unregistered = newToken();
  const connection = await connectClient(unregistered);
  try {
    const tools = await connection.client.listTools();
    checkEqual('官方 SDK 可通过 server/discover 建连', tools.tools.length, 25);
    await expectToolError('未注册凭据调用 network_me 返回 CREDENTIAL_INVALID', callTool(connection.client, 'network_me'), 'CREDENTIAL_INVALID');

    const listen = await rawRpc(
      'subscriptions/listen',
      { notifications: { resourceSubscriptions: ['agentnet://me'] } },
      { token: unregistered },
    );
    const listenError = listen.body.error as Record<string, unknown> | undefined;
    checkEqual('未注册身份建立订阅被拒绝', listenError?.code, -32602);
    const listenData = listenError?.data as Record<string, unknown> | undefined;
    checkEqual('订阅拒绝返回 CREDENTIAL_INVALID', listenData?.code, 'CREDENTIAL_INVALID');

    const roomListen = await rawRpc(
      'subscriptions/listen',
      { notifications: { resourceSubscriptions: [`agentnet://actors/${unregistered}/inbox`] } },
      { token: unregistered },
    );
    const roomListenError = roomListen.body.error as Record<string, unknown> | undefined;
    checkEqual('未注册身份订阅 inbox 被拒绝', roomListenError?.code, -32602);
  } finally {
    await connection.close();
  }

  const anonymous = new Client(
    { name: 'protocol-anonymous', version: '0.0.0' },
    { versionNegotiation: { mode: { pin: '2026-07-28' } } },
  );
  await anonymous.connect(new StreamableHTTPClientTransport(new URL(`${baseUrl()}/mcp`)));
  try {
    await expectToolError(
      '无 Authorization 调用 network_me 返回 CREDENTIAL_INVALID',
      callTool(anonymous, 'network_me'),
      'CREDENTIAL_INVALID',
    );
  } finally {
    await anonymous.close();
  }

  console.log(`协议验收通过: ${checksPassed()} 项`);
}

await main().catch((error) => {
  console.error('协议验收失败:', error);
  process.exit(1);
});
