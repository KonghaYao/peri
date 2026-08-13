import { createSignal, For, Show } from 'solid-js';
import { createProject, createProjectSession, importableSessions, importProjectSession, openProjectSession, openingSessionId, projects, projectSessions, readOnly, renameProjectSession, selectedSessionId, selectPersistedSessionLocally } from '../store';
import { Button, IconButton } from '../../ui/Button';
import { TextField } from '../../ui/Field';
import { Dialog } from '../../ui/Dialog';
import { Menu } from '../../ui/Menu';
import { Spinner } from '../../ui/Spinner';
import { importCandidates } from '../lib/session-import.mjs';

function PlusIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 4v12M4 10h12" /></svg>; }
function ChatIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4 4.5h12v9H8l-4 3v-12Z" /></svg>; }
function ImportIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3v9m0 0 3-3m-3 3L7 9M4 14.5h12v2H4z" /></svg>; }

export function ProjectSidebar(props: { onNavigate?: () => void }) {
  const [creating, setCreating] = createSignal(false);
  const [name, setName] = createSignal('');
  const [cwd, setCwd] = createSignal('');
  const [editing, setEditing] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal('');
  const [importingProject, setImportingProject] = createSignal<string | null>(null);
  const renameValid = () => !!draft().trim();

  const submitProject = (e: SubmitEvent) => {
    e.preventDefault();
    if (!cwd().trim()) return;
    createProject(name().trim() || cwd().split('/').filter(Boolean).at(-1) || 'Project', cwd().trim());
    setName(''); setCwd(''); setCreating(false);
  };

  return (
    <nav class="project-sidebar" aria-label="项目与会话">
      <div class="brand-row"><span class="brand-glyph">✦</span><span>acp-hub</span></div>
      <Button class="new-project-button" disabled={readOnly()} onClick={() => setCreating(true)}><PlusIcon />新建项目</Button>
      <Show when={readOnly()}><div class="readonly-label">只读模式</div></Show>
      <Dialog open={creating()} title="新建项目" onClose={() => setCreating(false)}>
        <form class="project-form" onSubmit={submitProject}>
          <TextField label="项目名称" value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="perihelion" autofocus />
          <TextField label="工作目录" value={cwd()} onInput={(e) => setCwd(e.currentTarget.value)} placeholder="/absolute/path" />
          <div class="form-actions"><Button type="button" onClick={() => setCreating(false)}>取消</Button><Button variant="primary" type="submit" disabled={!cwd().trim()}>创建</Button></div>
        </form>
      </Dialog>
      <div class="project-scroll">
        <Show when={projects().length} fallback={<p class="sidebar-empty">创建一个项目，然后从右侧 + 开始新会话。</p>}>
          <For each={projects().filter((p) => !p.archivedAt)}>{(project) => {
            const sessions = () => projectSessions().filter((s) => s.projectId === project.id);
            return <section class="project-group">
              <div class="project-heading">
                <div class="project-name" title={project.cwd}>{project.name}</div>
                <IconButton label={`向 ${project.name} 导入会话`} disabled={readOnly()} onClick={() => setImportingProject(project.id)}><ImportIcon /></IconButton>
                <IconButton label={`在 ${project.name} 新建会话`} disabled={readOnly()} onClick={() => createProjectSession(project.id)}><PlusIcon /></IconButton>
              </div>
              <div class="session-list">
                <For each={sessions()} fallback={<button disabled={readOnly()} class="session-empty" onClick={() => createProjectSession(project.id)}>开始第一次对话</button>}>
                  {(session) => <div class={`session-row ${selectedSessionId() === session.id ? 'is-selected' : ''}`}>
                    <button class="session-main" title={readOnly() && !session.activeChatId ? '需要 full 权限才能启动此会话' : undefined} onClick={() => { if (readOnly() && session.activeChatId) selectPersistedSessionLocally(session.id, session.activeChatId); else openProjectSession(session.id); props.onNavigate?.(); }} disabled={(readOnly() && !session.activeChatId) || session.lifecycle !== 'ready' || !!openingSessionId()}>
                      <ChatIcon /><span>{session.title}</span>
                      <Show when={openingSessionId() === session.id || ['activating','pending'].includes(session.lifecycle)}><Spinner label="正在打开" /></Show>
                    </button>
                    <button class="session-menu" disabled={readOnly()} aria-label={`重命名 ${session.title}`} onClick={() => { setEditing(session.id); setDraft(session.title); }}>•••</button>
                    <Menu open={editing() === session.id}>
                      <form class="rename-popover" onSubmit={(e) => { e.preventDefault(); renameProjectSession(session.id, draft()); setEditing(null); }}>
                        <TextField aria-label="会话名称" value={draft()} onInput={(e) => setDraft(e.currentTarget.value)} autofocus />
                        <Show when={!renameValid()}><div class="ui-error">名称不能为空</div></Show>
                        <div class="form-actions"><Button type="button" onClick={() => setEditing(null)}>取消</Button><Button variant="primary" type="submit" disabled={!renameValid()}>保存</Button></div>
                      </form>
                    </Menu>
                    <Show when={session.lifecycle === 'failed'}><div class="session-problem">打开失败 · <button disabled={readOnly()} onClick={() => createProjectSession(project.id, session.title)}>新建替代会话</button></div></Show>
                    <Show when={session.lifecycle === 'reconciliation_required'}><div class="session-problem session-problem--warn">需要服务端人工对账，暂不可重试</div></Show>
                  </div>}
                </For>
              </div>
            </section>;
          }}</For>
        </Show>
      </div>
      <Dialog open={!!importingProject()} title="导入 ACP 会话" onClose={() => setImportingProject(null)}>
        {(() => {
          const project = () => projects().find((item) => item.id === importingProject());
          const candidates = () => importCandidates(importableSessions(), project()?.cwd || '');
          return <div class="import-dialog">
            <div class="import-dialog__header"><h2>导入会话</h2><p>只显示当前项目目录中、尚未加入侧边栏的 ACP 会话。</p></div>
            <div class="import-session-list">
              <For each={candidates()} fallback={<div class="import-empty">没有可导入的会话</div>}>
                {(candidate) => <button class="import-session-row" onClick={() => { const id = importingProject(); if (id) importProjectSession(id, candidate.sessionId); setImportingProject(null); }}>
                  <ChatIcon /><span><strong>{candidate.title || '未命名会话'}</strong><small>{candidate.updatedAt || '时间未知'}</small></span><ImportIcon />
                </button>}
              </For>
            </div>
            <div class="form-actions"><Button onClick={() => setImportingProject(null)}>完成</Button></div>
          </div>;
        })()}
      </Dialog>
      <div class="sidebar-footer"><span class="status-dot" />本地安全连接</div>
    </nav>
  );
}
