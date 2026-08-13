import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CatalogActions, type CatalogActionsDependencies, type CatalogSendOptions } from './catalog-actions';

function harness(overrides: Partial<CatalogActionsDependencies> = {}) {
  let ready = true;
  let readOnly = false;
  let uncertain = false;
  let discovering: string | null = null;
  const sent: Array<{ frame: Record<string, unknown>; label: string; options: CatalogSendOptions }> = [];
  const deps: CatalogActionsDependencies = {
    isReady: () => ready,
    isReadOnly: () => readOnly,
    hasUncertainMetadata: () => uncertain,
    send: (frame, label, options) => { sent.push({ frame, label, options }); return true; },
    toast: vi.fn(),
    persistProblem: vi.fn(),
    onProjectArchived: vi.fn(),
    onSessionArchived: vi.fn(),
    discoveringProjectId: () => discovering,
    setDiscoveringProjectId: (value) => { discovering = value; },
    ...overrides,
  };
  return {
    actions: new CatalogActions(deps), deps, sent,
    setReady: (value: boolean) => { ready = value; },
    setReadOnly: (value: boolean) => { readOnly = value; },
    setUncertain: (value: boolean) => { uncertain = value; },
    discovering: () => discovering,
  };
}

describe('CatalogActions', () => {
  beforeEach(() => vi.restoreAllMocks());

  it('uses one closed mutation gate for role, connection and prior uncertainty', () => {
    const h = harness();
    h.setReadOnly(true);
    expect(h.actions.archiveProject('p1')).toBe(false);
    expect(h.deps.toast).toHaveBeenLastCalledWith('只读模式不能归档项目');

    h.setReadOnly(false); h.setReady(false);
    expect(h.actions.renameSession('s1', 'Name')).toBe(false);
    expect(h.deps.toast).toHaveBeenLastCalledWith('连接未就绪');

    h.setReady(true); h.setUncertain(true);
    expect(h.actions.restoreProject('p1')).toBe(false);
    expect(h.deps.toast).toHaveBeenLastCalledWith('先处理“结果尚未确认”的项目或会话操作');
    expect(h.sent).toHaveLength(0);
  });

  it('runs domain side effects only after committed or duplicate acknowledgement', () => {
    const h = harness();
    const onCommitted = vi.fn();
    expect(h.actions.setSessionArchived('s1', true, { onCommitted })).toBe(true);
    const request = h.sent[0];
    expect(request.label).toBe('session/archive');
    expect(request.options.retryOnUncertain).toBe(true);

    request.options.cb?.({ commandId: String(request.frame.commandId), status: 'accepted' });
    expect(h.deps.onSessionArchived).not.toHaveBeenCalled();
    request.options.cb?.({ commandId: String(request.frame.commandId), status: 'committed' });
    expect(h.deps.onSessionArchived).toHaveBeenCalledWith('s1');
    expect(onCommitted).toHaveBeenCalledOnce();
    expect(h.deps.toast).toHaveBeenCalledWith('会话已归档');
  });

  it('retains exact command identity and recovery copy on uncertain mutation', () => {
    const h = harness();
    const onFailed = vi.fn();
    h.actions.renameProject('p1', '  New name  ', { onFailed });
    const request = h.sent[0];
    expect(request.frame).toMatchObject({ type: 'project/rename', payload: { projectId: 'p1', name: 'New name' } });
    request.options.onTimeout?.();
    expect(h.deps.persistProblem).toHaveBeenCalledWith(
      '重命名项目结果尚未确认',
      expect.stringContaining('输入内容已保留'),
      request.frame.commandId,
    );
    expect(onFailed).toHaveBeenCalledOnce();
  });

  it('keeps discovery read-only, single-flight and safely retryable after timeout', () => {
    const h = harness();
    const failed = vi.fn();
    expect(h.actions.discoverSessions('p1', undefined, failed)).toBe(true);
    expect(h.discovering()).toBe('p1');
    expect(h.actions.discoverSessions('p2', undefined, failed)).toBe(false);
    expect(failed).toHaveBeenLastCalledWith('另一个项目正在刷新 ACP 会话。');

    h.sent[0].options.onTimeout?.();
    expect(h.discovering()).toBeNull();
    expect(failed).toHaveBeenLastCalledWith(expect.stringContaining('可以安全重试'));
    expect(h.sent[0].options.retryOnUncertain).toBeUndefined();
  });

  it('requires a durable session identity before closing import UI', () => {
    const h = harness();
    const onCommitted = vi.fn();
    const onFailed = vi.fn();
    h.actions.importSession('p1', 'acp-1', onCommitted, onFailed);
    const request = h.sent[0];
    request.options.cb?.({ status: 'committed' });
    expect(onCommitted).not.toHaveBeenCalled();
    request.options.cb?.({ status: 'duplicate', sessionId: 'logical-1' });
    expect(onCommitted).toHaveBeenCalledOnce();

    request.options.onTimeout?.();
    expect(onFailed).toHaveBeenCalledWith('uncertain');
    expect(h.deps.persistProblem).toHaveBeenCalledWith(
      '导入结果尚未确认',
      expect.stringContaining('原请求重试'),
      request.frame.commandId,
    );
  });
});
