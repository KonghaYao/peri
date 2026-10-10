import { errorText, type ChatApi } from '../api/client';
import {
  ChatSync, clientCommandFailure, nextCommandId,
  type ChatCommandPort, type ChatSyncOptions, type CommandFailure, type CommandOutcome, type SyncState, type SyncStatus,
} from '../api/sync';
import type { createChatQueries } from './queries';
import type { Chat, Phase } from './types';
import type { InteractionAnswer } from '@peri-code/sdk/view';

export interface ChatView {
  selected: Chat | null;
  state: SyncState | null;
  phase: Phase;
  historyReady: boolean;
  error: string;
}

/** 服务端确定拒绝：命令未生效、执行状态已知，只给提示，不进入 uncertain。 */
const REFUSAL_TEXT: Record<string, string> = {
  busy: '会话正在执行中，请等待本轮完成或先停止任务。',
  blocked: '会话执行被阻断，请先确认停止状态。',
  settled: '本轮任务已经结算，无需再次操作。',
  unauthorized: '身份验证失败，请在设置中检查访问令牌。',
  unavailable: '会话暂时不可用，请稍后重试。',
};

function refusalText(failure: CommandFailure): string {
  return REFUSAL_TEXT[failure.code] ?? `命令未被接受：${failure.message}`;
}

/** 交互回答失败码 → 用户可执行的提示（沿用原有鉴权/结算文案）。 */
function answerFailureText(failure: CommandFailure): string {
  if (failure.code === 'unauthorized') return '身份验证失败，请在设置中检查访问令牌。';
  if (failure.code === 'settled' || failure.code === 'expired') return '这个交互已经结算或失效，请刷新后重试。';
  return failure.source === 'client' ? `${failure.message}请刷新后重试。` : `回答未能提交：${failure.message}`;
}

export class ChatSession {
  view: ChatView = { selected: null, state: null, phase: 'idle', historyReady: true, error: '' };
  private revision = 0;
  private selectionRequest = 0;
  private disposed = false;
  private navigation: AbortController | undefined;
  private sync: ChatSync | undefined;
  private synced = false;
  private syncStatus: SyncStatus = 'connecting';
  private state: SyncState | undefined;
  private selecting = false;
  private stopping = false;
  private uncertain = false;
  private pending = false;
  private baseline = '';
  private command: Promise<void> | undefined;
  private releaseReady: ((ready: boolean) => void) | undefined;

  constructor(
    private readonly api: Pick<ChatApi, 'create'>,
    private readonly queries: ReturnType<typeof createChatQueries>,
    private readonly token: string,
    private readonly onChange: (view: ChatView) => void,
    private readonly syncOptions: Pick<ChatSyncOptions, 'socket' | 'origin' | 'reconnectDelayMs' | 'commandTimeoutMs'> = {},
  ) {}

  /** 会话命令通道：执行驻留在接受连接的 isolate，命令只能经当前同步连接下发。 */
  private channel(): ChatCommandPort | undefined {
    return this.disposed ? undefined : this.sync;
  }

  private update(change: Partial<ChatView>) {
    if (this.disposed) return;
    this.view = { ...this.view, ...change };
    this.onChange(this.view);
  }

  /** 执行状态不确定：阻断后续发送，并要求先确认停止。 */
  private block(reason: string, suffix: string) {
    this.uncertain = true;
    this.update({ phase: this.phase(), error: `${reason} ${suffix}` });
  }

  private phase(): Phase {
    if (this.stopping) return 'cancelling';
    if (this.uncertain || this.state?.executionBlocked) return 'blocked';
    if (this.pending || this.state?.running) return 'streaming';
    return this.synced || this.syncStatus === 'failed' ? 'idle' : 'syncing';
  }

  dismissError() { this.update({ error: '' }); }

  private receive(state: SyncState) {
    this.state = state;
    if (this.pending && (state.running || JSON.stringify(state.entries) !== this.baseline)) this.pending = false;
    this.queries.setHistory(state);
    this.queries.mergeChat(state.chat);
    this.update({ selected: state.chat, state,
      error: state.error ?? (this.uncertain ? this.view.error : ''), phase: this.phase() });
  }

  private status(status: SyncStatus, error?: Error) {
    this.syncStatus = status;
    this.synced = status === 'synced';
    this.update({ historyReady: this.synced, phase: this.phase(), ...(error ? { error: errorText(error) } : {}) });
    if (this.synced || status === 'failed') {
      this.releaseReady?.(this.synced);
      this.releaseReady = undefined;
    }
  }

