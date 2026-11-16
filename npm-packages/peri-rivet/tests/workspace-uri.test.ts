import { describe, expect, test } from 'bun:test';
import { isPeriError, type PeriError } from '../src/contracts/index.js';
import {
  canonicalizeWorkspaceUri,
  formatWorkspaceUri,
  MAX_WORKSPACE_URI_LENGTH,
  parseMachineId,
  parseWorkspaceUri,
  WorkspaceURI,
  type WorkspaceUri,
} from '../src/rivet/index.js';

const MACHINE = '00000000-0000-4000-8000-000000000001';
const LETTERED_MACHINE = '00000000-0000-4000-8000-00000000000a';
const PREFIX = `peri+sandbox://${MACHINE}`;

function uri(path: string): WorkspaceUri {
  return `${PREFIX}${path}` as WorkspaceUri;
}

function letteredUri(path: string): WorkspaceUri {
  return `peri+sandbox://${LETTERED_MACHINE}${path}` as WorkspaceUri;
}

function expectReason(run: () => unknown, reason: string, field = 'workspaceUri'): PeriError {
  try {
    run();
  } catch (error) {
    expect(isPeriError(error)).toBe(true);
    const peri = error as PeriError;
    expect(peri.code).toBe('invalid_identity');
    expect(peri.phase).toBe('input');
    expect(peri.details?.reason).toBe(reason);
    expect(peri.details?.field).toBe(field);
    return peri;
  }
  throw new Error(`expected ${reason}`);
}

describe('parseWorkspaceUri', () => {
  test('round-trips POSIX, Windows, and root forms', () => {
    const nested = uri('/srv/peri/workspace');
    expect(parseWorkspaceUri(nested)).toEqual({
      uri: nested,
      machineId: MACHINE,
      runtimePath: '/srv/peri/workspace',
    });
    expect(formatWorkspaceUri(parseWorkspaceUri(nested))).toBe(nested);

    const root = uri('/');
    expect(parseWorkspaceUri(root)).toEqual({ uri: root, machineId: MACHINE, runtimePath: '/' });
    expect(formatWorkspaceUri(parseWorkspaceUri(root))).toBe(root);

    const windows = uri('/c:/Users/peri/workspace');
    expect(parseWorkspaceUri(windows).runtimePath).toBe('c:/Users/peri/workspace');
    expect(formatWorkspaceUri(parseWorkspaceUri(windows))).toBe(windows);

    const driveRoot = uri('/c:/');
    expect(parseWorkspaceUri(driveRoot).runtimePath).toBe('c:/');
    expect(formatWorkspaceUri(parseWorkspaceUri(driveRoot))).toBe(driveRoot);
  });

  test('round-trips percent-encoded and pchar segments', () => {
    const runtimePath = "/srv/my workspace/中文/!$&'()*+,;=:@/%?-#";
    const formatted = formatWorkspaceUri({ machineId: MACHINE, runtimePath });
    expect(parseWorkspaceUri(formatted).runtimePath).toBe(runtimePath);
    expect(formatted).toContain('%20');
    expect(formatted).toContain('%E4%B8%AD%E6%96%87');
    expect(formatted).toContain('%25');
    expect(formatted).toContain('%3F');
    expect(formatted).toContain('%23');
  });

  test('accepts exactly 2048 characters and rejects 2049', () => {
    const pathLength = MAX_WORKSPACE_URI_LENGTH - PREFIX.length;
    const boundary = `${PREFIX}/${'a'.repeat(pathLength - 1)}`;
    expect(boundary.length).toBe(MAX_WORKSPACE_URI_LENGTH);
    expect(parseWorkspaceUri(boundary).runtimePath).toHaveLength(pathLength);

    expectReason(() => parseWorkspaceUri(`${boundary}b`), 'too_long');
  });

  test('rejects non-canonical and ambiguous input with stable reasons', () => {
    const cases: readonly [string, string][] = [
      ['http://example/srv', 'invalid_scheme'],
      [`PERI+SANDBOX://${MACHINE}/srv`, 'invalid_scheme'],
      ['peri+sandbox:/srv/x', 'missing_authority'],
      ['peri+sandbox:///srv/x', 'missing_machine'],
      ['peri+sandbox://00000000-0000-4000-8000-00000000000/srv', 'invalid_machine'],
      [`peri+sandbox://${LETTERED_MACHINE.toUpperCase()}/srv`, 'invalid_machine'],
      [uri(''), 'empty_path'],
      [uri('/srv//x'), 'empty_segment'],
      [uri('/srv/x/'), 'trailing_separator'],
      [uri('/srv/./x'), 'dot_segment'],
      [uri('/srv/%2E/x'), 'dot_segment'],
      [uri('/srv/../x'), 'path_traversal'],
      [uri('/srv/%2e%2e/x'), 'path_traversal'],
      [uri('/srv/%2F/x'), 'separator_in_segment'],
      [uri('/srv/%5C/x'), 'separator_in_segment'],
      [uri('/srv/a\\b'), 'separator_in_segment'],
      [uri('/srv/%G0'), 'invalid_percent_encoding'],
      [uri('/srv/%'), 'invalid_percent_encoding'],
      [uri('/srv/%FF'), 'invalid_utf8'],
      [uri('/srv/%C0%AF'), 'invalid_utf8'],
      [uri('/srv/%41'), 'non_canonical'],
      [uri('/srv/%e4%b8%ad'), 'non_canonical'],
      [uri('/srv/中文'), 'non_canonical'],
      [uri('/srv/my workspace'), 'non_canonical'],
      [uri('/srv/%00'), 'control_character'],
      [uri('/srv/%0A'), 'control_character'],
      [uri('/srv/x?y=1'), 'query_or_fragment'],
      [uri('/srv/x#frag'), 'query_or_fragment'],
      [`peri+sandbox://user@${MACHINE}/srv`, 'userinfo_or_port'],
      [`peri+sandbox://${MACHINE}:80/srv`, 'userinfo_or_port'],
      [uri('/C:/Users/peri'), 'non_canonical_path'],
      [uri('/c:'), 'non_canonical_path'],
    ];

    for (const [value, reason] of cases) {
      expectReason(() => parseWorkspaceUri(value), reason);
    }
  });

  test('does not echo the rejected input in the error message', () => {
    const error = expectReason(() => parseWorkspaceUri(uri('/srv/%41/secret')), 'non_canonical');
    expect(error.message).not.toContain('secret');
    expect(error.message).not.toContain('%41');
  });
});

