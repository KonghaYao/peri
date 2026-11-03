import { useMemo, useRef, useState, type FormEvent } from 'react';
import { ChevronRight, Folder, PanelLeftClose, Plus, RefreshCw, Search, Sparkles, X } from 'lucide-react';
import { cn } from '@/lib';
import { useFocusTrap, useMediaQuery } from '@/hooks';
import type { ChatSummary, ProjectSummary } from '../runtime';
import { SidebarThreadRow } from './SidebarThreadRow';

/** 与 Tailwind `md` 断点互补：小于 768px 时侧栏是 drawer。 */
const SIDEBAR_SHEET_MEDIA_QUERY = '(max-width: 767px)';
const PROJECT_NAME_MAX_LENGTH = 64;

const FALLBACK_PROJECT: ProjectSummary = {
  id: 'default',
  name: '默认项目',
  createdAt: '',
  isDefault: true,
};

export interface SidebarProps {
  chats: readonly ChatSummary[];
  projects: readonly ProjectSummary[];
  selectedId?: string | undefined;
  selectedRunning: boolean;
  loading: boolean;
  projectsLoading: boolean;
  disabled: boolean;
  open: boolean;
  onClose: () => void;
  onNew: () => void;
  onNewInProject: (projectId: string) => void;
  onCreateProject: (name: string) => Promise<unknown>;
  onRefresh: () => void;
  onSelect: (chat: ChatSummary) => void;
}

function SidebarSkeleton() {
  return <div className="flex flex-col gap-1.5 px-1 pt-1" aria-hidden="true">
    {[0, 1, 2, 3, 4].map((index) => <div key={index} className="bg-muted/70 h-8 animate-pulse rounded-lg" />)}
  </div>;
}

/**
 * 左侧导航：项目 → 会话。
 *
 * 项目来自 `/api/projects`，会话归属由服务端按 cwd 推导。项目只做展示分组，
 * 折叠状态是本地 UI 状态；项目内新建会话由父层携带 `projectId` 发起。
 */
