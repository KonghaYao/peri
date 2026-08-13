import { describe, expect, it } from 'vitest';
import * as Y from 'yjs';
import { renderChat, renderControl } from './yjs';

describe('renderChat tool projection', () => {
  it('preserves server-projected arguments, result and public error', () => {
    const doc = new Y.Doc();
    const root = doc.getMap<unknown>('root');
    const order = new Y.Array<string>();
    const entries = new Y.Map<unknown>();
    const calls = new Y.Map<unknown>();
    root.set('entry_order', order); root.set('entries', entries); root.set('tool_calls', calls);

    const entry = new Y.Map<unknown>();
    const blockOrder = new Y.Array<string>();
    const blocks = new Y.Map<unknown>();
    entries.set('turn:assistant', entry); order.push(['turn:assistant']);
    entry.set('turn_id', 'turn'); entry.set('kind', 'message'); entry.set('role', 'assistant'); entry.set('status', 'completed');
    entry.set('created_at', '2026-08-13T00:00:00Z'); entry.set('block_order', blockOrder); entry.set('blocks', blocks);

    const block = new Y.Map<unknown>();
    blocks.set('tool-block', block); blockOrder.push(['tool-block']);
    block.set('kind', 'tool_call'); block.set('tool_call_id', 'tc-1');

    const call = new Y.Map<unknown>();
    const args = new Y.Map<unknown>();
    const result = new Y.Map<unknown>();
    const error = new Y.Map<unknown>();
    calls.set('tc-1', call);
    call.set('name', 'shell'); call.set('status', 'error'); call.set('arguments', args); call.set('result', result); call.set('public_error', error);
    call.set('started_at', '2026-08-13T00:00:00.000Z'); call.set('completed_at', '2026-08-13T00:00:01.250Z'); call.set('result_omitted', false); call.set('result_bytes', 14);
    args.set('command', 'pwd'); result.set('exitCode', 1); error.set('code', 'FAILED'); error.set('message', 'safe public message');

    const [tool] = renderChat(doc).entries[0].toolCalls;
    expect(tool.arguments).toEqual({ command: 'pwd' });
    expect(tool.result).toEqual({ exitCode: 1 });
    expect(tool.publicError).toEqual({ code: 'FAILED', message: 'safe public message' });
    expect(tool.startedAt).toBe('2026-08-13T00:00:00.000Z');
    expect(tool.completedAt).toBe('2026-08-13T00:00:01.250Z');
    expect(tool.resultOmitted).toBe(false);
    expect(tool.resultBytes).toBe(14);
  });

  it('keeps timestamps optional for legacy snapshots', () => {
    const doc = new Y.Doc();
    const root = doc.getMap<unknown>('root');
    const order = new Y.Array<string>(); const entries = new Y.Map<unknown>(); const calls = new Y.Map<unknown>();
    root.set('entry_order', order); root.set('entries', entries); root.set('tool_calls', calls);
    const entry = new Y.Map<unknown>(); const blockOrder = new Y.Array<string>(); const blocks = new Y.Map<unknown>();
    entries.set('e', entry); order.push(['e']); entry.set('created_at', 'old'); entry.set('block_order', blockOrder); entry.set('blocks', blocks);
    const block = new Y.Map<unknown>(); blocks.set('b', block); blockOrder.push(['b']); block.set('kind', 'tool_call'); block.set('tool_call_id', 'tc');
    calls.set('tc', new Y.Map<unknown>());
    const [tool] = renderChat(doc).entries[0].toolCalls;
    expect(tool.startedAt).toBeNull(); expect(tool.completedAt).toBeNull();
    expect(tool.resultOmitted).toBeNull(); expect(tool.resultBytes).toBeNull();
  });

  it('repairs only exact-turn legacy orphan tools in stable order without duplicating blocks', () => {
    const doc = new Y.Doc();
    const root = doc.getMap<unknown>('root');
    const order = new Y.Array<string>(); const entries = new Y.Map<unknown>(); const calls = new Y.Map<unknown>();
    root.set('entry_order', order); root.set('entries', entries); root.set('tool_calls', calls);
    const entry = new Y.Map<unknown>(); const blockOrder = new Y.Array<string>(); const blocks = new Y.Map<unknown>();
    entries.set('t1:assistant', entry); order.push(['t1:assistant']);
    entry.set('turn_id', 't1'); entry.set('role', 'assistant'); entry.set('created_at', 'now'); entry.set('block_order', blockOrder); entry.set('blocks', blocks);
    const referencedBlock = new Y.Map<unknown>(); blocks.set('tool:linked', referencedBlock); blockOrder.push(['tool:linked']); referencedBlock.set('kind', 'tool_call'); referencedBlock.set('tool_call_id', 'linked');
    for (const [id, turnId, startedAt] of [
      ['late', 't1', '2026-08-13T00:00:02Z'],
      ['early', 't1', '2026-08-13T00:00:01Z'],
      ['linked', 't1', '2026-08-13T00:00:00Z'],
      ['ambiguous', '', '2026-08-13T00:00:00Z'],
      ['other-turn', 't2', '2026-08-13T00:00:00Z'],
    ]) {
      const tool = new Y.Map<unknown>(); tool.set('turn_id', turnId); tool.set('name', id); tool.set('started_at', startedAt); calls.set(id, tool);
    }

    expect(renderChat(doc).entries[0].toolCalls.map((tool) => tool.toolCallId)).toEqual(['linked', 'early', 'late']);
  });
});

describe('renderControl permission projection', () => {
  it('exposes only actionable pending records while retaining server CAS history', () => {
    const doc = new Y.Doc();
    const root = doc.getMap<unknown>('root');
    const permissions = new Y.Map<unknown>();
    root.set('pending_permissions', permissions);
    for (const [id, status] of [['pending', 'pending'], ['resolved', 'resolved'], ['expired', 'expired']] as const) {
      const permission = new Y.Map<unknown>();
      permission.set('permission_id', id);
      permission.set('status', status);
      permission.set('title', id);
      permissions.set(id, permission);
    }

    expect(renderControl(doc).pendingPermissions.map((item) => item.permissionId)).toEqual(['pending']);
    expect(permissions.size).toBe(3);
  });

  it('orders actionable requests by expiry and stable identity', () => {
    const doc = new Y.Doc();
    const permissions = new Y.Map<unknown>();
    doc.getMap<unknown>('root').set('pending_permissions', permissions);
    for (const [id, expiresAt] of [['late', '2026-08-13T12:02:00Z'], ['same-b', '2026-08-13T12:01:00Z'], ['unknown', 'bad'], ['same-a', '2026-08-13T12:01:00Z']] as const) {
      const permission = new Y.Map<unknown>();
      permission.set('permission_id', id);
      permission.set('status', 'pending');
      permission.set('expires_at', expiresAt);
      permissions.set(id, permission);
    }

    expect(renderControl(doc).pendingPermissions.map((item) => item.permissionId)).toEqual(['same-a', 'same-b', 'late', 'unknown']);
  });
});