  async select(chat: Chat | null): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    const request = ++this.selectionRequest;
    this.selecting = true;
    try {
      if (!await this.stop() || this.disposed || request !== this.selectionRequest) return false;
      const revision = ++this.revision;
      this.navigation?.abort();
      this.sync?.dispose();
      this.releaseReady?.(false);
      this.sync = undefined;
      this.synced = false;
      this.syncStatus = 'connecting';
      this.state = undefined;
      const controller = new AbortController();
      this.navigation = controller;
      const current = () => !this.disposed && this.revision === revision && !controller.signal.aborted;
      this.update({ selected: chat, state: null, historyReady: false, phase: 'loading', error: '' });
      try {
        if (!this.token) {
          this.update({ phase: 'idle', historyReady: true });
          return false;
        }
        const target = chat ?? await this.api.create(controller.signal);
        if (!current()) return false;
        this.update({ selected: target });
        this.queries.mergeChat(target);
        const history = await this.queries.readHistory(target.id, controller.signal);
        if (!current()) return false;
        this.update({ selected: history.chat, state: history, phase: 'syncing' });
        this.queries.mergeChat(history.chat);
        const ready = new Promise<boolean>(resolve => { this.releaseReady = resolve; });
        this.sync = new ChatSync({
          ...this.syncOptions, chatId: target.id, token: this.token,
          onState: state => { if (current()) this.receive(state); },
          onStatus: (status, error) => { if (current()) this.status(status, error); },
        });
        return await ready;
      } catch (failure) {
        if (current()) this.update({ error: errorText(failure), phase: 'idle', historyReady: false });
        return false;
      }
    } finally { if (request === this.selectionRequest) this.selecting = false; }
  }

  /** prompt 命令：未建立连接时明确失败，不静默丢弃。 */
  private async issuePrompt(content: string): Promise<CommandOutcome> {
    const channel = this.channel();
    if (!channel) return clientCommandFailure('unavailable', '会话尚未建立同步连接，命令未发送。');
    return await channel.command({ type: 'command', id: nextCommandId(), op: 'prompt', content });
  }

  async send(content: string): Promise<boolean> {
    if (this.disposed || !this.token || !content.trim() || this.selecting || this.stopping || this.pending ||
      this.command || this.uncertain || this.state?.running || this.state?.executionBlocked) return false;
    if (!this.view.selected && !await this.select(null)) return false;
    if (this.disposed || !this.synced || !this.view.selected || this.state?.running || this.state?.executionBlocked) return false;
    const revision = this.revision;
    this.pending = true;
    this.baseline = JSON.stringify(this.state?.entries ?? []);
    this.update({ phase: 'streaming', error: '' });
    const command = Promise.resolve().then(() => this.issuePrompt(content.trim()));
    const settled = command.then(() => undefined, () => undefined);
    this.command = settled;
    try {
      const result = await command;
      if (result.ok) return !this.disposed && revision === this.revision;
      this.pending = false;
      // 服务端答否说明命令未生效；客户端未获确认时执行状态未知，必须阻断重跑。
      if (result.source === 'client') this.block(errorText(result.message), '尚未确认执行状态，请先停止任务。');
      else this.update({ phase: this.phase(), error: refusalText(result) });
      return false;
    } catch (failure) {
      if (!this.disposed && revision === this.revision) {
        this.pending = false;
        this.block(errorText(failure), '尚未确认执行状态，请先停止任务。');
      }
      return false;
    } finally {
      if (this.command === settled) this.command = undefined;
    }
  }

  async stop(): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    if (!this.view.selected || (!this.pending && !this.command && !this.uncertain && !this.state?.running &&
      !this.state?.executionBlocked)) return true;
    this.stopping = true;
    this.update({ phase: 'cancelling', error: '' });
    try {
      // 等待在途 prompt 结算，避免取消先于命令到达；取消只在真实确认后成立。
      await this.command;
      if (this.disposed) return false;
      const channel = this.channel();
      const result = channel
        ? await channel.command({ type: 'command', id: nextCommandId(), op: 'cancel' })
        : clientCommandFailure('unavailable', '会话尚未建立同步连接。');
      if (!result.ok) throw new Error(result.source === 'client' ? result.message : refusalText(result));
      if (this.disposed) return false;
      this.pending = false;
      this.uncertain = false;
      return true;
    } catch (failure) {
      this.uncertain = true;
      this.update({ error: `${errorText(failure)} 尚未确认任务停止，请再次点击停止。` });
      return false;
    } finally {
      this.stopping = false;
      this.update({ phase: this.phase() });
    }
  }

  /** 交互回答：经当前会话的命令通道下发；未同步或已被拒绝时抛出可展示的明确原因。 */
  async answerInteraction(interactionId: string, answer: InteractionAnswer): Promise<void> {
    const channel = this.channel();
    if (!channel) throw new Error('会话同步尚未就绪，回答未提交，请稍后重试。');
    const result = await channel.command({ type: 'command', id: nextCommandId(), op: 'interaction', interactionId, answer });
    if (!result.ok) throw new Error(answerFailureText(result));
  }

  /** 工具载荷全文：同一条连接读取；未就绪时返回 client 失败，由 reader 映射成 unavailable。 */
  async readPayload(refId: string, signal?: AbortSignal): Promise<CommandOutcome> {
    const channel = this.channel();
    if (!channel) return { ok: false, code: 'unavailable', message: '会话同步尚未就绪，无法读取全文。', source: 'client' };
    return await channel.command({ type: 'command', id: nextCommandId(), op: 'payload', refId }, { signal });
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.revision++;
    this.navigation?.abort();
    this.sync?.dispose();
    this.releaseReady?.(false);
    this.releaseReady = undefined;
  }
}
