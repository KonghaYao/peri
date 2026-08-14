import { describe, expect, it } from 'vitest';
import { parseAuthSetup } from './auth-setup';

describe('parseAuthSetup', () => {
  it('accepts the two credential-free setup fields and ignores additive data', () => {
    expect(parseAuthSetup({
      authenticated: false,
      setup: {
        tokenFile: '/custom/acp hub/tokens.toml',
        generateCommand: "ACP_HUB_CONFIG_DIR='/custom/acp hub' acp-hub-server token generate --name web --role full",
        future: true,
      },
    })).toEqual({
      tokenFile: '/custom/acp hub/tokens.toml',
      generateCommand: "ACP_HUB_CONFIG_DIR='/custom/acp hub' acp-hub-server token generate --name web --role full",
    });
  });

  it.each([
    null,
    [],
    {},
    { setup: null },
    { setup: [] },
    { setup: { tokenFile: '', generateCommand: 'command' } },
    { setup: { tokenFile: '/path', generateCommand: 42 } },
  ])('rejects malformed setup without guessing a path: %j', (value) => {
    expect(parseAuthSetup(value)).toBeNull();
  });
});
