import { createEffect, createSignal, For, Show } from 'solid-js';
import { Dialog, primaryShortcut, Spinner, TextField } from '../../ui';
import { openProjectSession, openingSessionId, projectSessions, projects, readOnly, selectedSessionId, selectPersistedSessionLocally } from '../store';
import { formatRelativeTime, sessionDisplayTitle, shortSessionId } from '../lib/recovery-state.mjs';
import { searchProjectSessions } from '../lib/session-search.mjs';
import type { ProjectSessionInfo } from '../lib/yjs';

type SearchResult = ProjectSessionInfo & { project: { name: string } | null };

export function SessionSearch(props: { open: boolean; onClose: () => void; onSelected?: () => void }) {
  const [query, setQuery] = createSignal('');
  const [problem, setProblem] = createSignal<string | null>(null);
  let resultList: HTMLDivElement | undefined;
  const results = () => searchProjectSessions(query(), projects().filter((project) => !project.archivedAt), projectSessions()) as SearchResult[];
  createEffect(() => { if (!props.open) { setQuery(''); setProblem(null); } });
  const choose = (session: ProjectSessionInfo) => {
    setProblem(null);
    if (readOnly() && session.activeChatId) { selectPersistedSessionLocally(session.id, session.activeChatId); props.onClose(); props.onSelected?.(); }
    else if (!readOnly()) openProjectSession(session.id, {
      onCommitted: () => { props.onClose(); props.onSelected?.(); },
      onFailed: (message) => setProblem(message),
      onUncertain: () => setProblem('打开结果尚未确认。当前会话没有切换，请等待状态同步后再决定是否重试。'),
    });
    else return;
  };
  const focusResult = (direction: 1 | -1) => {
    const items = [...(resultList?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') || [])];
    if (!items.length) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = current < 0 ? (direction > 0 ? 0 : items.length - 1) : (current + direction + items.length) % items.length;
    items[next]?.focus();
  };
  return <Dialog open={props.open} title="搜索会话" showHeader dismissible={!openingSessionId()} onClose={props.onClose}>
    <div class="session-search-dialog">
      <TextField aria-label="搜索会话" value={query()} onInput={(event) => setQuery(event.currentTarget.value)} onKeyDown={(event) => { if (event.key === 'ArrowDown') { event.preventDefault(); focusResult(1); } }} placeholder="搜索标题、项目、目录或 Session ID" autofocus />
      <div ref={resultList} class="session-search-results" role="listbox" aria-label="搜索结果" onKeyDown={(event) => { if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); focusResult(event.key === 'ArrowDown' ? 1 : -1); } }}>
        <Show when={query().trim()} fallback={<div class="session-search-hint"><kbd>{primaryShortcut('K')}</kbd> 随时打开搜索。输入项目名或会话标题开始。</div>}>
          <For each={results()} fallback={<div class="session-search-hint">没有匹配的已保存会话</div>}>{(session) =>
            <button type="button" role="option" aria-selected={selectedSessionId() === session.id} disabled={(readOnly() && !session.activeChatId) || session.lifecycle !== 'ready' || !!openingSessionId()} onClick={() => choose(session)}>
              <span><strong>{sessionDisplayTitle(session.title, session.acpSessionId || session.id)}</strong><small>{session.project?.name || '未知项目'} · {formatRelativeTime(session.lastOpenedAt || session.updatedAt)}</small></span>
              <Show when={openingSessionId() === session.id} fallback={<code>…{shortSessionId(session.acpSessionId || session.id)}</code>}><Spinner label="正在打开" /></Show>
            </button>
          }</For>
        </Show>
      </div>
      <Show when={problem()}>{(message) => <p class="session-search-problem" role="alert">{message()}</p>}</Show>
    </div>
  </Dialog>;
}
