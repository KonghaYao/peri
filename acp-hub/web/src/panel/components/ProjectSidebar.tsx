import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { archiveProject, chatStatusSignal, connState, createProject, createProjectSession, creatingSessionProjectId, importableSessions, importProjectSession, openProjectSession, openingSessionId, permissions, projects, projectSessions, readOnly, renameProject, renameProjectSession, restoreProject, selectedCid, selectedSessionId, selectPersistedSessionLocally, turnActive } from '../store';
import { Button, Dialog, IconButton, Menu, MenuItem, Popover, primaryShortcut, Spinner, Status, TextField } from '../../ui';
import { importCandidates } from '../lib/session-import.mjs';
import { useAuthActions } from './AuthGate';
import { cleanSessionTitle, formatRelativeTime, sessionDisplayTitle, shortSessionId } from '../lib/recovery-state.mjs';
import { SessionSearch } from './SessionSearch';
import { runConfirmedMutation } from '../lib/form-mutation';
import { runtimeState } from '../lib/runtime-state.mjs';

function PlusIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 4v12M4 10h12" /></svg>; }
function ChatIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4 4.5h12v9H8l-4 3v-12Z" /></svg>; }
function ImportIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3v9m0 0 3-3m-3 3L7 9M4 14.5h12v2H4z" /></svg>; }
function ChevronIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m7 5 5 5-5 5" /></svg>; }
function MoreIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="4" cy="10" r="1" /><circle cx="10" cy="10" r="1" /><circle cx="16" cy="10" r="1" /></svg>; }
function SearchIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.5" cy="8.5" r="5" /><path d="m12.2 12.2 4 4" /></svg>; }
function EditIcon() { return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m4 14.8.7-3.4 7.8-7.8a1.5 1.5 0 0 1 2.1 0l1.8 1.8a1.5 1.5 0 0 1 0 2.1l-7.8 7.8-3.4.7Z" /><path d="m11.3 4.8 3.9 3.9" /></svg>; }

export function ProjectSidebar(props: { onNavigate?: () => void; intent?: { kind: 'create-project' | 'import'; projectId?: string; nonce: number } | null }) {
  const [creating, setCreating] = createSignal(false);
  const [projectCreateSubmitting, setProjectCreateSubmitting] = createSignal(false);
  const [name, setName] = createSignal('');
  const [cwd, setCwd] = createSignal('');
  const [editing, setEditing] = createSignal<string | null>(null);
  const [sessionRenameSubmitting, setSessionRenameSubmitting] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal('');
  const [importingProject, setImportingProject] = createSignal<string | null>(null);
  const [importCandidateId, setImportCandidateId] = createSignal<string | null>(null);
  const [importSubmitting, setImportSubmitting] = createSignal(false);
  const [importQuery, setImportQuery] = createSignal('');
  const [collapsedProjects, setCollapsedProjects] = createSignal(new Set<string>());
  const [projectMenu, setProjectMenu] = createSignal<string | null>(null);
  const [archiveCandidate, setArchiveCandidate] = createSignal<string | null>(null);
  const [archiveSubmitting, setArchiveSubmitting] = createSignal(false);
  const [archivedOpen, setArchivedOpen] = createSignal(false);
  const [restoringProject, setRestoringProject] = createSignal<string | null>(null);
  const [renamingProject, setRenamingProject] = createSignal<string | null>(null);
  const [projectNameDraft, setProjectNameDraft] = createSignal('');
  const [projectRenameSubmitting, setProjectRenameSubmitting] = createSignal(false);
  const [searchOpen, setSearchOpen] = createSignal(false);
  const auth = useAuthActions();
  const renameValid = () => !!draft().trim();
  const projectHasRunningSession = (projectId: string) => projectSessions().some((session) => session.projectId === projectId && !!session.activeChatId);
  const toggleProject = (projectId: string) => setCollapsedProjects((current) => {
    const next = new Set(current);
    if (next.has(projectId)) next.delete(projectId); else next.add(projectId);
    return next;
  });

  createEffect(() => {
    const intent = props.intent;
    if (!intent) return;
    if (intent.kind === 'create-project') setCreating(true);
    if (intent.kind === 'import' && intent.projectId) {
      setImportCandidateId(null);
      setImportQuery('');
      setImportingProject(intent.projectId);
    }
  });

  onMount(() => {
    const shortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === 'k') {
        event.preventDefault();
        setSearchOpen((open) => !open);
      }
    };
    document.addEventListener('keydown', shortcut);
    onCleanup(() => document.removeEventListener('keydown', shortcut));
  });

  const submitProject = (e: SubmitEvent) => {
    e.preventDefault();
    if (!cwd().trim() || projectCreateSubmitting()) return;
    runConfirmedMutation(
      () => setProjectCreateSubmitting(true),
      () => setProjectCreateSubmitting(false),
      (committed, failed) => createProject(name().trim() || cwd().split('/').filter(Boolean).at(-1) || 'Project', cwd().trim(), committed, failed),
      () => { setName(''); setCwd(''); setCreating(false); },
    );
  };

  return (
    <nav class="project-sidebar" aria-label="项目与会话">
      <div class="brand-row"><span class="brand-glyph">✦</span><span>acp-hub</span></div>
      <Button class="new-project-button" disabled={readOnly()} onClick={() => setCreating(true)}><PlusIcon />新建项目</Button>
      <Button class="session-search-button" onClick={() => setSearchOpen(true)}><SearchIcon />搜索会话<kbd>{primaryShortcut('K')}</kbd></Button>
      <SessionSearch open={searchOpen()} onClose={() => setSearchOpen(false)} onSelected={props.onNavigate} />
      <Show when={readOnly()}><div class="readonly-label">只读模式</div></Show>
      <Dialog open={creating()} title="新建项目" dismissible={!projectCreateSubmitting()} onClose={() => setCreating(false)}>
        <form class="project-form" onSubmit={submitProject}>
          <TextField label="项目名称" value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="perihelion" autofocus />
          <TextField label="工作目录" value={cwd()} onInput={(e) => setCwd(e.currentTarget.value)} placeholder="/absolute/path" />
          <div class="form-actions"><Button type="button" disabled={projectCreateSubmitting()} onClick={() => setCreating(false)}>取消</Button><Button variant="primary" type="submit" busy={projectCreateSubmitting()} disabled={!cwd().trim()}>创建</Button></div>
        </form>
      </Dialog>
      <div class="project-scroll">
        <Show when={projects().length} fallback={<p class="sidebar-empty">创建一个项目，然后从右侧 + 开始新会话。</p>}>
          <For each={projects().filter((p) => !p.archivedAt)}>{(project) => {
            const sessions = () => projectSessions().filter((s) => s.projectId === project.id);
            const collapsed = () => collapsedProjects().has(project.id);
            const projectMenuId = `project-menu-${project.id}`;
            let projectMenuTrigger: HTMLButtonElement | undefined;
            return <section class="project-group">
              <div class="project-heading">
                <button type="button" class={`project-disclosure ${collapsed() ? '' : 'is-expanded'}`} aria-expanded={!collapsed()} aria-controls={`project-sessions-${project.id}`} onClick={() => toggleProject(project.id)}><ChevronIcon /><span><strong>{project.name}</strong><small>{sessions().length ? `${sessions().length} 个会话` : '还没有会话'}</small></span></button>
                <IconButton tooltipPlacement="end" label={`在 ${project.name} 新建会话`} busy={creatingSessionProjectId() === project.id} disabled={readOnly() || !!creatingSessionProjectId()} onClick={() => createProjectSession(project.id)}><PlusIcon /></IconButton>
                <IconButton tooltipPlacement="end" ref={projectMenuTrigger} label={`${project.name} 项目操作`} disabled={readOnly()} aria-haspopup="menu" aria-expanded={projectMenu() === project.id} aria-controls={projectMenu() === project.id ? projectMenuId : undefined} onClick={() => setProjectMenu((open) => open === project.id ? null : project.id)}><MoreIcon /></IconButton>
                <Menu open={projectMenu() === project.id} id={projectMenuId} label={`${project.name} 项目操作`} trigger={() => projectMenuTrigger} onClose={() => setProjectMenu(null)}>
                  <MenuItem onClick={() => { setProjectMenu(null); setProjectNameDraft(project.name); setRenamingProject(project.id); }}>重命名项目</MenuItem>
                  <MenuItem onClick={() => { setProjectMenu(null); setImportCandidateId(null); setImportQuery(''); setImportingProject(project.id); }}><ImportIcon />导入已有会话</MenuItem>
                  <MenuItem tone="danger" disabled={projectHasRunningSession(project.id)} title={projectHasRunningSession(project.id) ? '请先关闭此项目中正在运行的会话' : undefined} onClick={() => { setProjectMenu(null); setArchiveCandidate(project.id); }}>归档项目</MenuItem>
                </Menu>
              </div>
              <div id={`project-sessions-${project.id}`} class="session-list" hidden={collapsed()}>
                <For each={sessions()} fallback={<Button busy={creatingSessionProjectId() === project.id} disabled={readOnly() || !!creatingSessionProjectId()} class="session-empty" onClick={() => createProjectSession(project.id)}>开始第一次对话</Button>}>
                  {(session) => {
                    let renameTrigger: HTMLButtonElement | undefined;
                    const renameId = `rename-session-${session.id}`;
                    const selected = () => selectedSessionId() === session.id;
                    const state = () => runtimeState({
                      hasSession: true,
                      lifecycle: session.lifecycle,
                      isOpening: openingSessionId() === session.id,
                      hasRuntime: !!session.activeChatId,
                      chatStatus: selected() ? chatStatusSignal()[selectedCid() ?? ''] : null,
                      hasPendingPermission: selected() && permissions().some((permission) => permission.status === 'pending'),
                      turnActive: selected() && turnActive(),
                    });
                    return <div class={`session-row ${selectedSessionId() === session.id ? 'is-selected' : ''}`}>
                    <button type="button" class="session-main" aria-current={selectedSessionId() === session.id ? 'page' : undefined} title={readOnly() && !session.activeChatId ? '需要 full 权限才能启动此会话' : undefined} onClick={() => { if (readOnly() && session.activeChatId) { selectPersistedSessionLocally(session.id, session.activeChatId); props.onNavigate?.(); } else openProjectSession(session.id, { onCommitted: props.onNavigate }); }} disabled={(readOnly() && !session.activeChatId) || session.lifecycle !== 'ready' || !!openingSessionId()}>
                      <ChatIcon /><span class="session-copy"><strong>{sessionDisplayTitle(session.title, session.acpSessionId || session.id)}</strong><small class={`session-state session-state--${state().tone}`}>{state().label} · {formatRelativeTime(session.lastOpenedAt || session.updatedAt)}</small></span>
                      <Show when={openingSessionId() === session.id || ['activating','pending'].includes(session.lifecycle)}><Spinner label="正在打开" /></Show>
                    </button>
                    <IconButton tooltipPlacement="end" ref={renameTrigger} class="session-menu" disabled={readOnly() || !!sessionRenameSubmitting()} label={`重命名 ${sessionDisplayTitle(session.title, session.acpSessionId || session.id)}`} aria-haspopup="dialog" aria-expanded={editing() === session.id} aria-controls={editing() === session.id ? renameId : undefined} onClick={() => { setEditing(session.id); setDraft(session.title); }}><EditIcon /></IconButton>
                    <Popover open={editing() === session.id} id={renameId} label={`重命名 ${session.title}`} trigger={() => renameTrigger} dismissible={sessionRenameSubmitting() !== session.id} onClose={() => setEditing(null)}>
                      <form class="rename-popover" onSubmit={(e) => { e.preventDefault(); if (!renameValid() || sessionRenameSubmitting()) return; runConfirmedMutation(() => setSessionRenameSubmitting(session.id), () => setSessionRenameSubmitting(null), (committed, failed) => renameProjectSession(session.id, draft(), committed, failed), () => setEditing(null)); }}>
                        <TextField aria-label="会话名称" value={draft()} error={!renameValid() ? '名称不能为空' : undefined} onInput={(e) => setDraft(e.currentTarget.value)} autofocus />
                        <div class="form-actions"><Button type="button" disabled={sessionRenameSubmitting() === session.id} onClick={() => setEditing(null)}>取消</Button><Button variant="primary" type="submit" busy={sessionRenameSubmitting() === session.id} disabled={!renameValid()}>保存</Button></div>
                      </form>
                    </Popover>
                    <Show when={session.lifecycle === 'failed'}><div class="session-problem">打开失败 · <Button size="compact" busy={creatingSessionProjectId() === project.id} disabled={readOnly() || !!creatingSessionProjectId()} onClick={() => createProjectSession(project.id, session.title)}>新建替代会话</Button></div></Show>
                    <Show when={session.lifecycle === 'reconciliation_required'}><div class="session-problem session-problem--warn">需要服务端人工对账，暂不可重试</div></Show>
                  </div>;
                  }}
                </For>
              </div>
            </section>;
          }}</For>
        </Show>
      </div>
      <Show when={projects().some((project) => !!project.archivedAt)}>
        <section class="archived-projects">
          <button type="button" class="archived-projects__toggle" aria-expanded={archivedOpen()} aria-controls="archived-project-list" onClick={() => setArchivedOpen((open) => !open)}><ChevronIcon /><span>已归档</span><small>{projects().filter((project) => !!project.archivedAt).length}</small></button>
          <Show when={archivedOpen()}><div id="archived-project-list" class="archived-project-list"><For each={projects().filter((project) => !!project.archivedAt)}>{(project) => <div class="archived-project-row"><span><strong>{project.name}</strong><small>{projectSessions().filter((session) => session.projectId === project.id).length} 个会话</small></span><Button busy={restoringProject() === project.id} disabled={readOnly() || !!restoringProject()} onClick={() => runConfirmedMutation(() => setRestoringProject(project.id), () => setRestoringProject(null), (committed, failed) => restoreProject(project.id, committed, failed), () => {})}>恢复</Button></div>}</For></div></Show>
        </section>
      </Show>
      <Dialog open={!!importingProject()} title="导入 ACP 会话" dismissible={!importSubmitting()} onClose={() => setImportingProject(null)}>
        {(() => {
          const project = () => projects().find((item) => item.id === importingProject());
          const candidates = () => {
            const all = importCandidates(importableSessions(), project()?.cwd || '');
            const query = importQuery().trim().toLocaleLowerCase();
            if (!query) return all;
            return all.filter((candidate) => cleanSessionTitle(candidate.title).toLocaleLowerCase().includes(query) || candidate.sessionId.toLocaleLowerCase().includes(query));
          };
          return <div class="import-dialog">
            <div class="import-dialog__header"><span class="dialog-eyebrow">{project()?.name}</span><h2>导入会话</h2><p>只显示此项目目录中、尚未加入侧边栏的 ACP 会话。导入不会复制或移动原会话。</p></div>
            <TextField label="搜索会话" value={importQuery()} onInput={(event) => setImportQuery(event.currentTarget.value)} placeholder="按标题或会话 ID 搜索" />
            <div class="import-session-list">
              <For each={candidates()} fallback={<div class="import-empty">没有可导入的会话</div>}>
                {(candidate) => <button type="button" disabled={importSubmitting()} class={`import-session-row ${importCandidateId() === candidate.sessionId ? 'is-selected' : ''}`} aria-pressed={importCandidateId() === candidate.sessionId} onClick={() => setImportCandidateId(candidate.sessionId)}>
                  <ChatIcon /><span><strong>{cleanSessionTitle(candidate.title)}</strong><small>{formatRelativeTime(candidate.updatedAt)} · ID …{shortSessionId(candidate.sessionId)}</small></span><Show when={importCandidateId() === candidate.sessionId}><span class="import-session-check" aria-hidden="true">✓</span></Show>
                </button>}
              </For>
            </div>
            <div class="form-actions"><Button disabled={importSubmitting()} onClick={() => setImportingProject(null)}>取消</Button><Button variant="primary" busy={importSubmitting()} disabled={!importCandidateId()} onClick={() => { const projectId = importingProject(); const sessionId = importCandidateId(); if (!projectId || !sessionId) return; setImportSubmitting(true); const sent = importProjectSession(projectId, sessionId, () => { setImportSubmitting(false); setImportingProject(null); setImportCandidateId(null); }, () => setImportSubmitting(false)); if (!sent) setImportSubmitting(false); }}>导入所选会话</Button></div>
          </div>;
        })()}
      </Dialog>
      <Dialog open={!!renamingProject()} title="重命名项目" dismissible={!projectRenameSubmitting()} onClose={() => setRenamingProject(null)}>
        <form class="project-form" onSubmit={(event) => { event.preventDefault(); const id = renamingProject(); if (!id || !projectNameDraft().trim()) return; runConfirmedMutation(() => setProjectRenameSubmitting(true), () => setProjectRenameSubmitting(false), (committed, failed) => renameProject(id, projectNameDraft(), committed, failed), () => setRenamingProject(null)); }}>
          <TextField label="项目名称" value={projectNameDraft()} onInput={(event) => setProjectNameDraft(event.currentTarget.value)} autofocus />
          <p class="form-note">只修改侧边栏名称，不会改变工作目录或 ACP 会话。</p>
          <div class="form-actions"><Button disabled={projectRenameSubmitting()} onClick={() => setRenamingProject(null)}>取消</Button><Button variant="primary" type="submit" busy={projectRenameSubmitting()} disabled={!projectNameDraft().trim()}>保存</Button></div>
        </form>
      </Dialog>
      <Dialog open={!!archiveCandidate()} title="归档项目" dismissible={!archiveSubmitting()} onClose={() => setArchiveCandidate(null)}>
        {(() => {
          const project = () => projects().find((item) => item.id === archiveCandidate());
          const count = () => projectSessions().filter((session) => session.projectId === archiveCandidate()).length;
          const running = () => !!archiveCandidate() && projectHasRunningSession(archiveCandidate()!);
          return <div class="runtime-dialog"><span class="dialog-eyebrow">项目管理</span><h2>归档“{project()?.name}”？</h2><p>项目会从侧边栏隐藏，{count()} 个已保存会话不会被删除。此操作不会删除工作目录中的任何文件。</p><Show when={running()}><p class="runtime-dialog__warning">此项目仍有运行实例。请先在对应会话的菜单中关闭运行实例，再归档项目。</p></Show><div class="form-actions"><Button disabled={archiveSubmitting()} onClick={() => setArchiveCandidate(null)}>取消</Button><Button variant="danger" busy={archiveSubmitting()} disabled={running()} onClick={() => { const id = archiveCandidate(); if (!id) return; runConfirmedMutation(() => setArchiveSubmitting(true), () => setArchiveSubmitting(false), (committed, failed) => archiveProject(id, committed, failed), () => setArchiveCandidate(null)); }}>归档项目</Button></div></div>;
        })()}
      </Dialog>
      <div class="sidebar-footer"><Status tone={connState().kind || 'idle'}>{connState().text}</Status><Button size="compact" onClick={auth?.logout}>退出登录</Button></div>
    </nav>
  );
}
