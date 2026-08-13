declare module '*.mjs' {
  type MarkdownInlineToken = { type: string; text?: string; href?: string; children?: MarkdownInlineToken[] };
  type MarkdownBlockToken = { type: string; level?: number; language?: string; text?: string; children?: MarkdownInlineToken[] | MarkdownBlockToken[]; ordered?: boolean; items?: MarkdownInlineToken[][] };
  export function unimportedSessions<T extends { sessionId: string }>(sessions: T[], projectSessions: Array<{ acpSessionId?: string | null }>): T[];
  export function importCandidates<T extends { cwd?: string | null }>(sessions: T[], cwd: string): T[];
  export function isTurnActive(activeTurn: unknown): boolean;
  export function retainLiveRuntimeHints<T extends { activeChatId: string | null }>(sessions: T[], chats: Array<{ id: string; status: string | null }>): T[];
  export function cleanSessionTitle(title?: string | null): string;
  export function formatRelativeTime(value?: string | null, now?: number): string;
  export function shortSessionId(sessionId?: string | null): string;
  export function sessionDisplayTitle(title?: string | null, stableId?: string | null): string;
  export function connectionProblemForClose(code?: number): { code: number | null; title: string; detail: string; action: 'reconnect' | 'login' };
  export function connectionTransition(state: string, detail?: { retryMs?: number; code?: number }, hasPrincipal?: boolean): { ready: boolean; busy: boolean; status: { text: string; kind: 'idle' | 'ok' | 'warn' | 'err' }; problem: { code: number | null; title: string; detail: string; action: 'reconnect' | 'login' } | null } | null;
  export function safeHref(value?: string | null): string | null;
  export function parseInline(source?: string | null): MarkdownInlineToken[];
  export function parseMarkdown(source?: string | null): MarkdownBlockToken[];
  export function messageActivity(entries: ReadonlyArray<object>): string;
  export function nextFollowState(state: { stick: boolean; hasNewContent: boolean; previousActivity: string; activity: string }): { stick: boolean; hasNewContent: boolean; activity: string };
  export function messageTime(value?: string | null, now?: number): { label: string; exact: string } | null;
  export function acquireInert(target: { inert: boolean } | null): () => number;
  export function acquireOverlay(target: { inert: boolean } | null): { isTop: () => boolean; release: () => number };
  export function activeOverlayCount(): number;
  export function authFeedback(status: number, phase?: 'status' | 'login'): { kind: string; message: string; retryable: boolean } | null;
  export function searchProjectSessions<T, P>(query: string, projects: P[], sessions: T[]): Array<T & { project: P | null }>;
  export function runtimeState(input: { hasSession: boolean; lifecycle?: string; isOpening: boolean; hasRuntime: boolean; isSelected?: boolean; isHydrated?: boolean; chatStatus?: string | null; hasPendingPermission: boolean; turnActive: boolean }): { label: string; tone: 'idle' | 'ready' | 'busy' | 'attention' | 'danger' };
}
