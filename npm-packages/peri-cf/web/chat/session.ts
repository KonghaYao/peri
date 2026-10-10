import { ChatApi, errorText } from '../api/client';
import { ChatSync, type ChatSyncOptions, type SyncState, type SyncStatus } from '../api/sync';
import type { createChatQueries } from './queries';
import type { Chat, Message, Phase } from './types';

export interface ChatView {
  selected: Chat | null;
  messages: Message[];
  phase: Phase;
  historyReady: boolean;
  error: string;
}

export class ChatSession {
  view: ChatView = { selected: null, messages: [], phase: 'idle', historyReady: true, error: '' };
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
    private readonly api: Pick<ChatApi, 'create' | 'send' | 'cancel'>,
    private readonly queries: ReturnType<typeof createChatQueries>,
    private readonly token: string,
    private readonly onChange: (view: ChatView) => void,
    private readonly syncOptions: Pick<ChatSyncOptions, 'socket' | 'origin' | 'reconnectDelayMs'> = {},
  ) {}

  private update(change: Partial<ChatView>) {
    if (this.disposed) return;
    this.view = { ...this.view, ...change };
    this.onChange(this.view);
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
    if (this.pending && (state.running || JSON.stringify(state.messages) !== this.baseline)) this.pending = false;
    this.queries.setHistory(state);
    this.queries.mergeChat(state.chat);
    this.update({ selected: state.chat, messages: state.messages,
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
      this.update({ selected: chat, messages: [], historyReady: false, phase: 'loading', error: '' });
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
        this.update({ selected: history.chat, messages: history.messages, phase: 'syncing' });
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

  async send(content: string): Promise<boolean> {
    if (this.disposed || !this.token || !content.trim() || this.selecting || this.stopping || this.pending ||
      this.command || this.uncertain || this.state?.running || this.state?.executionBlocked) return false;
    if (!this.view.selected && !await this.select(null)) return false;
    if (this.disposed || !this.synced || !this.view.selected || this.state?.running || this.state?.executionBlocked) return false;
    const revision = this.revision;
    const chatId = this.view.selected.id;
    this.pending = true;
    this.baseline = JSON.stringify(this.state?.messages ?? []);
    this.update({ phase: 'streaming', error: '' });
    const command = Promise.resolve().then(() => this.api.send(chatId, content.trim()));
    this.command = command;
    try {
      await command;
      return !this.disposed && revision === this.revision;
    } catch (failure) {
      if (!this.disposed && revision === this.revision) {
        this.uncertain = true;
        this.update({ phase: this.phase(), error: `${errorText(failure)} 尚未确认执行状态，请先停止任务。` });
      }
      return false;
    } finally {
      if (this.command === command) this.command = undefined;
    }
  }

  async stop(): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    if (!this.view.selected || (!this.pending && !this.command && !this.uncertain && !this.state?.running &&
      !this.state?.executionBlocked)) return true;
    this.stopping = true;
    this.update({ phase: 'cancelling', error: '' });
    try {
      try { await this.command; } catch {}
      if (this.disposed) return false;
      await this.api.cancel(this.view.selected.id);
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
