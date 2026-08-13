import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { createSignal } from 'solid-js';
import { describe, expect, it, vi } from 'vitest';
import { Button, IconButton } from './Button';
import { Badge } from './Badge';
import { CopyButton } from './CopyButton';
import { Dialog } from './Dialog';
import { Drawer } from './Drawer';
import { TextField } from './Field';
import { Markdown } from '../panel/components/Markdown';
import { Popover } from './Popover';
import { Menu } from './Menu';
import { Status } from './Status';
import { Textarea } from './Textarea';
import { SelectField } from './SelectField';
import { MenuItem } from './MenuItem';
import { ToastViewport } from './Toast';
import { primaryShortcut } from './keyboard';
import { Tooltip } from './Tooltip';

describe('Button', () => {
  it('keeps component-only props out of the DOM and locks while busy', () => {
    render(() => <Button variant="primary" size="compact" busy data-testid="button">Save</Button>);
    const button = screen.getByTestId('button');
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute('aria-busy', 'true');
    expect(button).not.toHaveAttribute('variant');
    expect(button).not.toHaveAttribute('busy');
    expect(button).not.toHaveAttribute('size');
    expect(button).toHaveClass('ui-button--compact');
    expect(button).toHaveTextContent('正在处理');
  });

  it('keeps secondary safety decisions neutral and semantic', () => {
    render(() => <Button variant="secondary">拒绝</Button>);
    expect(screen.getByRole('button', { name: '拒绝' })).toHaveClass('ui-button--secondary');
  });

  it('describes custom icon help without repeating its action label', () => {
    const { unmount } = render(() => <IconButton label="Archive" title="Close the running session first">×</IconButton>);
    const button = screen.getByRole('button', { name: 'Archive' });
    fireEvent.focusIn(button);
    expect(button).toHaveAccessibleDescription('Close the running session first');
    unmount();
    render(() => <IconButton label="Create session">+</IconButton>);
    const repeated = screen.getByRole('button', { name: 'Create session' });
    fireEvent.focusIn(repeated);
    expect(repeated).not.toHaveAttribute('aria-describedby');
  });

  it('never submits a surrounding form unless submit is explicit', () => {
    const submit = vi.fn((event: SubmitEvent) => event.preventDefault());
    render(() => <form onSubmit={submit}><Button>Cancel</Button><Button type="submit">Save</Button></form>);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(submit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(submit).toHaveBeenCalledOnce();
  });
});

describe('Tooltip', () => {
  it('shows immediately for keyboard focus and Escape dismisses supplemental help', async () => {
    render(() => <Tooltip content="Create a session"><button>New</button></Tooltip>);
    const button = screen.getByRole('button', { name: 'New' });
    fireEvent.focusIn(button);
    const tooltip = screen.getByRole('tooltip');
    expect(tooltip).toHaveTextContent('Create a session');
    expect(document.body.contains(tooltip)).toBe(true);
    expect(button.parentElement?.contains(tooltip)).toBe(false);
    expect(button).not.toHaveAttribute('aria-describedby');
    fireEvent.keyDown(button, { key: 'Escape' });
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
  });

  it('anchors an end-aligned portal and flips above near the viewport edge', () => {
    render(() => <Tooltip content="Stop generation" placement="end"><button>Stop</button></Tooltip>);
    const anchor = screen.getByRole('button', { name: 'Stop' }).parentElement!;
    vi.spyOn(anchor, 'getBoundingClientRect').mockReturnValue({ x: 960, y: 720, left: 960, right: 1000, top: 720, bottom: 760, width: 40, height: 40, toJSON: () => ({}) });
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 768 });
    fireEvent.focusIn(anchor.firstElementChild!);
    const tooltip = screen.getByRole('tooltip');
    expect(tooltip).toHaveClass('ui-tooltip--end', 'is-above');
    expect(tooltip).toHaveStyle({ left: '1000px', top: '713px' });
  });

  it('delays pointer help and cancels it when the pointer leaves', async () => {
    vi.useFakeTimers();
    render(() => <Tooltip content="More actions" delay={400}><button>More</button></Tooltip>);
    const anchor = screen.getByRole('button', { name: 'More' }).parentElement!;
    fireEvent.pointerEnter(anchor);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(400);
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    fireEvent.pointerLeave(anchor);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    vi.useRealTimers();
  });

  it('dismisses before an icon action opens its next surface and ignores touch hover', async () => {
    vi.useFakeTimers();
    const action = vi.fn();
    render(() => <Tooltip content="Session actions" delay={100}><button onClick={action}>Actions</button></Tooltip>);
    const button = screen.getByRole('button', { name: 'Actions' });
    const anchor = button.parentElement!;
    fireEvent.focusIn(button);
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    fireEvent.click(button);
    expect(action).toHaveBeenCalledOnce();
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    fireEvent.focusOut(button);
    fireEvent.pointerEnter(anchor, { pointerType: 'touch' });
    await vi.advanceTimersByTimeAsync(100);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    vi.useRealTimers();
  });
});

