import { describe, expect, test } from 'bun:test';
import type { AgentDraft } from '@shared/agent';
import { PREVIEW_CHAT_ID, SANDBOX_MCP_SERVER_NAME } from '@shared/agent';
import {
  assertSandboxTargetCompatible,
  buildAgentTarget,
  buildPreviewWarnings,
  REDACTED_VALUE,
  redactTarget,
} from '../../../server/agents/open-target';
import { testEnv } from '../helpers';

const env = testEnv({
  storeToken: 'store-super-secret-value',
});

const draft: AgentDraft = {
  name: 'Reviewer',
  description: '',
  config: {
    instructions: 'Be concise',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: { '01_intro': true },
    mcpServers: {
      docs: {
        type: 'http',
        url: 'https://example.com/mcp',
        headers: { Authorization: 'Bearer mcp-header-secret' },
      },
      local: { type: 'stdio', command: 'npx', env: { TOKEN: 'stdio-env-secret' } },
    },
  },
};

describe('buildAgentTarget', () => {
  test('composes PeriConfig under agent.settings.config', () => {
    const target = buildAgentTarget(env, draft, 'chat-1');
    const config = target.agent.settings.config;

    expect(target.chatId).toBe('chat-1');
    expect(target.workspace).toBe(env.workspaceUri);
    expect(config.active_alias).toBe('sonnet');
    expect(config.providers).toEqual([{ id: 'anthropic-main', type: 'anthropic' }]);
    expect(config.profiles).toEqual({
      sonnet: { provider: 'anthropic-main', model: 'claude-sonnet-4-5' },
    });
    expect(config.meta_harness).toEqual({ '01_intro': true });
    expect(target.storage).toEqual({ url: env.storeUrl, authToken: 'store-super-secret-value' });
    expect(target.session.instructions).toBe('Be concise');
    expect(target.session.mcpServers).toEqual(draft.config.mcpServers);
  });

  test('never puts Manager-scoped keys inside agent', () => {
    const target = buildAgentTarget(env, draft, PREVIEW_CHAT_ID);
    const keys = Object.keys(target.agent);

    expect(keys).toEqual(['settings']);
    expect(target.agent).not.toHaveProperty('cwd');
    expect(target.agent).not.toHaveProperty('storage');
    expect(target.agent).not.toHaveProperty('machineId');
  });

  test('omits meta harness, instructions and MCP when empty', () => {
    const target = buildAgentTarget(
      testEnv(),
      {
        name: 'Empty',
        description: '',
        config: { instructions: '', provider: null, metaHarness: {}, mcpServers: {} },
      },
      PREVIEW_CHAT_ID,
    );

    expect(target.agent.settings.config.meta_harness).toBeUndefined();
    expect(target.agent.settings.config.providers).toEqual([]);
    expect(target.agent.settings.config.profiles).toEqual({});
    expect(target.session.instructions).toBeUndefined();
    expect(target.session.mcpServers).toBeUndefined();
    expect(target.storage.authToken).toBeUndefined();
  });
});

describe('redactTarget', () => {
  test('masks every credential value but keeps keys and non-secret data', () => {
    const original = buildAgentTarget(env, draft, PREVIEW_CHAT_ID);
    const key = crypto.randomUUID();
    const provider = original.agent.settings.config.providers[0];
    if (!provider) throw new Error('provider missing');
    provider.apiKey = key;
    const redacted = redactTarget(original);

    expect(redacted.agent.settings.config.providers[0]?.apiKey).toBe(REDACTED_VALUE);
    expect(redacted.storage.authToken).toBe(REDACTED_VALUE);
    expect(redacted.session.mcpServers?.docs).toEqual({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: { Authorization: REDACTED_VALUE },
    });
    expect(redacted.session.mcpServers?.local).toEqual({
      type: 'stdio',
      command: 'npx',
      env: { TOKEN: REDACTED_VALUE },
    });
    expect(redacted.session.instructions).toBe('Be concise');
    expect(JSON.stringify(redacted)).not.toContain('super-secret-value');

    // The source target is not mutated; open-target will use it unmasked for real chats.
    expect(original.agent.settings.config.providers[0]?.apiKey === key).toBe(true);
    expect(original.storage.authToken).toBe('store-super-secret-value');
  });
});

