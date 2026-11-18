import { LOOPBACK_HOST, MCP_PATH, PACKAGE_NAME } from './constants.js';

/** 根路径说明页：只用实际监听端口拼回环地址，不读取数据库或请求头。 */
export function renderInstructionsPage(port: number): string {
  const mcpUrl = `http://${LOOPBACK_HOST}:${port}${MCP_PATH}`;

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${PACKAGE_NAME} 使用说明</title>
<style>
  :root { color-scheme: light dark; }
  body { margin: 0; font-family: system-ui, -apple-system, "Segoe UI", sans-serif; line-height: 1.6; }
  main { max-width: 52rem; margin: 0 auto; padding: 2rem 1.25rem 4rem; }
  h1 { font-size: 1.6rem; margin-bottom: 0.5rem; }
  h2 { font-size: 1.2rem; margin-top: 2rem; }
  code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  pre { padding: 0.9rem 1rem; overflow-x: auto; border: 1px solid #8884; border-radius: 6px; }
  dt { font-weight: 600; margin-top: 1rem; }
  dd { margin-left: 0; }
  .note { border-left: 3px solid #d18b00; padding-left: 0.9rem; }
</style>
</head>
<body>
<main>
<h1>${PACKAGE_NAME} 使用说明</h1>
<p>本地 SQLite MCP 服务已启动，仅监听回环地址 <code>${LOOPBACK_HOST}</code>。</p>
<dl>
  <dt>MCP 端点（Streamable HTTP）</dt>
  <dd><code>${mcpUrl}</code></dd>
</dl>

<h2>接入配置示例</h2>
<p>以下为通用的 Streamable HTTP 客户端配置；字段名可能因客户端而异，请按客户端文档调整。</p>
<pre><code>{
  "mcpServers": {
    "sqlite-mcp": {
      "type": "streamable-http",
      "url": "${mcpUrl}"
    }
  }
}</code></pre>

<h2>工具与参数</h2>
<p><code>list_tables</code>：无参数，列出主数据库中的表和视图。</p>
<pre><code>{}</code></pre>
<p><code>describe_table</code>：传入表或视图名称，查看字段和定义。</p>
<pre><code>{"table":"example_table"}</code></pre>
<p><code>execute_sql</code>：传入恰好一条 SQL，可选按顺序绑定 <code>?</code> 占位符的参数。</p>
<pre><code>{"sql":"SELECT * FROM example_table LIMIT 5","params":[]}</code></pre>
<p>参数支持字符串、数字、布尔值和 <code>null</code>，例如 <code>{"sql":"SELECT * FROM example_table WHERE id = ?","params":[1]}</code>。</p>

<h2>建议顺序</h2>
<ol>
  <li>先调用 <code>list_tables</code> 了解可用表和视图。</li>
  <li>再调用 <code>describe_table</code> 确认字段名、类型和主键。</li>
  <li>最后用 <code>execute_sql</code> 执行查询或写入。</li>
</ol>

<h2>注意</h2>
<ul class="note">
  <li>端点无鉴权；能访问本机回环端口的进程都可以调用。</li>
  <li><code>execute_sql</code> 具备写权限，可执行写入和 DDL，请自行确认语句影响。</li>
  <li>建议串行调用工具，避免对同一数据库的并发写入互相干扰。</li>
  <li><code>execute_sql</code> 每次只接受一条语句；结果最多返回 100 行，<code>truncated</code> 只表示响应被截断。</li>
</ul>
</main>
</body>
</html>
`;
}
