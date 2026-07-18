import html from 'solid-js/html';
import { createSignal, createEffect, For, Show, onMount, onCleanup } from 'solid-js';
import { useTheme, useScmVersion, useParentMethod } from '/lib/solid-hooks.js';
import { getJSON, sendJSON } from '/lib/api.js';
import { Header, IconButton, Button, Badge, Empty, Tabs } from '/lib/ui/index.js';

function sCls(e) {
  if (e.index === '?' && e.worktree === '?') return 'unknown';
  if (e.index === 'A') return 'added';
  if (e.index === 'D') return 'deleted';
  return 'modified';
}
function sLbl(e) {
  if (e.index === '?' && e.worktree === '?') return 'U';
  if (e.index === 'A') return 'A';
  if (e.index === 'D') return 'D';
  if (e.index === 'R') return 'R';
  return 'M';
}

export function ScmPanel() {
  const [theme] = useTheme();
  const [, setScmVersion] = useScmVersion();
  const openGraph = useParentMethod('openGraph');
  const setStatusText = useParentMethod('setStatusText');

  const [diffFile, setDiffFile] = createSignal(null);
  const [diffHtml, setDiffHtml] = createSignal('');
  const [msg, setMsg] = createSignal('');
  const [committing, setCommitting] = createSignal(false);
  const [activeTab, setActiveTab] = createSignal('changes');

  // SCM 状态：手动管理（避免 createResource 内部 createRenderEffect 引发响应式循环）
  const [status, setStatus] = createSignal(null);
  // 并发请求去重 + 浅比较避免无意义 DOM 重建
  let fetching = false;
  let lastData = null;
  const refresh = async () => {
    if (fetching) return;
    fetching = true;
    try {
      const d = await getJSON('/api/scm/status');
      // 浅比较：若数据未变则不触发 signal 更新，避免 <For> DOM 重建
      if (!lastData || lastData.branch !== d.branch
        || lastData.staged?.length !== d.staged?.length
        || lastData.unstaged?.length !== d.unstaged?.length
        || lastData.hasRepo !== d.hasRepo) {
        lastData = d;
        setStatus(d);
      }
    } catch (e) {
      // 忽略单次失败，保持旧状态
      console.error('[scm] refresh failed:', e);
    } finally {
      fetching = false;
    }
  };
  // 初始拉取 + 定时轮询（不可见时 SKIP）
  onMount(() => {
    refresh();
    const id = setInterval(refresh, 10000);
    onCleanup(() => clearInterval(id));
  });

  // 从 signal 派生
  const branch = () => status()?.branch || '';
  const staged = () => status()?.staged || [];
  const unstaged = () => status()?.unstaged || [];

  // 同步状态栏文字
  createEffect(() => {
    const d = status();
    if (d?.hasRepo) {
      const sc = (d.staged || []).length + (d.unstaged || []).length;
      setStatusText?.(d.branch + (sc > 0 ? ` ${sc} files` : ' \u2713'));
    }
  });

  const stage = async (p) => {
    await sendJSON('/api/scm/stage', 'POST', { files: [p], action: 'stage' });
    await refresh();
  };
  const unstage = async (p) => {
    await sendJSON('/api/scm/stage', 'POST', { files: [p], action: 'unstage' });
    await refresh();
  };
  const discard = async (p) => {
    await sendJSON('/api/scm/discard', 'POST', { files: [p] });
    await refresh();
  };
  const commit = async () => {
    if (!msg().trim() || staged().length === 0) return;
    setCommitting(true);
    try {
      await sendJSON('/api/scm/commit', 'POST', { message: msg() });
      setMsg('');
      await refresh();
      setScmVersion(v => (v || 0) + 1);
    } finally { setCommitting(false); }
  };
  const showDiff = async (p, s) => {
    try {
      const d = await getJSON('/api/scm/diff?file=' + encodeURIComponent(p) + '&staged=' + s);
      setDiffFile({ path: p, staged: s });
      try {
        setDiffHtml(d.diff ? Diff2Html.html(Diff2Html.parse(d.diff), { drawFileList: false, matching: 'lines', outputFormat: 'side-by-side' }) : '');
      } catch (parseErr) {
        console.error('Diff2Html parse failed:', parseErr);
        setDiffHtml('');
      }
    } catch (e) {
      setDiffFile(null);
      setDiffHtml('');
    }
  };

  const hasChanges = () => staged().length > 0 || unstaged().length > 0;

  const tabs = () => [
    { id: 'changes', label: 'Changes' },
    { id: 'graph', label: 'Graph' },
  ];
  const handleTabChange = (id) => {
    if (id === 'graph') {
      openGraph();
      return;
    }
    setActiveTab(id);
  };

  return html`
    <${Header} title="Changes" class="px-2">
      <${Tabs} tabs=${tabs} activeId=${activeTab} onChange=${handleTabChange} />
      <${IconButton} title="refresh" onClick=${() => refresh()}>\u27F3<//>
    <//>
    <div class="flex-1 overflow-y-auto py-1 min-h-0">
      <${Show} when=${hasChanges}
        fallback=${() => html`<${Empty} icon="\u2713" text="没有更改" />`}>
        <${Show} when=${() => staged().length > 0}>
          <div>
            <div class="flex justify-between px-2 py-1 text-[11px] text-text-muted font-semibold">
              <span>暂存</span>
              <span>${() => staged().length}</span>
            </div>
            <${For} each=${staged}>${(e) => html`
              <div class="flex items-center gap-1.5 px-2 py-[3px] hover:bg-bg-hover cursor-pointer text-[12px]"
                onClick=${() => showDiff(e.path, true)}>
                <span class=${'text-[10px] font-mono w-[16px] text-right ' + (sCls(e) === 'added' ? 'text-success' : sCls(e) === 'deleted' ? 'text-error' : 'text-warning')}>${sLbl(e)}</span>
                <span class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap text-text">${e.path}</span>
                <${IconButton} title="unstage" onClick=${() => unstage(e.path)}>−<//>
              </div>
            `}<//>
          </div>
        <//>
        <${Show} when=${() => unstaged().length > 0}>
          <div>
            <div class="flex justify-between px-2 py-1 text-[11px] text-text-muted font-semibold">
              <span>更改</span>
              <span>${() => unstaged().length}</span>
            </div>
            <${For} each=${unstaged}>${(e) => html`
              <div class="flex items-center gap-1.5 px-2 py-[3px] hover:bg-bg-hover cursor-pointer text-[12px]"
                onClick=${() => showDiff(e.path, false)}>
                <span class=${'text-[10px] font-mono w-[16px] text-right ' + (sCls(e) === 'added' ? 'text-success' : sCls(e) === 'deleted' ? 'text-error' : 'text-warning')}>${sLbl(e)}</span>
                <span class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap text-text">${e.path}</span>
                <${IconButton} title="stage" class="text-success" onClick=${() => stage(e.path)}>+<//>
                <${IconButton} title="discard" class="text-error" onClick=${() => discard(e.path)}>\uD83D\uDDD1<//>
              </div>
            `}<//>
          </div>
        <//>
      <//>
      <${Show} when=${diffFile}>
        <div class="border-t border-border p-2">
          <div class="flex items-center justify-between mb-1">
            <span class="text-[11px] text-text-muted">${() => diffFile()?.path}</span>
            <${IconButton} title="close diff" onClick=${() => setDiffFile(null)}>\u2715<//>
          </div>
          <div class="diff-container text-[12px] overflow-auto max-h-[200px]" innerHTML=${diffHtml}></div>
        </div>
      <//>
    </div>
    <${Show} when=${() => staged().length > 0}>
      <div class="flex gap-1.5 p-2 border-t border-border">
        <input
          class="flex-1 bg-bg-secondary border border-border text-text text-[12px] rounded px-2 py-1 focus:border-accent"
          placeholder="commit message"
          value=${msg}
          onInput=${(e) => setMsg(e.target.value)}
          onKeyDown=${(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); commit(); } }}
        />
        <${Button} variant="primary" disabled=${() => !msg().trim() || committing() || staged().length === 0} onClick=${commit}>
          ${() => committing() ? '...' : 'Commit'}
        <//>
      </div>
    <//>
  `;
}
