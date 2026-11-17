import { describe, expect, test } from 'bun:test';
import { AgentConfig, AgentDraft } from '@shared/agent';

const provider = { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' } as const;

describe('AgentDraft', () => {
  test('applies defaults and trims text', () => {
    const draft = AgentDraft.parse({ name: '  Reviewer  ', config: {} });

    expect(draft.name).toBe('Reviewer');
    expect(draft.description).toBe('');
    expect(draft.config).toEqual({
      instructions: '',
      provider: null,
      metaHarness: {},
      mcpServers: {},
    });
  });

  test('accepts provider and http/stdio MCP servers', () => {
    const draft = AgentDraft.parse({
      name: 'Reviewer',
      description: 'Checks changes',
      config: {
        instructions: 'Be concise',
        provider,
        metaHarness: { '01_intro': true, ToolsMiddleware: false },
        mcpServers: {
          docs: {
            type: 'http',
            url: 'https://example.com/mcp',
            headers: { Authorization: 'Bearer token' },
          },
          local: {
            type: 'stdio',
            command: 'bunx',
            args: ['mcp-server'],
            env: { TOKEN: 'secret' },
          },
        },
      },
    });

    expect(draft.config.provider).toEqual(provider);
    expect(draft.config.mcpServers.docs).toEqual({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: { Authorization: 'Bearer token' },
    });
    expect(draft.config.mcpServers.local).toEqual({
      type: 'stdio',
      command: 'bunx',
      args: ['mcp-server'],
      env: { TOKEN: 'secret' },
    });
  });

  test('rejects unknown top-level keys', () => {
    const result = AgentDraft.safeParse({ name: 'Reviewer', config: {}, allowedTools: ['read'] });
    expect(result.success).toBe(false);
  });

  test('rejects unknown config keys', () => {
    const result = AgentConfig.safeParse({ instructions: '', allowedTools: ['read'] });
    expect(result.success).toBe(false);
  });

  test('rejects ACP MCP transports', () => {
    const result = AgentDraft.safeParse({
      name: 'Reviewer',
      config: { mcpServers: { live: { type: 'acp' } } },
    });
    expect(result.success).toBe(false);
  });

  test('rejects invalid MCP server names', () => {
    for (const name of ['with space', 'slash/name', '', 'a'.repeat(65)]) {
      const result = AgentConfig.safeParse({
        mcpServers: { [name]: { type: 'stdio', command: 'node' } },
      });
      expect(result.success).toBe(false);
    }
  });

  test('accepts 64-character MCP server names', () => {
    const name = 'a'.repeat(64);
    const result = AgentConfig.safeParse({ mcpServers: { [name]: { type: 'stdio', command: 'node' } } });
    expect(result.success).toBe(true);
  });

  test('validates MCP URLs and commands', () => {
    expect(AgentConfig.safeParse({ mcpServers: { docs: { type: 'http', url: 'not-a-url' } } }).success).toBe(
      false,
    );
    expect(AgentConfig.safeParse({ mcpServers: { local: { type: 'stdio', command: '' } } }).success).toBe(
      false,
    );
  });

  test('rejects MCP URLs that are not http(s)', () => {
    for (const url of ['file:///etc/passwd', 'ftp://example.com/mcp']) {
      const result = AgentConfig.safeParse({ mcpServers: { docs: { type: 'http', url } } });
      expect(result.success).toBe(false);
      if (!result.success) {
        expect(result.error.issues[0]?.message).toContain('http or https');
      }
    }
  });

  test('accepts http and https MCP URLs', () => {
    for (const url of ['https://example.com/mcp', 'http://127.0.0.1:3000/mcp']) {
      expect(AgentConfig.safeParse({ mcpServers: { docs: { type: 'http', url } } }).success).toBe(true);
    }
  });

  test('rejects blank stdio commands and trims accepted ones', () => {
    for (const command of ['', '   ', '\n\t']) {
      expect(AgentConfig.safeParse({ mcpServers: { local: { type: 'stdio', command } } }).success).toBe(
        false,
      );
    }

    const parsed = AgentConfig.parse({ mcpServers: { local: { type: 'stdio', command: '  node  ' } } });
    expect(parsed.mcpServers.local).toEqual({ type: 'stdio', command: 'node' });
  });

  test('validates provider fields', () => {
    expect(AgentConfig.safeParse({ provider: { ...provider, type: 'google' } }).success).toBe(false);
    expect(AgentConfig.safeParse({ provider: { ...provider, id: '' } }).success).toBe(false);
    expect(AgentConfig.safeParse({ provider: { ...provider, model: '' } }).success).toBe(false);
    expect(AgentConfig.safeParse({ provider: { ...provider, baseUrl: 'not-a-url' } }).success).toBe(false);
  });

  test('enforces name boundaries', () => {
    expect(AgentDraft.safeParse({ name: '   ', config: {} }).success).toBe(false);
    expect(AgentDraft.safeParse({ name: 'a'.repeat(81), config: {} }).success).toBe(false);
    expect(AgentDraft.safeParse({ name: 'a'.repeat(80), config: {} }).success).toBe(true);
  });

  test('enforces description and instructions limits', () => {
    expect(AgentDraft.safeParse({ name: 'A', description: 'a'.repeat(401), config: {} }).success).toBe(false);
    expect(AgentConfig.safeParse({ instructions: 'a'.repeat(32_769) }).success).toBe(false);
    expect(AgentConfig.safeParse({ instructions: 'a'.repeat(32_768) }).success).toBe(true);
  });

  test('rejects non-boolean metaHarness values', () => {
    const result = AgentConfig.safeParse({ metaHarness: { '01_intro': 'yes' } });
    expect(result.success).toBe(false);
  });
});
