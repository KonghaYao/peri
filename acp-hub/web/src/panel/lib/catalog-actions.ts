import * as H from './protocol';

export type CatalogFrame = ReturnType<typeof H.action>;

export interface CatalogAck {
  commandId?: string;
  status?: string;
  sessionId?: string;
  chatId?: string;
  [key: string]: unknown;
}

export interface CatalogError {
  commandId?: string;
  code?: string;
  message?: string;
  retryable?: boolean;
}

export interface CatalogSendOptions {
  cb?: (ack: CatalogAck) => void;
  onTimeout?: () => void;
  onError?: (error: CatalogError) => void;
  retryOnUncertain?: boolean;
}

export interface CatalogActionsDependencies {
  isReady: () => boolean;
  isReadOnly: () => boolean;
  hasUncertainMetadata: () => boolean;
  send: (frame: CatalogFrame, label: string, options: CatalogSendOptions) => boolean;
  toast: (message: string) => void;
  persistProblem: (title: string, detail: string, commandId: string) => void;
  onProjectArchived: (projectId: string) => void;
  onSessionArchived: (sessionId: string) => void;
  discoveringProjectId: () => string | null;
  setDiscoveringProjectId: (projectId: string | null) => void;
}

export interface MutationCallbacks {
  onCommitted?: () => void;
  onFailed?: () => void;
}

const committed = (ack: CatalogAck) => ack.status === 'committed' || ack.status === 'duplicate';

/**
 * Owns the complete browser lifecycle for project/session catalog commands.
 *
 * The Solid store supplies transport and local selection side effects, but it
 * cannot vary authorization gates, uncertain-result wording, retry identity or
 * terminal acknowledgement semantics between otherwise equivalent mutations.
 */
export class CatalogActions {
  constructor(private readonly deps: CatalogActionsDependencies) {}

  createProject(name: string, cwd: string, callbacks: MutationCallbacks = {}): boolean {
    if (!this.canMutate('只读模式不能创建项目')) return false;
    const frame = H.projectCreate(name, cwd);
    return this.sendMutation(frame, 'project/create', '项目已创建', {
      title: '创建项目结果尚未确认',
      detail: '项目可能已经创建。表单内容已保留，请先等待侧边栏同步，不要立即重复提交。',
    }, callbacks);
  }

  archiveProject(projectId: string, callbacks: MutationCallbacks = {}): boolean {
    if (!this.canMutate('只读模式不能归档项目')) return false;
    const frame = H.projectArchive(projectId);
    return this.sendMutation(frame, 'project/archive', '项目已归档', {
      title: '归档结果尚未确认',
      detail: '项目可能已经归档。请等待侧边栏同步后再决定是否重试。',
    }, callbacks, () => this.deps.onProjectArchived(projectId));
  }

  restoreProject(projectId: string, callbacks: MutationCallbacks = {}): boolean {
    if (!this.canMutate('只读模式不能恢复项目')) return false;
    const frame = H.projectRestore(projectId);
    return this.sendMutation(frame, 'project/restore', '项目已恢复', {
      title: '恢复项目结果尚未确认',
      detail: '项目可能已经恢复。请等待侧边栏同步后再决定是否重试。',
    }, callbacks);
  }

  renameProject(projectId: string, name: string, callbacks: MutationCallbacks = {}): boolean {
    if (!name.trim()) return false;
    if (!this.canMutate('只读模式不能重命名项目')) return false;
    const frame = H.projectRename(projectId, name.trim());
    return this.sendMutation(frame, 'project/rename', '项目已重命名', {
      title: '重命名项目结果尚未确认',
      detail: '名称可能已经保存。输入内容已保留，请先等待侧边栏同步。',
    }, callbacks);
  }

  renameSession(sessionId: string, name: string, callbacks: MutationCallbacks = {}): boolean {
    if (!name.trim()) return false;
    if (!this.canMutate('只读模式不能重命名会话')) return false;
    const frame = H.persistedSessionRename(sessionId, name.trim());
    return this.sendMutation(frame, 'session/rename', '会话已重命名', {
      title: '重命名会话结果尚未确认',
      detail: '名称可能已经保存。输入内容已保留，请先等待侧边栏同步。',
    }, callbacks);
  }

