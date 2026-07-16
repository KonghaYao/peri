import html from 'solid-js/html';
import { createSignal, createResource, createEffect, createMemo, For, Show, onMount, onCleanup } from 'solid-js';
import { useTheme, useParentMethod } from '/lib/solid-hooks.js';
import { getJSON } from '/lib/api.js';
import { Header, IconButton } from '/lib/ui/index.js';
import { Graph, drawGraph, LANE_W, ROW_H } from '/lib/scm/graph-layout.js';

export function ScmGraph() {
  const [theme] = useTheme();
  const closeGraph = useParentMethod('closeGraph');

  const [commits, setCommits] = createSignal([]);
  const [loadError, setLoadError] = createSignal('');

  onMount(async () => {
    try {
      const d = await getJSON('/api/scm/graph?max=200');
      if (d.hasRepo === false) { setLoadError('not a git repo'); return; }
      setCommits(d.commits || []);
    } catch (e) { setLoadError(e.message || String(e)); }
  });

  // 布局引擎
  const graphData = createMemo(() => {
    const cs = commits();
    if (!cs.length) return null;
    const g = new Graph();
    g.loadCommits(cs);
    const canvasW = g.getContentWidth(LANE_W);
    const totalH = cs.length * ROW_H;
    return { graph: g, canvasW, totalH };
  });

  // Canvas 绘制
  let canvasRef = null;
  createEffect(() => {
    const data = graphData();
    if (!data || !canvasRef) return;
    const { graph, canvasW, totalH } = data;
    const dpr = window.devicePixelRatio || 1;
    canvasRef.width = canvasW * dpr;
    canvasRef.height = totalH * dpr;
    canvasRef.style.width = canvasW + 'px';
    canvasRef.style.height = totalH + 'px';
    const ctx = canvasRef.getContext('2d');
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, canvasW, totalH);
    drawGraph(ctx, graph, commits());
  });

  // diff
  const [selectedHash, setSelectedHash] = createSignal(null);
  const [hoveredIdx, setHoveredIdx] = createSignal(-1);
  const [diff] = createResource(selectedHash, async (h) => {
    if (!h) return null;
    try {
      const d = await getJSON('/api/scm/commit-diff?hash=' + encodeURIComponent(h));
      if (!d.diff) return '';
      try {
        return window.Diff2Html.html(window.Diff2Html.parse(d.diff), {
          drawFileList: false, matching: 'lines', outputFormat: 'side-by-side',
        });
      } catch { return ''; }
    } catch { return ''; }
  });

  // Esc 关闭
  onMount(() => {
    const onKey = (e) => { if (e.key === 'Escape') closeGraph(); };
    document.addEventListener('keydown', onKey);
    onCleanup(() => document.removeEventListener('keydown', onKey));
  });

  const getIdxFromY = (clientY) => {
    if (!canvasRef) return -1;
    const rect = canvasRef.getBoundingClientRect();
    return Math.floor((clientY - rect.top) / ROW_H);
  };

  const handleCanvasClick = (e) => {
    const idx = getIdxFromY(e.clientY);
    const cs = commits();
    if (idx >= 0 && idx < cs.length && cs[idx].hash) {
      setSelectedHash(cs[idx].hash);
    }
  };

  const handleCanvasMove = (e) => {
    setHoveredIdx(getIdxFromY(e.clientY));
  };
  const handleCanvasLeave = () => setHoveredIdx(-1);

  return html`
    <${Header} title="Git Graph">
      <${IconButton} title="close (Esc)" onClick=${() => closeGraph()}>\u2715<//>
    <//>
    <div class="flex min-h-0 flex-1">
      <div class="flex-1 overflow-y-auto">
        <${Show} when=${loadError}
          fallback=${() => html`
            <${Show} when=${() => commits().length > 0}
              fallback=${() => html`<div class="p-5 text-center text-text-muted">加载中...</div>`}>
              <div style="display:flex">
                <canvas ref=${el => { canvasRef = el; }}
                  onClick=${handleCanvasClick}
                  onMouseMove=${handleCanvasMove}
                  onMouseLeave=${handleCanvasLeave}
                  style="flex-shrink:0;cursor:pointer" />
                <div class="flex-1 min-w-0">
                  <${For} each=${commits}>${(c, i) => html`
                    <div class="flex items-center cursor-pointer text-xs px-2"
                      style=${{ height: ROW_H + 'px', lineHeight: ROW_H + 'px' }}
                      classList=${{
                        'bg-bg-active': selectedHash() === c.hash,
                        'bg-bg-hover': hoveredIdx() === i(),
                      }}
                      onMouseEnter=${() => setHoveredIdx(i())}
                      onMouseLeave=${() => setHoveredIdx(-1)}
                      onClick=${() => c.hash && setSelectedHash(c.hash)}>
                      <${Show} when=${c.uncommitted}>
                        <span class="text-text-muted italic">${c.subject}</span>
                      <//>
                      <${Show} when=${!c.uncommitted}>
                        <span class="shrink-0 font-mono text-text-muted pr-2">${c.shortHash}</span>
                        <${Show} when=${() => (c.refs || []).length > 0}>
                          <span class="flex shrink-0 gap-0.5 mr-1">
                            <${For} each=${() => c.refs}>${(ref) => html`
                              <span class="max-w-[120px] overflow-hidden whitespace-nowrap rounded px-1.5 py-[1px] text-[10px] text-ellipsis text-white"
                                classList=${{ 'bg-accent': ref !== 'HEAD' && !ref.startsWith('HEAD ->'), 'bg-success': ref === 'HEAD' || ref.startsWith('HEAD ->') }}>
                                ${ref}
                              </span>
                            `}<//>
                          </span>
                        <//>
                        <span class="min-w-0 flex-1 overflow-hidden whitespace-nowrap text-ellipsis">${c.subject}</span>
                      <//>
                    </div>
                  `}<//>
                </div>
              </div>
            <//>
          `}>
          <div class="p-5 text-center text-text-muted">failed: ${loadError}</div>
        <//>
      </div>
      <div class="flex-1 overflow-auto border-l border-border p-2 min-w-0">
        <${Show} when=${selectedHash()}
          fallback=${() => html`<div class="p-5 text-center text-text-muted">选择 commit 查看 diff</div>`}>
          <${Show} when=${diff()}
            fallback=${() => html`<div class="p-2 text-[11px] text-text-muted">loading diff...</div>`}>
            <div class="text-[13px]" innerHTML=${diff}></div>
          <//>
        <//>
      </div>
    </div>
  `;
}