describe('Badge', () => {
  it('owns a closed semantic tone without leaking component props', () => {
    render(() => <Badge tone="warn" data-testid="badge">Pending</Badge>);
    const badge = screen.getByTestId('badge');
    expect(badge).toHaveClass('ui-badge--warn');
    expect(badge).not.toHaveAttribute('tone');
    expect(badge).toHaveTextContent('Pending');
  });
});

describe('TextField', () => {
  it('owns an explicit label and hint relationship without leaking props', () => {
    render(() => <TextField label="Project" hint="Absolute path" />);
    const input = screen.getByRole('textbox', { name: 'Project' });
    expect(input).toHaveAccessibleDescription('Absolute path');
    expect(input).not.toHaveAttribute('label');
    expect(input).not.toHaveAttribute('hint');
  });

  it('owns invalid state and connects hint plus error text', () => {
    render(() => <TextField label="Session name" hint="Shown in the sidebar" error="Name is required" />);
    const input = screen.getByRole('textbox', { name: 'Session name' });
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(input).toHaveAccessibleDescription('Shown in the sidebar Name is required');
    expect(input).not.toHaveAttribute('error');
  });
});

describe('Status', () => {
  it('owns semantic tone and optional live behavior without leaking props', () => {
    render(() => <Status tone="warn" live data-testid="status">Reconnecting</Status>);
    const status = screen.getByTestId('status');
    expect(status).toHaveAttribute('role', 'status');
    expect(status).toHaveAttribute('aria-live', 'polite');
    expect(status).toHaveClass('ui-status--warn');
    expect(status).not.toHaveAttribute('tone');
    expect(status).not.toHaveAttribute('live');
  });
});

describe('ToastViewport', () => {
  it('owns polite notification semantics and renders stable records', () => {
    render(() => <ToastViewport label="Operation updates" items={[{ id: 1, content: 'Saved' }, { id: 2, content: 'Connected' }]} />);
    const viewport = screen.getByRole('region', { name: 'Operation updates' });
    expect(viewport).toHaveAttribute('aria-live', 'polite');
    expect(viewport).toHaveAttribute('aria-relevant', 'additions');
    expect(viewport).toHaveTextContent('Saved');
    expect(viewport).toHaveTextContent('Connected');
  });
});

describe('keyboard labels', () => {
  it('uses the visible platform modifier instead of claiming every user has Command', () => {
    const platform = navigator.platform;
    Object.defineProperty(navigator, 'platform', { configurable: true, value: 'Linux x86_64' });
    expect(primaryShortcut('k')).toBe('Ctrl+K');
    Object.defineProperty(navigator, 'platform', { configurable: true, value: 'MacIntel' });
    expect(primaryShortcut('k')).toBe('⌘K');
    Object.defineProperty(navigator, 'platform', { configurable: true, value: platform });
  });
});

