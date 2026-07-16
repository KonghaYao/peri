import html from 'solid-js/html';
import { createResource, Show } from 'solid-js';
import { useCurrentFile, useTheme } from '/lib/solid-hooks.js';
import { getJSON } from '/lib/api.js';
import { Empty } from '/lib/ui/index.js';

const TEXT_EXTS = new Set([
  'md','txt','json','xml','yml','yaml','toml',
  'js','ts','jsx','tsx','html','htm','css','scss','less',
  'py','rb','go','rs','java','c','cpp','h','hpp',
  'sh','bash','zsh','sql','graphql','svg','env',
]);

function isTextFile(name) {
  const ext = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  return ext === '' || TEXT_EXTS.has(ext);
}

function isMarkdown(name) {
  return name.toLowerCase().endsWith('.md');
}

export function Preview() {
  const [file] = useCurrentFile();
  useTheme();

  const [content] = createResource(file, async (f) => {
    if (!f) return null;
    try {
      if (!isTextFile(f.name)) {
        return { type: 'binary', name: f.name };
      }
      const text = await getJSON('/api/file?path=' + encodeURIComponent(f.path));
      return { type: isMarkdown(f.name) ? 'md' : 'code', name: f.name, content: text.content ?? text };
    } catch (e) {
      return { type: 'error', message: e.message };
    }
  });

  return html`
    <${Show} when=${file}
      fallback=${() => html`<${Empty} text="未选择文件" />`}>
      <${Show} when=${content}
        fallback=${() => html`<${Empty} text="加载中..." />`}>
        <${Show} when=${() => content()?.type === 'md'}>
          <div class="markdown-body" innerHTML=${() => marked.parse(content().content)}></div>
        <//>
        <${Show} when=${() => content()?.type === 'code'}>
          <pre class="code-body"><code>${() => content()?.content}</code></pre>
        <//>
        <${Show} when=${() => content()?.type === 'binary'}>
          <${Empty} text=${() => `📎 ${content().name}（二进制文件不支持预览）`} />
        <//>
        <${Show} when=${() => content()?.type === 'error'}>
          <${Empty} text=${() => content()?.message || '加载失败'} />
        <//>
      <//>
    <//>
  `;
}