describe('formatWorkspaceUri', () => {
  test('normalizes machine case and encodes path segments', () => {
    expect(formatWorkspaceUri({ machineId: LETTERED_MACHINE.toUpperCase(), runtimePath: '/srv/x' })).toBe(
      letteredUri('/srv/x'),
    );
    expect(formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/' })).toBe(uri('/'));
    expect(formatWorkspaceUri({ machineId: MACHINE, runtimePath: 'c:/' })).toBe(uri('/c:/'));
    expect(formatWorkspaceUri({ machineId: MACHINE, runtimePath: 'c:/Users/x' })).toBe(uri('/c:/Users/x'));
  });

  test('rejects non-canonical runtime paths with stable reasons', () => {
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: 'srv/x' }), 'relative_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: './x' }), 'relative_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '' }), 'relative_path');
    expectReason(
      () => formatWorkspaceUri({ machineId: MACHINE, runtimePath: 'c:\\x' }),
      'non_canonical_path',
    );
    expectReason(
      () => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/a/../b' }),
      'non_canonical_path',
    );
    expectReason(
      () => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/a//b' }),
      'non_canonical_path',
    );
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/a/' }), 'non_canonical_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/a/.' }), 'non_canonical_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: 'C:/x' }), 'non_canonical_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/c:/foo' }), 'ambiguous_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/c:' }), 'ambiguous_path');
    expectReason(() => formatWorkspaceUri({ machineId: MACHINE, runtimePath: '/a\nb' }), 'control_character');
    expectReason(
      () => formatWorkspaceUri({ machineId: '00000000-0000-4000-8000-00000000000', runtimePath: '/x' }),
      'invalid_machine',
    );
    expectReason(
      () => formatWorkspaceUri({ machineId: MACHINE, runtimePath: `/${'a'.repeat(2000)}` }),
      'too_long',
    );
  });
});