export function Sidebar({
  chats, projects, selectedId, selectedRunning, loading, projectsLoading, disabled,
  open, onClose, onNew, onNewInProject, onCreateProject, onRefresh, onSelect,
}: SidebarProps) {
  const [query, setQuery] = useState('');
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const [creating, setCreating] = useState(false);
  const [projectName, setProjectName] = useState('');
  const [createError, setCreateError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const asideRef = useRef<HTMLElement>(null);
  const sheet = useMediaQuery(SIDEBAR_SHEET_MEDIA_QUERY);
  useFocusTrap(open && sheet, asideRef);

  const filtered = useMemo(() => {
    const keyword = query.trim().toLowerCase();
    if (!keyword) return chats;
    return chats.filter((chat) => (chat.title || '新会话').toLowerCase().includes(keyword));
  }, [chats, query]);

  /**
   * 项目清单缺失时仍展示会话：未知归属回落到默认项目，避免网络失败吞掉列表。
   * 默认项目排在最后，新建项目优先展示。
   */
  const sections = useMemo(() => {
    const list = projects.length > 0 ? projects : [FALLBACK_PROJECT];
    const fallback = list.find((project) => project.isDefault)?.id ?? list[0]!.id;
    const byProject = new Map<string, ChatSummary[]>();
    for (const project of list) byProject.set(project.id, []);
    for (const chat of filtered) {
      const id = byProject.has(chat.projectId) ? chat.projectId : fallback;
      byProject.get(id)!.push(chat);
    }
    const ordered = [...list].sort((left, right) => Number(left.isDefault) - Number(right.isDefault));
    return ordered.map((project) => ({ project, chats: byProject.get(project.id) ?? [] }));
  }, [projects, filtered]);

  const searching = query.trim().length > 0;
  const visibleSections = searching
    ? sections.filter((section) => section.chats.length > 0)
    : sections;

  const toggleProject = (id: string) => {
    setCollapsed((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const cancelCreate = () => {
    setCreating(false);
    setProjectName('');
    setCreateError('');
  };

  const submitProject = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const name = projectName.trim();
    if (!name || submitting) return;
    setSubmitting(true);
    setCreateError('');
    try {
      await onCreateProject(name);
      cancelCreate();
    } catch (error) {
      setCreateError(error instanceof Error ? error.message : '项目创建失败。');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <>
      {open && <div className="fixed inset-0 z-30 bg-black/30 backdrop-blur-sm md:hidden" onClick={onClose} aria-hidden="true" />}
      <aside
        ref={asideRef}
        id="peri-sidebar"
        data-slot="peri-sidebar"
        role={sheet ? 'dialog' : 'complementary'}
        aria-modal={sheet && open ? true : undefined}
        aria-label="会话导航"
        aria-hidden={sheet && !open ? true : undefined}
        inert={sheet && !open ? true : undefined}
        className={cn(
          'bg-sidebar text-sidebar-foreground fixed inset-y-0 left-0 z-40 flex w-[min(290px,85vw)] shrink-0 flex-col border-r shadow-xl transition-transform duration-200 motion-reduce:transition-none',
          'md:static md:z-auto md:w-[272px] md:translate-x-0 md:shadow-none',
          open ? 'visible translate-x-0' : 'invisible -translate-x-full md:visible',
        )}
      >
        <div className="flex items-center gap-2 px-4 pt-4 pb-3">
          <span className="bg-primary text-primary-foreground grid size-8 place-items-center rounded-xl">
            <Sparkles size={18} aria-hidden="true" />
          </span>
          <span className="text-[22px] leading-none font-semibold tracking-tight">
            peri<span className="text-[var(--green)]">.</span>
          </span>
          <button
            type="button"
            className="text-muted-foreground hover:bg-accent hover:text-foreground ml-auto inline-flex size-8 items-center justify-center rounded-lg md:hidden"
            aria-label="关闭会话列表"
            onClick={onClose}
          >
            <PanelLeftClose size={18} />
          </button>
        </div>

        <div className="px-3">
          <button
            type="button"
            disabled={disabled}
            onClick={onNew}
            className="bg-foreground text-background hover:bg-foreground/85 flex w-full items-center gap-2 rounded-xl px-3 py-2 text-[13px] font-medium transition-colors disabled:opacity-45"
          >
            <Plus size={16} aria-hidden="true" />
            新建会话
          </button>
        </div>

        <label className="bg-background/70 focus-within:ring-ring/40 mx-3 mt-3 flex items-center gap-2 rounded-lg border px-2.5 py-1.5 focus-within:ring-2">
          <Search size={14} className="text-muted-foreground shrink-0" aria-hidden="true" />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索会话"
            aria-label="搜索会话"
            className="text-foreground placeholder:text-muted-foreground/70 min-w-0 flex-1 bg-transparent text-xs outline-none"
          />
        </label>

        <div className="text-muted-foreground/70 flex items-center justify-between px-4 pt-4 pb-1 text-[10px] tracking-[0.12em] uppercase">
          <span>项目</span>
          <div className="flex items-center gap-1">
            <button
              type="button"
              className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-6 items-center justify-center rounded-md"
              aria-label="新建项目"
              disabled={disabled}
              onClick={() => {
                setCreating((value) => !value);
                setCreateError('');
              }}
            >
              <Plus size={13} />
            </button>
            <button
              type="button"
              className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-6 items-center justify-center rounded-md"
              aria-label="刷新会话列表"
              disabled={loading}
              onClick={onRefresh}
            >
              <RefreshCw size={13} className={loading || projectsLoading ? 'animate-spin motion-reduce:animate-none' : undefined} />
            </button>
          </div>
        </div>

        {creating && (
          <form onSubmit={submitProject} className="mx-3 mb-1 flex flex-col gap-1" aria-label="新建项目表单">
            <div className="flex items-center gap-1">
              <input
                autoFocus
                value={projectName}
                onChange={(event) => setProjectName(event.target.value)}
                placeholder="项目名称"
                aria-label="项目名称"
                maxLength={PROJECT_NAME_MAX_LENGTH}
                className="bg-background/70 focus-visible:ring-ring/40 min-w-0 flex-1 rounded-lg border px-2 py-1 text-xs outline-none focus-visible:ring-2"
              />
              <button
                type="submit"
                disabled={submitting || projectName.trim().length === 0}
                aria-label="创建项目"
                className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-6 items-center justify-center rounded-md disabled:opacity-40"
              >
                <Plus size={13} />
              </button>
              <button
                type="button"
                onClick={cancelCreate}
                aria-label="取消新建项目"
                className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-6 items-center justify-center rounded-md"
              >
                <X size={13} />
              </button>
            </div>
            {createError && (
              <p role="alert" className="text-[11px] text-red-600 dark:text-red-400">{createError}</p>
            )}
          </form>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {loading && chats.length === 0 && projects.length === 0 ? (
            <SidebarSkeleton />
          ) : visibleSections.length === 0 ? (
            <p className="text-muted-foreground/70 px-2.5 py-8 text-center text-xs">
              没有匹配的会话。
            </p>
          ) : visibleSections.map(({ project, chats: rows }) => {
            const isCollapsed = collapsed.has(project.id);
            return (
              <section
                key={project.id}
                className="mb-1"
                aria-label={project.name}
                data-slot="peri-sidebar-project"
              >
                <div className="flex items-center gap-1 rounded-lg px-1.5 py-1.5 hover:bg-accent/40">
                  <button
                    type="button"
                    className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                    aria-expanded={!isCollapsed}
                    aria-label={`${project.name}，${rows.length} 个会话`}
                    onClick={() => toggleProject(project.id)}
                  >
                    <ChevronRight
                      size={13}
                      aria-hidden="true"
                      className={cn(
                        'text-muted-foreground/70 shrink-0 transition-transform motion-reduce:transition-none',
                        !isCollapsed && 'rotate-90',
                      )}
                    />
                    <Folder size={14} className="text-muted-foreground/80 shrink-0" aria-hidden="true" />
                    <span className="truncate text-[12px] font-medium">{project.name}</span>
                    <span className="text-muted-foreground/60 shrink-0 text-[10px]">{rows.length}</span>
                  </button>
                  {!project.isDefault && (
                    <button
                      type="button"
                      aria-label={`在 ${project.name} 中新建会话`}
                      title={`在 ${project.name} 中新建会话`}
                      disabled={disabled}
                      onClick={() => onNewInProject(project.id)}
                      className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-6 shrink-0 items-center justify-center rounded-md disabled:opacity-40"
                    >
                      <Plus size={13} />
                    </button>
                  )}
                </div>
                {!isCollapsed && (
                  rows.length === 0 ? (
                    <p className="text-muted-foreground/50 px-7 py-1.5 text-[11px]">还没有会话</p>
                  ) : (
                    <div className="flex flex-col gap-0.5">
                      {rows.map((chat) => (
                        <SidebarThreadRow
                          key={chat.sessionId}
                          chat={chat}
                          selected={chat.sessionId === selectedId}
                          running={chat.sessionId === selectedId ? selectedRunning : false}
                          disabled={disabled}
                          onSelect={() => onSelect(chat)}
                        />
                      ))}
                    </div>
                  )
                )}
              </section>
            );
          })}
        </div>
      </aside>
    </>
  );
}
