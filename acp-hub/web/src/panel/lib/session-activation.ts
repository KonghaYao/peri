import * as H from './protocol';
import type { ProjectSessionInfo } from './registry-view';
import { SessionNavigator, type OpeningSession, type SessionNavigationEffect, type SessionNavigationSnapshot } from './session-navigator';
import {
  acceptQuickStart,
  completeQuickStart,
  failQuickStart,
  markQuickStartUncertain,
  quickStartSubmission,
  resetQuickStart,
  retryQuickStartDelivery,
  startQuickStart,
} from './quick-start-delivery';

type ActionFrame = ReturnType<typeof H.action>;
export interface ActivationAck {
  commandId?: string;
  status?: string;
  sessionId?: string;
  chatId?: string;
}
export interface ActivationError { message?: string; retryable?: boolean }
export interface ActivationSendOptions {
  cb?: (ack: ActivationAck) => void;
  onAccepted?: (ack: ActivationAck) => void;
  onTimeout?: () => void;
  onError?: (error: ActivationError) => void;
  retryOnUncertain?: boolean;
}
export interface OpenSessionCallbacks {
  onCommitted?: () => void;
  onFailed?: (message: string) => void;
  onUncertain?: () => void;
}

export interface SessionActivationDependencies {
  isReady: () => boolean;
  isReadOnly: () => boolean;
  hasUncertainMetadata: () => boolean;
  hasMessageSubmission: () => boolean;
  creatingProjectId: () => string | null;
  setCreatingProjectId: (projectId: string | null) => void;
  sessions: () => ProjectSessionInfo[];
  selectedSessionId: () => string | null;
  currentChatId: () => string | null;
  preferredSessionId: () => string | null;
  send: (frame: ActionFrame, label: string, options: ActivationSendOptions) => boolean;
  retry: (commandId: string) => 'sent' | 'missing' | 'already_pending' | 'unavailable';
  hasUncertainCommand: (commandId: string) => boolean;
  activate: (sessionId: string, chatId: string) => void;
  forgetPreference: () => void;
  sendFirstMessage: (text: string) => boolean;
  onNavigationChange: (snapshot: SessionNavigationSnapshot) => void;
  toast: (message: string) => void;
  persistProblem: (title: string, detail: string, commandId?: string) => void;
}

const isCommitted = (ack: ActivationAck) => ack.status === 'committed' || ack.status === 'duplicate';

/**
 * Owns logical-session activation policy across create, quick start, restore and open.
 * Transport, persistence UI and actual chat subscription remain injected effects.
 */
export class SessionActivation {
  private readonly navigator: SessionNavigator;

  constructor(private readonly deps: SessionActivationDependencies) {
    this.navigator = new SessionNavigator(deps.onNavigationChange);
  }

  create(projectId: string, title?: string): boolean {
    const rejection = this.creationRejection();
    if (rejection) {
      this.deps.toast(rejection);
      return false;
    }
    const frame = H.persistedSessionCreate(projectId, title);
    this.deps.setCreatingProjectId(projectId);
    const finish = () => this.deps.setCreatingProjectId(null);
    return this.deps.send(frame, 'session/create', {
      retryOnUncertain: true,
      cb: (ack) => {
        if (!isCommitted(ack)) return;
        finish();
        if (!ack.sessionId || !ack.chatId) {
          this.deps.persistProblem(
            '创建会话回复不完整',
            '服务器已提交操作，但没有同时返回逻辑会话和运行实例标识。页面没有切换，以避免进入错误会话；请等待侧边栏投影同步。',
            frame.commandId,
          );
          return;
        }
        this.applyEffects(this.navigator.transition({ type: 'local-select', sessionId: ack.sessionId, chatId: ack.chatId }));
      },
      onError: finish,
      onTimeout: () => {
        finish();
        this.deps.persistProblem('创建会话结果尚未确认', '会话可能已经创建。请等待侧边栏同步，不要立即再次点击新建。', frame.commandId);
      },
    });
  }

  quickStart(projectId: string, text: string): boolean {
    const source = text.trim();
    if (!source || quickStartSubmission() || this.deps.hasMessageSubmission() || this.deps.creatingProjectId()) return false;
    const rejection = this.mutationRejection();
    if (rejection) {
      this.deps.toast(rejection);
      return false;
    }
    const firstLine = source.split(/\r?\n/, 1)[0];
    const title = [...firstLine].slice(0, 60).join('');
    const frame = H.persistedSessionCreate(projectId, title);
    if (!startQuickStart(frame.commandId, projectId, source)) return false;
    return this.deps.send(frame, 'session/create', {
      retryOnUncertain: true,
      onAccepted: () => acceptQuickStart(frame.commandId),
      cb: (ack) => {
        const activation = completeQuickStart(frame.commandId, ack.status, ack.sessionId, ack.chatId);
        if (!activation) return;
        this.deps.activate(activation.sessionId, activation.chatId);
        if (!this.deps.sendFirstMessage(activation.text)) {
          this.deps.persistProblem(
            '首条消息尚未发送',
            `会话已经创建，但消息未提交。请复制以下原文后重新发送：\n\n${activation.text}`,
            activation.commandId,
          );
        }
      },
      onTimeout: () => markQuickStartUncertain(frame.commandId),
      onError: (error) => failQuickStart(frame.commandId, error.message || '无法创建会话。你的消息仍保留。'),
    });
  }