describe('Textarea', () => {
  it('forwards normal props and hides component-only auto-growth controls', () => {
    render(() => <Textarea autoResize maxHeight={180} aria-label="Message" />);
    const textarea = screen.getByRole('textbox', { name: 'Message' });
    expect(textarea).not.toHaveAttribute('autoresize');
    expect(textarea).not.toHaveAttribute('maxheight');
    fireEvent.input(textarea, { target: { value: 'Draft' } });
    expect(textarea).toHaveValue('Draft');
  });
});

describe('SelectField', () => {
  it('owns label, hint and error relationships without leaking component props', () => {
    render(() => <SelectField label="保存到项目" hint="选择持久归属" error="请选择项目"><option value="p1">Perihelion</option></SelectField>);
    const select = screen.getByRole('combobox', { name: '保存到项目' });
    expect(select).toHaveAccessibleDescription('选择持久归属 请选择项目');
    expect(select).toHaveAttribute('aria-invalid', 'true');
    expect(select).not.toHaveAttribute('hint');
    expect(select).not.toHaveAttribute('error');
  });
});

describe('MenuItem', () => {
  it('owns menu semantics, button type and danger tone without leaking props', () => {
    render(() => <MenuItem tone="danger">Archive</MenuItem>);
    const item = screen.getByRole('menuitem', { name: 'Archive' });
    expect(item).toHaveAttribute('type', 'button');
    expect(item).toHaveClass('ui-menu__item--danger');
    expect(item).not.toHaveAttribute('tone');
  });
});

describe('CopyButton', () => {
  it('reports success and writes the exact source', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    render(() => <CopyButton text="exact source" label="Copy" />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(screen.getByRole('button')).toHaveTextContent('已复制'));
    expect(writeText).toHaveBeenCalledWith('exact source');
  });

  it('reports clipboard failure instead of pretending success', async () => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error('denied')) } });
    render(() => <CopyButton text="source" label="Copy" />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(screen.getByRole('button')).toHaveTextContent('复制失败'));
  });
});

describe('Dialog', () => {
  it('moves focus in, closes on Escape, restores focus and releases inert', async () => {
    const app = document.createElement('div');
    app.id = 'app'; document.body.append(app);
    const outside = document.createElement('button');
    outside.textContent = 'Outside'; document.body.append(outside); outside.focus();
    let setOpen!: (value: boolean) => void;
    function Harness() {
      const [open, update] = createSignal(true); setOpen = update;
      return <Dialog open={open()} title="Rename" onClose={() => setOpen(false)}><input aria-label="Name" /></Dialog>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Name' })).toHaveFocus());
    const backdrop = screen.getByRole('dialog').closest('.ui-dialog-backdrop');
    expect(document.body.contains(backdrop)).toBe(true);
    expect(app.contains(backdrop)).toBe(false);
    expect(app.inert).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(app.inert).toBe(false);
    expect(outside).toHaveFocus();
    app.remove(); outside.remove();
  });

  it('lets only the topmost nested dialog consume Escape', async () => {
    const app = document.createElement('div');
    app.id = 'app'; document.body.append(app);
    let setOuter!: (value: boolean) => void;
    let setInner!: (value: boolean) => void;
    function Harness() {
      const [outer, updateOuter] = createSignal(true); setOuter = updateOuter;
      const [inner, updateInner] = createSignal(true); setInner = updateInner;
      return <><Dialog open={outer()} title="Outer" onClose={() => setOuter(false)}><button>Outer action</button></Dialog><Dialog open={inner()} title="Inner" onClose={() => setInner(false)}><button>Inner action</button></Dialog></>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getAllByRole('dialog')).toHaveLength(2));
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.getAllByRole('dialog')).toHaveLength(1));
    expect(screen.getByRole('dialog', { name: 'Outer' })).toBeInTheDocument();
    expect(app.inert).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(app.inert).toBe(false);
    app.remove();
  });

  it('does not imply dismissal while a dialog owns an in-flight mutation', async () => {
    const close = vi.fn();
    render(() => <Dialog open title="Saving" dismissible={false} onClose={close}><button>Working</button></Dialog>);
    await waitFor(() => expect(screen.getByRole('dialog', { name: 'Saving' })).toBeInTheDocument());
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.mouseDown(screen.getByRole('dialog').closest('.ui-dialog-backdrop')!);
    expect(close).not.toHaveBeenCalled();
  });

  it('can own a visible title and explicit close action', async () => {
    const close = vi.fn();
    render(() => <Dialog open showHeader title="搜索会话" onClose={close}><input aria-label="Query" /></Dialog>);
    expect(screen.getByRole('heading', { name: '搜索会话' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '关闭搜索会话' }));
    expect(close).toHaveBeenCalledOnce();
  });
});

