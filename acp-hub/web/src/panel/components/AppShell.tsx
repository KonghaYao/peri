import { createEffect, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { ProjectSidebar } from './ProjectSidebar';
import { ChatView } from './ChatView';

export function AppShell() {
  const [open, setOpen] = createSignal(false);
  const [mobile, setMobile] = createSignal(false);
  let drawer: HTMLElement | undefined;
  let trigger: HTMLElement | null = null;
  onMount(() => {
    const close = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
      if (e.key !== 'Tab' || !mobile() || !open() || !drawer) return;
      const items = [...drawer.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled),[tabindex]:not([tabindex="-1"])')];
      const edge = e.shiftKey ? items[0] : items.at(-1);
      if (items.length && document.activeElement === edge) { e.preventDefault(); (e.shiftKey ? items.at(-1) : items[0])?.focus(); }
    };
    document.addEventListener('keydown', close);
    onCleanup(() => document.removeEventListener('keydown', close));
    const query = window.matchMedia('(max-width: 767px)');
    const sync = () => setMobile(query.matches);
    sync(); query.addEventListener('change', sync);
    onCleanup(() => query.removeEventListener('change', sync));
  });
  createEffect(() => {
    if (!mobile()) return;
    if (open()) queueMicrotask(() => drawer?.querySelector<HTMLElement>('button:not(:disabled),input')?.focus());
    else if (trigger) { trigger.focus(); trigger = null; }
  });
  const openDrawer = () => { trigger = document.activeElement as HTMLElement | null; setOpen(true); };
  return (
    <div class="app-shell">
      <aside ref={drawer} class={`project-drawer ${open() ? 'is-open' : ''}`} inert={mobile() && !open()}>
        <ProjectSidebar onNavigate={() => setOpen(false)} />
      </aside>
      <main class="conversation-pane">
        <ChatView onOpenNavigation={openDrawer} />
      </main>
      <Show when={open()}><button class="drawer-scrim" aria-label="关闭导航" onClick={() => setOpen(false)} /></Show>
    </div>
  );
}
