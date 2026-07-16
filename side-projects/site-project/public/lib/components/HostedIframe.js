// ========== HostedIframe 组件（父侧专用） ==========
// 封装：创建 iframe、expose store、wrap 子 API、lazy src、显隐、reload、超时检测。
import html from 'solid-js/html';
import { createSignal, onMount, onCleanup } from 'solid-js';
import { expose, wrap, windowEndpoint } from 'comlink';

/**
 * @param {{
 *   src: string,
 *   name: string,
 *   store?: object,
 *   lazy?: boolean,
 *   onReady?: (childAPI: object) => void,
 *   ref?: (api: object) => void | { current: object },
 *   class?: string,
 * }} props
 *
 * 注意：reload() 后外部持有的旧 childAPI 引用会失效，需通过 api.child() 重新获取。
 */
export function HostedIframe(props) {
  const [status, setStatus] = createSignal('idle');   // idle | loading | ready | error
  const [error, setError] = createSignal(null);
  let frameRef = null;
  let childAPI = null;
  let loadTimer = null;

  const doExpose = () => {
    if (props.store && frameRef?.contentWindow) {
      expose(props.store, windowEndpoint(frameRef.contentWindow, window));
    }
  };

  const ensureSrc = () => {
    if (!frameRef) return;
    const cur = frameRef.src || '';
    // 尚未设置 / about:blank / 当前页 → 设置目标 src
    if (!cur || cur === 'about:blank' || cur === location.href) {
      frameRef.src = props.src;
    }
  };

  const startTimer = () => {
    if (loadTimer) clearTimeout(loadTimer);
    loadTimer = setTimeout(() => {
      if (status() !== 'ready') {
        setStatus('error');
        setError('load timeout (10s)');
      }
    }, 10000);
  };

  const load = () => {
    setStatus('loading');
    setError(null);
    ensureSrc();
    startTimer();
  };

  onMount(() => {
    // 把 imperative api 暴露给 ref（此时 frameRef 已绑定）
    const api = {
      open:   () => { ensureSrc(); if (frameRef) frameRef.style.display = 'block'; },
      close:  () => { if (frameRef) frameRef.style.display = 'none'; },
      reload: () => {
        childAPI = null;
        setStatus('loading');
        setError(null);
        startTimer();
        if (frameRef) frameRef.src = props.src;
      },
      call:   (method, ...args) => childAPI?.[method]?.(...args),
      child:  () => childAPI,
      loaded: () => status() === 'ready',
      status: () => status(),
      error:  () => error(),
    };
    if (typeof props.ref === 'function') props.ref(api);
    else if (props.ref) props.ref.current = api;

    if (!props.lazy) load();

    const onLoad = () => {
      // 跳过空白页 load（lazy iframe 首次 load 为 about:blank）
      const cur = frameRef?.src || '';
      if (!cur || cur === 'about:blank') return;
      if (loadTimer) { clearTimeout(loadTimer); loadTimer = null; }
      try {
        doExpose();
        childAPI = wrap(windowEndpoint(frameRef.contentWindow, window));
        setStatus('ready');
        props.onReady?.(childAPI);
      } catch (e) {
        setStatus('error');
        setError(e.message);
        console.error('[HostedIframe] load failed:', e);
      }
    };
    const onError = (e) => {
      setStatus('error');
      setError(e.message || 'frame error');
    };

    frameRef.addEventListener('load', onLoad);
    frameRef.addEventListener('error', onError);
    onCleanup(() => {
      if (loadTimer) clearTimeout(loadTimer);
      frameRef?.removeEventListener('load', onLoad);
      frameRef?.removeEventListener('error', onError);
    });
  });

  return html`<iframe
    ref=${(el) => { frameRef = el; }}
    src=${props.lazy ? undefined : props.src}
    name=${props.name}
    class=${props.class}
    sandbox="allow-scripts allow-same-origin"
  />`;
}