describe('Drawer', () => {
  it('becomes a labeled modal only on compact layouts and restores its trigger', async () => {
    const background = document.createElement('main');
    document.body.append(background);
    const trigger = document.createElement('button');
    trigger.textContent = 'Open navigation';
    document.body.append(trigger);
    trigger.focus();
    let close!: () => void;
    function Harness() {
      const [open, setOpen] = createSignal(true);
      close = () => setOpen(false);
      return <Drawer open={open()} modal label="Project navigation" background={() => background} onClose={close}><button>First project</button><button>Last project</button></Drawer>;
    }
    render(() => <Harness />);
    const drawer = await screen.findByRole('dialog', { name: 'Project navigation' });
    await waitFor(() => expect(screen.getByRole('button', { name: 'First project' })).toHaveFocus());
    expect(drawer).toHaveAttribute('aria-modal', 'true');
    expect(background.inert).toBe(true);
    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true });
    expect(screen.getByRole('button', { name: 'Last project' })).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Project navigation' })).not.toBeInTheDocument());
    expect(background.inert).toBe(false);
    expect(trigger).toHaveFocus();
    background.remove(); trigger.remove();
  });

  it('stays structural and non-modal on wide layouts', () => {
    const background = document.createElement('main');
    render(() => <Drawer open={false} modal={false} label="Project navigation" background={() => background} onClose={() => {}}><button>Project</button></Drawer>);
    const navigation = screen.getByText('Project').closest('aside');
    expect(navigation).not.toHaveAttribute('role');
    expect(navigation).not.toHaveAttribute('aria-modal');
    expect(navigation?.inert).toBe(false);
    expect(screen.queryByRole('button', { name: '关闭Project navigation' })).not.toBeInTheDocument();
  });

  it('lets a nested Dialog consume Escape before the navigation layer', async () => {
    const app = document.createElement('div'); app.id = 'app'; document.body.append(app);
    const background = document.createElement('main'); document.body.append(background);
    function Harness() {
      const [drawerOpen, setDrawerOpen] = createSignal(true);
      const [dialogOpen, setDialogOpen] = createSignal(true);
      return <Drawer open={drawerOpen()} modal label="Project navigation" background={() => background} onClose={() => setDrawerOpen(false)}><button>Project</button><Dialog open={dialogOpen()} title="Create project" onClose={() => setDialogOpen(false)}><button>Create</button></Dialog></Drawer>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getAllByRole('dialog')).toHaveLength(2));
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.getAllByRole('dialog')).toHaveLength(1));
    expect(screen.getByRole('dialog', { name: 'Project navigation' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    app.remove(); background.remove();
  });

  it('lets a nested Menu consume one Escape before the navigation layer', async () => {
    const background = document.createElement('main'); document.body.append(background);
    function Harness() {
      const [drawerOpen, setDrawerOpen] = createSignal(true);
      const [menuOpen, setMenuOpen] = createSignal(true);
      let trigger: HTMLButtonElement | undefined;
      return <Drawer open={drawerOpen()} modal label="Project navigation" background={() => background} onClose={() => setDrawerOpen(false)}><button ref={trigger}>Project actions</button><Menu open={menuOpen()} id="actions" label="Project actions" trigger={() => trigger} onClose={() => setMenuOpen(false)}><MenuItem>Rename</MenuItem></Menu></Drawer>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getByRole('menu', { name: 'Project actions' })).toBeInTheDocument());
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('menu')).not.toBeInTheDocument());
    expect(screen.getByRole('dialog', { name: 'Project navigation' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    background.remove();
  });
});

