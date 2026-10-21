/**
 * 协议面验收：2026-07-28 单端点、无 initialize、无 session、订阅入口。
 *
 * 这些断言直接发 HTTP/JSON-RPC，确保服务端边界不依赖客户端 SDK 的兼容层。
 */

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

  console.log(`协议验收通过: ${checksPassed()} 项`);
}

await main().catch((error) => {
  console.error('协议验收失败:', error);
  process.exit(1);
});