  setSessionArchived(sessionId: string, archive: boolean, callbacks: MutationCallbacks = {}): boolean {
    if (!this.canMutate('只读模式不能整理会话')) return false;
    const frame = archive ? H.persistedSessionArchive(sessionId) : H.persistedSessionRestore(sessionId);
    return this.sendMutation(frame, archive ? 'session/archive' : 'session/restore', archive ? '会话已归档' : '会话已恢复', {
      title: archive ? '归档会话结果尚未确认' : '恢复会话结果尚未确认',
      detail: archive
        ? '会话可能已经归档。请等待侧边栏同步，再使用原请求确认。'
        : '会话可能已经恢复。请等待侧边栏同步，再使用原请求确认。',
    }, callbacks, archive ? () => this.deps.onSessionArchived(sessionId) : undefined);
  }

  importSession(
    projectId: string,
    acpSessionId: string,
    onCommitted?: () => void,
    onFailed?: (kind: 'failed' | 'uncertain') => void,
  ): boolean {
    if (!this.canMutate('只读模式不能导入会话')) return false;
    const frame = H.persistedSessionImport(projectId, acpSessionId);
    return this.deps.send(frame, 'session/import', {
      retryOnUncertain: true,
      cb: (ack) => {
        if (!committed(ack) || !ack.sessionId) return;
        this.deps.toast('会话已加入侧边栏');
        onCommitted?.();
      },
      onError: () => onFailed?.('failed'),
      onTimeout: () => {
        this.deps.persistProblem('导入结果尚未确认', '服务器可能已导入此会话。请等待侧边栏刷新；如需确认，请使用原请求重试。', frame.commandId);
        onFailed?.('uncertain');
      },
    });
  }

  discoverSessions(projectId: string, onCommitted?: () => void, onFailed?: (message: string) => void): boolean {
    if (!this.deps.isReady() || this.deps.isReadOnly() || this.deps.discoveringProjectId()) {
      onFailed?.(this.deps.isReadOnly()
        ? '只读模式不能刷新 ACP 会话。'
        : !this.deps.isReady()
          ? '连接未就绪，暂时不能读取 ACP 会话。'
          : '另一个项目正在刷新 ACP 会话。');
      return false;
    }
    const frame = H.persistedSessionDiscover(projectId);
    this.deps.setDiscoveringProjectId(projectId);
    const finish = () => this.deps.setDiscoveringProjectId(null);
    return this.deps.send(frame, 'session/discover', {
      cb: (ack) => {
        if (!committed(ack)) return;
        finish();
        onCommitted?.();
      },
      onError: (error) => {
        finish();
        onFailed?.(error.message || '无法读取 ACP 会话。');
      },
      onTimeout: () => {
        finish();
        onFailed?.('读取 ACP 会话超时。临时 discovery runtime 会由 server 清理，可以安全重试。');
      },
    });
  }

  private canMutate(readOnlyMessage: string): boolean {
    if (this.deps.isReadOnly()) {
      this.deps.toast(readOnlyMessage);
      return false;
    }
    if (!this.deps.isReady()) {
      this.deps.toast('连接未就绪');
      return false;
    }
    if (this.deps.hasUncertainMetadata()) {
      this.deps.toast('先处理“结果尚未确认”的项目或会话操作');
      return false;
    }
    return true;
  }

  private sendMutation(
    frame: CatalogFrame,
    label: string,
    successMessage: string,
    uncertain: { title: string; detail: string },
    callbacks: MutationCallbacks,
    beforeCommitted?: () => void,
  ): boolean {
    return this.deps.send(frame, label, {
      retryOnUncertain: true,
      cb: (ack) => {
        if (!committed(ack)) return;
        beforeCommitted?.();
        this.deps.toast(successMessage);
        callbacks.onCommitted?.();
      },
      onError: () => callbacks.onFailed?.(),
      onTimeout: () => {
        this.deps.persistProblem(uncertain.title, uncertain.detail, frame.commandId);
        callbacks.onFailed?.();
      },
    });
  }
}