describe('Markdown', () => {
  it('renders raw HTML and active links as inert text', () => {
    render(() => <Markdown source={'<script>alert(1)</script>\n\n[unsafe](javascript:alert(2))'} />);
    expect(document.querySelector('script')).not.toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.getByText('<script>alert(1)</script>')).toBeInTheDocument();
    expect(screen.getByText(/unsafe \(javascript:alert\(2\)\)/)).toBeInTheDocument();
  });

  it('isolates safe links and exposes copyable fenced code', () => {
    render(() => <Markdown source={'[docs](https://example.com)\n\n```ts\nconst x = 1;\n```'} />);
    const link = screen.getByRole('link', { name: /docs/ });
    expect(link).toHaveAttribute('target', '_blank');
    expect(link).toHaveAttribute('rel', 'noopener noreferrer');
    expect(screen.getByRole('button', { name: '复制代码' })).toBeInTheDocument();
    expect(screen.getByText('const x = 1;')).toBeInTheDocument();
  });
});

describe('Popover', () => {
  it('enters focus, closes on outside interaction and restores its trigger', async () => {
    const trigger = document.createElement('button');
    trigger.textContent = 'Rename'; document.body.append(trigger); trigger.focus();
    const outside = document.createElement('button');
    outside.textContent = 'Outside'; document.body.append(outside);
    let setOpen!: (value: boolean) => void;
    function Harness() {
      const [open, update] = createSignal(true); setOpen = update;
      return <Popover open={open()} id="rename" label="Rename session" trigger={() => trigger} onClose={() => setOpen(false)}><input aria-label="Session name" /></Popover>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Session name' })).toHaveFocus());
    fireEvent.pointerDown(outside);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
    trigger.remove(); outside.remove();
  });

  it('keeps an in-flight edit open across Escape and outside interaction', async () => {
    const trigger = document.createElement('button');
    trigger.textContent = 'Rename'; document.body.append(trigger);
    const outside = document.createElement('button');
    outside.textContent = 'Outside'; document.body.append(outside);
    const close = vi.fn();
    render(() => <Popover open id="rename-busy" label="Rename session" trigger={() => trigger} dismissible={false} onClose={close}><input aria-label="Session name" /></Popover>);
    await waitFor(() => expect(screen.getByRole('dialog', { name: 'Rename session' })).toBeInTheDocument());
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.pointerDown(outside);
    expect(close).not.toHaveBeenCalled();
    trigger.remove(); outside.remove();
  });
});

describe('Menu', () => {
  it('supports arrow navigation, Escape dismissal and trigger focus restoration', async () => {
    const trigger = document.createElement('button');
    trigger.textContent = 'Actions'; document.body.append(trigger); trigger.focus();
    let setOpen!: (value: boolean) => void;
    function Harness() {
      const [open, update] = createSignal(true); setOpen = update;
      return <Menu open={open()} id="actions" label="Session actions" trigger={() => trigger} onClose={() => setOpen(false)}><button role="menuitem">First</button><button role="menuitem">Second</button></Menu>;
    }
    render(() => <Harness />);
    await waitFor(() => expect(screen.getByRole('menuitem', { name: 'First' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'ArrowDown' });
    expect(screen.getByRole('menuitem', { name: 'Second' })).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('menu')).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
    trigger.remove();
  });
});
