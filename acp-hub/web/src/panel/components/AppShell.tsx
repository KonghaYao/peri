import { createSignal, onCleanup, onMount } from 'solid-js';
import { ProjectSidebar } from './ProjectSidebar';
import { ChatView } from './ChatView';
import { compactViewportQuery, Drawer } from '../../ui';

export function AppShell() {
  const [open, setOpen] = createSignal(false);
  const [mobile, setMobile] = createSignal(false);
  const [sidebarIntent, setSidebarIntent] = createSignal<{ kind: 'create-project' | 'import'; projectId?: string; nonce: number } | null>(null);
  let drawer: HTMLElement | undefined;
  let main: HTMLElement | undefined;
  onMount(() => {
    const query = window.matchMedia(compactViewportQuery);
    const sync = () => {
      setMobile(query.matches);
      if (!query.matches) setOpen(false);
    };
    sync(); query.addEventListener('change', sync);
    onCleanup(() => query.removeEventListener('change', sync));
  });
  const openDrawer = () => {
    if (mobile()) setOpen(true);
    else queueMicrotask(() => drawer?.querySelector<HTMLElement>('.project-heading button:not(:disabled),.new-project-button:not(:disabled)')?.focus());
  };
  const requestSidebar = (kind: 'create-project' | 'import', projectId?: string) => {
    setSidebarIntent({ kind, projectId, nonce: Date.now() });
    if (mobile()) openDrawer();
  };
  return (
    <div class="app-shell">
      <Drawer ref={(element) => { drawer = element; }} class="project-drawer" open={open()} modal={mobile()} label="项目与会话导航" background={() => main} onClose={() => setOpen(false)}>
        <ProjectSidebar onNavigate={() => setOpen(false)} intent={sidebarIntent()} />
      </Drawer>
      <main ref={main} class="conversation-pane">
        <ChatView onOpenNavigation={openDrawer} onCreateProject={() => requestSidebar('create-project')} onImport={(projectId) => requestSidebar('import', projectId)} />
      </main>
    </div>
  );
}
