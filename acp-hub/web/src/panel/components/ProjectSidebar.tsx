import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { archiveProject, archiveProjectSession, chatStatusSignal, connState, createProject, createProjectSession, creatingSessionProjectId, discoverProjectSessions, discoveringSessionsProjectId, importableSessions, importProjectSession, isTerminal, navigateProjectSession, openingSessionId, permissions, projects, projectSessions, renameProject, renameProjectSession, restoreProject, restoreProjectSession, runtimeDocsHydrated, selectedCid, selectedSessionId, turnActive } from '../store';
import { readOnly } from '../lib/auth-state';
import { Button, Dialog, Icon, IconButton, Menu, MenuItem, primaryShortcut, Status, TextField } from '../../ui';
import { useAuthActions } from './AuthGate';
import { SessionSearch } from './SessionSearch';
import { SessionImportDialog } from './SessionImportDialog';
import { ProjectSessionRow } from './ProjectSessionRow';
import { runConfirmedMutation } from '../lib/form-mutation';
import { runtimeState } from '../lib/runtime-state.mjs';
import { sessionDisplayTitle } from '../lib/recovery-state.mjs';

function PlusIcon() { return <Icon><path d="M10 4v12M4 10h12" /></Icon>; }
function ImportIcon() { return <Icon><path d="M10 3v9m0 0 3-3m-3 3L7 9M4 14.5h12v2H4z" /></Icon>; }
function ChevronIcon() { return <Icon size="small"><path d="m7 5 5 5-5 5" /></Icon>; }
function MoreIcon() { return <Icon><circle cx="4" cy="10" r="1" /><circle cx="10" cy="10" r="1" /><circle cx="16" cy="10" r="1" /></Icon>; }
function SearchIcon() { return <Icon><circle cx="8.5" cy="8.5" r="5" /><path d="m12.2 12.2 4 4" /></Icon>; }

