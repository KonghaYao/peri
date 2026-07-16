import html from 'solid-js/html';
import { createSignal, Show } from 'solid-js';
import { useTheme, useParentMethod } from '/lib/solid-hooks.js';
import { sendJSON } from '/lib/api.js';
import { Header, IconButton, Button, KVInput, Empty } from '/lib/ui/index.js';

export function Getman() {
  useTheme();
  const closeGetman = useParentMethod('closeGetman');

  const [method, setMethod] = createSignal('GET');
  const [url, setUrl] = createSignal('');
  const [body, setBody] = createSignal('');
  const [headers, setHeaders] = createSignal([{ key: '', value: '' }]);
  const [importVisible, setImportVisible] = createSignal(false);
  const [curlText, setCurlText] = createSignal('');
  const [loading, setLoading] = createSignal(false);
  const [response, setResponse] = createSignal(null);
  const [parseError, setParseError] = createSignal('');

  const canSend = () => !loading() && !!url().trim();

  const send = async () => {
    if (!url().trim()) return;
    setLoading(true);
    setResponse(null);
    try {
      const hdrs = {};
      headers().forEach(h => { if (h.key.trim()) hdrs[h.key.trim()] = h.value; });
      const r = await sendJSON('/api/getman/proxy', 'POST', {
        method: method(),
        url: url(),
        headers: hdrs,
        body: body() || null,
      });
      setResponse(r);
    } catch (e) {
      setResponse({ error: e.message || String(e) });
    } finally {
      setLoading(false);
    }
  };

  const parseCurl = async () => {
    setParseError('');
    try {
      const d = await sendJSON('/api/getman/parse-curl', 'POST', { curl: curlText() });
      if (d.error) {
        setParseError(d.error);
        return;
      }
      setMethod(d.method || 'GET');
      setUrl(d.url || '');
      if (d.body) setBody(d.body);
      if (d.headers && typeof d.headers === 'object') {
        const entries = Object.entries(d.headers).map(([k, v]) => ({ key: k, value: String(v) }));
        if (entries.length > 0) setHeaders(entries);
      }
      setImportVisible(false);
      setCurlText('');
    } catch (e) {
      setParseError(e.message || String(e));
    }
  };

  const responseStatus = () => {
    const r = response();
    if (!r || r.error) return null;
    return r.status;
  };
  const isError = () => !!response()?.error;
  const statusClass = (code) => {
    if (code == null) return '';
    if (code >= 200 && code < 400) return 'bg-success/20 text-success';
    return 'bg-error/20 text-error';
  };

  return html`
    <${Header} title="Getman">
      <${IconButton} title="import curl" onClick=${() => setImportVisible(!importVisible())}>↓<//>
      <${IconButton} title="send" onClick=${send} disabled=${() => !canSend()}>▶<//>
      <${IconButton} title="close" onClick=${() => closeGetman()}>✕<//>
    <//>
    <div class="flex-1 overflow-y-auto p-4 flex flex-col gap-3">
      <${Show} when=${importVisible}>
        <div class="flex gap-2 items-stretch">
          <textarea class="flex-1" rows="3" placeholder="paste curl command..." value=${curlText} onInput=${e => setCurlText(e.target.value)}></textarea>
          <${Button} variant="primary" onClick=${parseCurl}>parse<//>
        </div>
        <${Show} when=${parseError}>
          <div class="response error">${parseError}</div>
        <//>
      <//>
      <div class="flex gap-2">
        <select value=${method} onChange=${e => setMethod(e.target.value)}>
          <option>GET</option>
          <option>POST</option>
          <option>PUT</option>
          <option>PATCH</option>
          <option>DELETE</option>
          <option>HEAD</option>
        </select>
        <input class="flex-1" type="text" placeholder="https://..." value=${url} onInput=${e => setUrl(e.target.value)} />
        <${Button} variant="primary" onClick=${send} disabled=${() => !canSend()}>
          ${() => loading() ? '...' : 'send'}
        <//>
      </div>
      <div class="flex flex-col gap-1">
        <h3 class="text-[11px] text-text-muted uppercase tracking-wide">Headers</h3>
        <${KVInput}
          entries=${() => headers()}
          onChange=${(next) => setHeaders(next)}
          keyPlaceholder="key"
          valuePlaceholder="value"
          addLabel="+ header"
        />
      </div>
      <${Show} when=${() => method() !== 'GET' && method() !== 'HEAD'}>
        <div class="flex flex-col gap-1">
          <h3 class="text-[11px] text-text-muted uppercase tracking-wide">Body</h3>
          <textarea class="w-full font-mono resize-vertical" rows="4" value=${body} onInput=${e => setBody(e.target.value)}></textarea>
        </div>
      <//>
      <${Show} when=${response}>
        <div class="flex flex-col gap-1">
          <h3 class="text-[11px] text-text-muted uppercase tracking-wide">Response</h3>
          <${Show} when=${() => !isError()}>
            <div class="flex gap-2 items-center">
              <span class="px-1.5 py-0.5 rounded font-mono text-[11px] font-semibold ${() => statusClass(responseStatus())}">${() => responseStatus() ?? '--'}</span>
              <${Show} when=${() => response()?.headers?.['content-type']}>
                <span style="color:var(--color-text-muted);font-size:11px">${() => response()?.headers?.['content-type']}</span>
              <//>
            </div>
          <//>
          <div class="response ${() => isError() ? 'error' : ''}">${() => JSON.stringify(response(), null, 2)}</div>
        </div>
      <//>
      <${Show} when=${() => !response() && !loading()}>
        <${Empty} text="Enter a URL and press send" />
      <//>
    </div>
  `;
}
