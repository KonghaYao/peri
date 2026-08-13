export interface NavigableSession {
  id: string;
  lifecycle: string;
  activeChatId?: string | null;
}

export interface OpeningSession {
  commandId: string;
  sessionId: string;
  previousSessionId: string | null;
  previousChatId: string | null;
}

export interface SessionNavigationSnapshot {
  opening: OpeningSession | null;
  restoringSessionId: string | null;
  restoreAttempted: boolean;
}

export type SessionNavigationEvent =
  | { type: 'catalog'; ready: boolean; readOnly: boolean; preferredId: string | null; selectedSessionId: string | null; sessions: NavigableSession[] }
  | { type: 'open-started'; commandId: string; sessionId: string; previousSessionId: string | null; previousChatId: string | null }
  | { type: 'open-terminal'; commandId?: string; status?: string; chatId?: string }
  | { type: 'open-failed'; commandId?: string }
  | { type: 'open-uncertain'; commandId: string }
  | { type: 'local-select'; sessionId: string; chatId: string }
  | { type: 'connection-lost' }
  | { type: 'reset' };

export type SessionNavigationEffect =
  | { type: 'request-open'; sessionId: string }
  | { type: 'activate'; sessionId: string; chatId: string }
  | { type: 'forget-preference' };

/**
 * Owns logical-session navigation independently from transport and SolidJS.
 *
 * The navigator is the sole arbiter of whether a runtime chat may replace the
 * current selection. In particular, a terminal acknowledgement can activate a
 * chat only while its exact open command is still current. Late acknowledgements
 * remain available to CommandTracker for durable reconciliation, but cannot
 * move the UI to a stale chat.
 */
export class SessionNavigator {
  private state: SessionNavigationSnapshot = {
    opening: null,
    restoringSessionId: null,
    restoreAttempted: false,
  };

  constructor(private readonly onChange?: (snapshot: SessionNavigationSnapshot) => void) {}

  snapshot(): Readonly<SessionNavigationSnapshot> {
    return this.state;
  }

  transition(event: SessionNavigationEvent): SessionNavigationEffect[] {
    switch (event.type) {
      case 'catalog':
        return this.reconcileCatalog(event);
      case 'open-started':
        if (this.state.opening) return [];
        this.update({
          ...this.state,
          opening: {
            commandId: event.commandId,
            sessionId: event.sessionId,
            previousSessionId: event.previousSessionId,
            previousChatId: event.previousChatId,
          },
        });
        return [];
      case 'open-terminal': {
        const opening = this.state.opening;
        if (!opening || opening.commandId !== event.commandId) return [];
        if (!['committed', 'duplicate'].includes(event.status || '') || !event.chatId) return [];
        this.update({ ...this.state, opening: null, restoringSessionId: null });
        return [{ type: 'activate', sessionId: opening.sessionId, chatId: event.chatId }];
      }
      case 'open-failed':
      case 'open-uncertain':
        if (this.state.opening?.commandId !== event.commandId) return [];
        this.update({ ...this.state, opening: null, restoringSessionId: null });
        return [];
      case 'local-select':
        this.update({ ...this.state, restoringSessionId: null });
        return [{ type: 'activate', sessionId: event.sessionId, chatId: event.chatId }];
      case 'connection-lost':
        this.update({ ...this.state, opening: null, restoringSessionId: null });
        return [];
      case 'reset':
        this.update({ opening: null, restoringSessionId: null, restoreAttempted: false });
        return [];
    }
  }

  private reconcileCatalog(event: Extract<SessionNavigationEvent, { type: 'catalog' }>): SessionNavigationEffect[] {
    if (!event.ready || this.state.restoreAttempted || event.selectedSessionId || this.state.opening) return [];
    const preferred = event.preferredId
      ? event.sessions.find((session) => session.id === event.preferredId && session.lifecycle === 'ready')
      : null;
    if (!preferred) {
      this.update({ ...this.state, restoreAttempted: true, restoringSessionId: null });
      return event.preferredId ? [{ type: 'forget-preference' }] : [];
    }
    if (event.readOnly) {
      this.update({ ...this.state, restoreAttempted: true, restoringSessionId: null });
      return preferred.activeChatId
        ? [{ type: 'activate', sessionId: preferred.id, chatId: preferred.activeChatId }]
        : [];
    }
    this.update({ ...this.state, restoreAttempted: true, restoringSessionId: preferred.id });
    return [{ type: 'request-open', sessionId: preferred.id }];
  }

  private update(next: SessionNavigationSnapshot): void {
    this.state = next;
    this.onChange?.(next);
  }
}