describe('WorkspaceURI', () => {
  test('builds the documented workspace and round-trips through parse', () => {
    const workspace = new WorkspaceURI({
      machineId: MACHINE,
      runtimePath: '/Users/me/project',
    });
    const text = workspace.toString();

    expect(text).toBe(uri('/Users/me/project'));
    expect(parseWorkspaceUri(text)).toEqual({
      uri: text,
      machineId: MACHINE,
      runtimePath: '/Users/me/project',
    });
  });

  test('captures input at construction and keeps toString stable', () => {
    const parts = { machineId: MACHINE, runtimePath: '/srv/original' };
    const workspace = new WorkspaceURI(parts);
    const text = workspace.toString();

    parts.machineId = LETTERED_MACHINE;
    parts.runtimePath = '/srv/changed';

    expect(workspace.toString()).toBe(text);
    expect(workspace.toString()).toBe(uri('/srv/original'));
  });

  test('normalizes and encodes like formatWorkspaceUri', () => {
    const workspace = new WorkspaceURI({
      machineId: LETTERED_MACHINE.toUpperCase(),
      runtimePath: '/srv/my workspace/中文',
    });

    expect(workspace.toString()).toBe(letteredUri('/srv/my%20workspace/%E4%B8%AD%E6%96%87'));
    expect(workspace.toString()).toBe(
      formatWorkspaceUri({ machineId: LETTERED_MACHINE, runtimePath: '/srv/my workspace/中文' }),
    );
  });

  test('rejects invalid identities with the existing reasons', () => {
    expectReason(
      () =>
        new WorkspaceURI({
          machineId: '00000000-0000-4000-8000-00000000000',
          runtimePath: '/srv/x',
        }),
      'invalid_machine',
    );
    expectReason(() => new WorkspaceURI({ machineId: MACHINE, runtimePath: 'srv/x' }), 'relative_path');
    expectReason(
      () => new WorkspaceURI({ machineId: MACHINE, runtimePath: '/srv/../x' }),
      'non_canonical_path',
    );
    expectReason(
      () => new WorkspaceURI({ machineId: MACHINE, runtimePath: '/srv/a\nb' }),
      'control_character',
    );
  });
});

describe('canonicalizeWorkspaceUri', () => {
  test('fixes scheme, UUID, and percent-encoding case without touching path semantics', () => {
    expect(canonicalizeWorkspaceUri(`PERI+SANDBOX://${LETTERED_MACHINE.toUpperCase()}/srv/x`)).toBe(
      letteredUri('/srv/x'),
    );
    expect(canonicalizeWorkspaceUri(uri('/srv/%e4%b8%ad'))).toBe(uri('/srv/%E4%B8%AD'));
    expect(canonicalizeWorkspaceUri(uri('/srv/%41'))).toBe(uri('/srv/A'));
    expect(canonicalizeWorkspaceUri(uri('/srv/%3f'))).toBe(uri('/srv/%3F'));
    expect(canonicalizeWorkspaceUri(uri('/srv/中文'))).toBe(uri('/srv/%E4%B8%AD%E6%96%87'));
    expect(canonicalizeWorkspaceUri(uri('/srv/my workspace'))).toBe(uri('/srv/my%20workspace'));
  });

  test('is idempotent and parseable', () => {
    const once = canonicalizeWorkspaceUri(`PERI+SANDBOX://${LETTERED_MACHINE.toUpperCase()}/srv/%41%3f`);
    expect(canonicalizeWorkspaceUri(once)).toBe(once);
    expect(parseWorkspaceUri(once).runtimePath).toBe('/srv/A?');
  });

  test('still rejects structural and security problems', () => {
    expectReason(() => canonicalizeWorkspaceUri(uri('/srv/../x')), 'path_traversal');
    expectReason(() => canonicalizeWorkspaceUri(uri('/srv/./x')), 'dot_segment');
    expectReason(() => canonicalizeWorkspaceUri(uri('/srv/x/')), 'trailing_separator');
    expectReason(() => canonicalizeWorkspaceUri(uri('/srv/%2F/x')), 'separator_in_segment');
    expectReason(() => canonicalizeWorkspaceUri(uri('/srv/x?y')), 'query_or_fragment');
    expectReason(() => canonicalizeWorkspaceUri(uri('/C:/x')), 'non_canonical_path');
  });
});

describe('parseMachineId', () => {
  test('accepts only canonical lowercase UUIDs', () => {
    expect(parseMachineId(MACHINE)).toBe(MACHINE);
    expectReason(() => parseMachineId(LETTERED_MACHINE.toUpperCase()), 'invalid_machine', 'machineId');
    expectReason(() => parseMachineId('00000000-0000-4000-8000-00000000000'), 'invalid_machine', 'machineId');
    expectReason(() => parseMachineId(`{${MACHINE}}`), 'invalid_machine', 'machineId');
  });
});