  retryQuickStart(): void {
    const current = quickStartSubmission();
    if (!current || current.phase !== 'uncertain' || !this.deps.hasUncertainCommand(current.commandId)) return;
    if (!this.deps.isReady()) {
      this.deps.toast('连接未就绪，暂时不能重新确认');
      return;
    }
    if (this.deps.retry(current.commandId) === 'sent') retryQuickStartDelivery(current.commandId);
    else this.deps.toast('连接未就绪，原请求仍已保留');
  }

  navigate(sessionId: string, callbacks: OpenSessionCallbacks = {}): boolean {
    const session = this.deps.sessions().find((item) => item.id === sessionId);
    if (!session || session.archivedAt || session.lifecycle !== 'ready') {
      callbacks.onFailed?.('会话尚未就绪');
      return false;
    }
    if (this.deps.isReadOnly()) {
      if (!session.activeChatId) {
        callbacks.onFailed?.('只读模式只能查看已启动的会话');
        return false;
      }
      this.applyEffects(this.navigator.transition({ type: 'local-select', sessionId: session.id, chatId: session.activeChatId }));
      callbacks.onCommitted?.();
      return true;
    }
    return this.open(session.id, callbacks);
  }

  reconcileCatalog(sessions: ProjectSessionInfo[]): void {
    this.applyEffects(this.navigator.transition({
      type: 'catalog',
      ready: this.deps.isReady(),
      readOnly: this.deps.isReadOnly(),
      preferredId: this.deps.preferredSessionId(),
      selectedSessionId: this.deps.selectedSessionId(),
      sessions: sessions.filter((session) => !session.archivedAt),
    }));
  }

  connectionLost(): void { this.applyEffects(this.navigator.transition({ type: 'connection-lost' })); }

  reset(): void {
    resetQuickStart();
    this.deps.setCreatingProjectId(null);
    this.applyEffects(this.navigator.transition({ type: 'reset' }));
  }

  private open(sessionId: string, callbacks: OpenSessionCallbacks = {}): boolean {
    const rejection = this.openRejection();
    if (rejection) {
      this.deps.toast(rejection);
      callbacks.onFailed?.(rejection);
      return false;
    }
    const frame = H.persistedSessionOpen(sessionId);
    this.navigator.transition({
      type: 'open-started',
      commandId: frame.commandId,
      sessionId,
      previousSessionId: this.deps.selectedSessionId(),
      previousChatId: this.deps.currentChatId(),
    });
    return this.deps.send(frame, 'session/open', {
      cb: (ack) => {
        const effects = this.navigator.transition({ type: 'open-terminal', commandId: ack.commandId, status: ack.status, chatId: ack.chatId });
        if (!effects.length) return;
        this.applyEffects(effects);
        callbacks.onCommitted?.();
      },
      onError: (error) => {
        this.navigator.transition({ type: 'open-failed', commandId: frame.commandId });
        callbacks.onFailed?.(error.message || '无法打开会话');
      },
      onTimeout: () => {
        this.navigator.transition({ type: 'open-uncertain', commandId: frame.commandId });
        this.deps.persistProblem('打开结果尚未确认', '未切换当前会话。请等待侧边栏状态同步；如果会话仍未启动，再重新打开。', frame.commandId);
        callbacks.onUncertain?.();
      },
    });
  }

  private mutationRejection(): string | null {
    if (this.deps.isReadOnly()) return '只读模式不能创建会话';
    if (!this.deps.isReady()) return '连接未就绪';
    if (this.deps.hasUncertainMetadata()) return '先处理“结果尚未确认”的项目或会话操作';
    return null;
  }

  private creationRejection(): string | null {
    const rejection = this.mutationRejection();
    if (rejection) return rejection;
    if (this.deps.creatingProjectId() || quickStartSubmission()) return '已有会话正在创建';
    return null;
  }

  private openRejection(): string | null {
    if (this.deps.isReadOnly()) return '只读模式不能打开运行会话';
    if (!this.deps.isReady()) return '连接未就绪';
    if (this.navigator.snapshot().opening) return '另一个会话正在打开';
    if (this.deps.hasUncertainMetadata()) return '先确认上一项操作';
    return null;
  }

  private applyEffects(effects: SessionNavigationEffect[]): void {
    for (const effect of effects) {
      if (effect.type === 'request-open') this.open(effect.sessionId);
      if (effect.type === 'activate') this.deps.activate(effect.sessionId, effect.chatId);
      if (effect.type === 'forget-preference') this.deps.forgetPreference();
    }
  }
}

export type { OpeningSession };