describe('buildPreviewWarnings', () => {
  test('reports missing keys and runtime warnings', () => {
    const warnings = buildPreviewWarnings({
      ...draft,
      config: {
        ...draft.config,
        provider: { id: 'oai', type: 'openai', model: 'gpt-5' },
      },
    });

    expect(warnings.map((warning) => warning.code)).toEqual([
      'provider_missing',
      'mcp_not_connected',
      'history_not_persisted',
    ]);
    expect(warnings[0]?.message).toContain('共享 Provider');
    expect(warnings[0]?.message).not.toContain('sk-');
  });
});

describe('sandbox target injection', () => {
  const injection = {
    serverName: SANDBOX_MCP_SERVER_NAME as typeof SANDBOX_MCP_SERVER_NAME,
    url: 'http://127.0.0.1:8961/gateway/actor-1/request/mcp',
    headers: {
      'x-rivet-token': 'gateway-value',
      'x-peri-sandbox-token': 'actor-secret-value',
    },
  };

  test('adds the reserved HTTP entry without dropping user MCP servers', () => {
    const target = buildAgentTarget(env, draft, 'chat-1', { sandbox: injection });
    const servers = target.session.mcpServers ?? {};
    expect(target.workspace).toBe(env.workspaceUri);
    expect(servers.docs).toEqual({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: { Authorization: 'Bearer mcp-header-secret' },
    });
    expect(servers[SANDBOX_MCP_SERVER_NAME]).toEqual({
      type: 'http',
      url: injection.url,
      headers: injection.headers,
    });
    expect(target.agent.settings.config.meta_harness).toEqual({
      '01_intro': true,
      McpMiddleware: true,
    });
  });

  test('redacts both sandbox credentials in a target copied for preview', () => {
    const target = buildAgentTarget(env, draft, PREVIEW_CHAT_ID, { sandbox: injection });
    const redacted = redactTarget(target);
    const server = redacted.session.mcpServers?.[SANDBOX_MCP_SERVER_NAME];
    expect(server).toEqual({
      type: 'http',
      url: injection.url,
      headers: {
        'x-rivet-token': REDACTED_VALUE,
        'x-peri-sandbox-token': REDACTED_VALUE,
      },
    });
    expect(JSON.stringify(redacted)).not.toContain('actor-secret-value');
  });

  test('an explicit McpMiddleware:false is rejected instead of silently enabled', () => {
    const explicitOff: AgentDraft = {
      ...draft,
      config: { ...draft.config, metaHarness: { McpMiddleware: false } },
    };
    expect(() => buildAgentTarget(env, explicitOff, 'chat-1', { sandbox: injection })).toThrow(
      expect.objectContaining({ code: 'not_ready', details: { reason: 'sandbox_mcp_disabled' } }),
    );
  });

  test('a user server named like the reserved entry is rejected', () => {
    const conflicting: AgentDraft = {
      ...draft,
      config: {
        ...draft.config,
        mcpServers: {
          ...draft.config.mcpServers,
          [SANDBOX_MCP_SERVER_NAME]: { type: 'stdio', command: 'node' },
        },
      },
    };
    expect(() => assertSandboxTargetCompatible(conflicting)).toThrow(
      expect.objectContaining({ code: 'invalid_input' }),
    );
    expect(() => buildAgentTarget(env, conflicting, 'chat-1', { sandbox: injection })).toThrow(
      expect.objectContaining({ code: 'invalid_input' }),
    );
  });

  test('preview reports a misconfigured sandbox and an explicit MCP opt-out', () => {
    const misconfigured = buildPreviewWarnings(
      draft,
      { mode: 'wasm', reasons: [] },
      {
        mode: 'misconfigured',
        reasons: ['sandbox_peri_missing'],
      },
    );
    expect(misconfigured.map((warning) => warning.code)).toContain('sandbox_unavailable');

    const explicitOff = buildPreviewWarnings(
      {
        ...draft,
        config: { ...draft.config, metaHarness: { McpMiddleware: false } },
      },
      { mode: 'wasm', reasons: [] },
      { mode: 'ready', reasons: [] },
    );
    expect(explicitOff.map((warning) => warning.code)).toContain('sandbox_mcp_disabled');
  });
});