export function ProjectSidebar(props: { onNavigate?: () => void; intent?: { kind: 'create-project' | 'import'; projectId?: string; nonce: number } | null }) {
  const [creating, setCreating] = createSignal(false);
  const [projectCreateSubmitting, setProjectCreateSubmitting] = createSignal(false);
  const [name, setName] = createSignal('');
  const [cwd, setCwd] = createSignal('');
  const [editing, setEditing] = createSignal<string | null>(null);
  const [sessionMenu, setSessionMenu] = createSignal<string | null>(null);
  const [archiveSessionCandidate, setArchiveSessionCandidate] = createSignal<string | null>(null);
  const [sessionLifecycleBusy, setSessionLifecycleBusy] = createSignal<string | null>(null);
  const [archivedSessionsOpen, setArchivedSessionsOpen] = createSignal(new Set<string>());
  const [importingProject, setImportingProject] = createSignal<string | null>(null);
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
  const sessionHasRunningRuntime = (session: { id: string; activeChatId?: string | null }) => {
    if (!session.activeChatId) return false;
    return session.id !== selectedSessionId() || !isTerminal(chatStatusSignal()[session.activeChatId]);
  };
  const projectHasRunningSession = (projectId: string) => projectSessions().some((session) => session.projectId === projectId && sessionHasRunningRuntime(session));
  const toggleProject = (projectId: string) => setCollapsedProjects((current) => {
    const next = new Set(current);
    if (next.has(projectId)) next.delete(projectId); else next.add(projectId);
    return next;
  });
  const toggleArchivedSessions = (projectId: string) => setArchivedSessionsOpen((current) => {
    const next = new Set(current);
    if (next.has(projectId)) next.delete(projectId); else next.add(projectId);
    return next;
  });

  createEffect(() => {
    const intent = props.intent;
    if (!intent) return;
    if (intent.kind === 'create-project') setCreating(true);
    if (intent.kind === 'import' && intent.projectId) {
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
            const sessions = () => projectSessions().filter((s) => s.projectId === project.id && !s.archivedAt);
            const archivedSessions = () => projectSessions().filter((s) => s.projectId === project.id && !!s.archivedAt);
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
                  <MenuItem onClick={() => { setProjectMenu(null); setImportingProject(project.id); }}><ImportIcon />导入已有会话</MenuItem>
                  <MenuItem tone="danger" disabled={projectHasRunningSession(project.id)} title={projectHasRunningSession(project.id) ? '请先关闭此项目中正在运行的会话' : undefined} onClick={() => { setProjectMenu(null); setArchiveCandidate(project.id); }}>归档项目</MenuItem>
                </Menu>
              </div>
              <div id={`project-sessions-${project.id}`} class="session-list" hidden={collapsed()}>
                <For each={sessions()} fallback={<Button busy={creatingSessionProjectId() === project.id} disabled={readOnly() || !!creatingSessionProjectId()} class="session-empty" onClick={() => createProjectSession(project.id)}>开始第一次对话</Button>}>
                  {(session) => {
                    const selected = () => selectedSessionId() === session.id;
                    const state = () => runtimeState({
                      hasSession: true,
                      lifecycle: session.lifecycle,
                      isOpening: openingSessionId() === session.id,
                      hasRuntime: !!session.activeChatId,
                      isSelected: selected(),
                      isHydrated: selected() ? runtimeDocsHydrated() : undefined,
                      chatStatus: selected() ? chatStatusSignal()[selectedCid() ?? ''] : null,
                      hasPendingPermission: selected() && permissions().some((permission) => permission.status === 'pending'),
                      turnActive: selected() && turnActive(),
                    });
                    return <ProjectSessionRow
                      session={session}
                      state={state()}
                      selected={selected()}
                      opening={openingSessionId() === session.id}
                      navigationBusy={!!openingSessionId()}
                      readOnly={readOnly()}
                      renameOpen={editing() === session.id}
                      menuOpen={sessionMenu() === session.id}
                      runtimeActive={sessionHasRunningRuntime(session)}
                      replacementBusy={creatingSessionProjectId() === project.id}
                      onNavigate={() => props.onNavigate?.()}
                      onOpen={(sessionId, onCommitted) => { navigateProjectSession(sessionId, { onCommitted }); }}
                      onSelectRuntime={(sessionId) => { navigateProjectSession(sessionId); }}
                      onRenameOpenChange={(open) => setEditing(open ? session.id : null)}
                      onMenuOpenChange={(open) => setSessionMenu(open ? session.id : null)}
                      onRename={renameProjectSession}
                      onCreateReplacement={(title) => { createProjectSession(project.id, title); }}
                      onArchiveRequest={setArchiveSessionCandidate}
                    />;
                  }}
                </For>
                <Show when={archivedSessions().length > 0}>
                  <button type="button" class="archived-sessions__toggle" aria-expanded={archivedSessionsOpen().has(project.id)} aria-controls={`archived-sessions-${project.id}`} onClick={() => toggleArchivedSessions(project.id)}><ChevronIcon /><span>已归档会话</span><small>{archivedSessions().length}</small></button>
                  <Show when={archivedSessionsOpen().has(project.id)}><div id={`archived-sessions-${project.id}`} class="archived-session-list"><For each={archivedSessions()}>{(session) => <div class="archived-session-row"><span><strong>{sessionDisplayTitle(session.title, session.acpSessionId || session.id)}</strong><small>{session.lifecycle === 'ready' ? '会话已保存' : session.lifecycle}</small></span><Button size="compact" busy={sessionLifecycleBusy() === session.id} disabled={readOnly() || !!sessionLifecycleBusy()} onClick={() => runConfirmedMutation(() => setSessionLifecycleBusy(session.id), () => setSessionLifecycleBusy(null), (committed, failed) => restoreProjectSession(session.id, committed, failed), () => {})}>恢复</Button></div>}</For></div></Show>
                </Show>
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
      <SessionImportDialog
        open={!!importingProject()}
        project={projects().find((item) => item.id === importingProject()) || null}
        sessions={importableSessions()}
        discovering={discoveringSessionsProjectId() === importingProject()}
        onDiscover={discoverProjectSessions}
        onClose={() => setImportingProject(null)}
        onImport={importProjectSession}
      />
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
      <Dialog open={!!archiveSessionCandidate()} title="归档会话" dismissible={!sessionLifecycleBusy()} onClose={() => setArchiveSessionCandidate(null)}>
        {(() => {
          const session = () => projectSessions().find((item) => item.id === archiveSessionCandidate());
          const displayTitle = () => sessionDisplayTitle(session()?.title, session()?.acpSessionId || session()?.id);
          const running = () => !!session() && sessionHasRunningRuntime(session()!);
          return <div class="runtime-dialog"><span class="dialog-eyebrow">会话整理</span><h2>归档“{displayTitle()}”？</h2><p>会话会从当前项目列表隐藏，但 ACP thread、消息历史和本地项目文件都不会删除。之后可以在“已归档会话”中恢复。</p><Show when={running()}><p class="runtime-dialog__warning">此会话仍有运行实例。请先在会话顶部菜单中关闭运行实例。</p></Show><div class="form-actions"><Button disabled={!!sessionLifecycleBusy()} onClick={() => setArchiveSessionCandidate(null)}>取消</Button><Button variant="danger" busy={!!sessionLifecycleBusy()} disabled={running()} onClick={() => { const id = archiveSessionCandidate(); if (!id) return; runConfirmedMutation(() => setSessionLifecycleBusy(id), () => setSessionLifecycleBusy(null), (committed, failed) => archiveProjectSession(id, committed, failed), () => setArchiveSessionCandidate(null)); }}>归档会话</Button></div></div>;
        })()}
      </Dialog>
      <div class="sidebar-footer"><Status tone={connState().kind || 'idle'}>{connState().text}</Status><Button size="compact" onClick={auth?.logout}>退出登录</Button></div>
    </nav>
  );
}
