import { useState } from 'react';
import { Blocks, Plus, Trash2, X } from 'lucide-react';
import { configProblems, type BrowserConfig, type McpServerEntry } from '../settings/config';

/**
 * 插件页：MCP 服务器列表（可添加多条），条目保存在浏览器 VFS 的配置文件里（IndexedDB）。
 * 宿主启动时读取：URL 改写为同源 relay 前缀（`/api/relay/mcp/...`）后随 settings 注入，
 * 浏览器宿主只经 relay 连接 MCP 上游；保存后刷新页面让新配置生效。
 */
export interface PluginsViewProps {
  config: BrowserConfig;
  onSave: (config: BrowserConfig) => void;
  /** 上一次保存失败的原因（写入 VFS 失败时由 App 传入）。 */
  error?: string;
  onClose?: () => void;
}

const emptyEntry = (): McpServerEntry => ({ name: '', url: '', headers: '' });

export function PluginsView({ config, onSave, error, onClose }: PluginsViewProps) {
  const [draft, setDraft] = useState(config);
  const problems = configProblems(draft);
  const update = (index: number, patch: Partial<McpServerEntry>) => setDraft((current) => ({
    ...current,
    mcpServers: current.mcpServers.map((entry, position) => position === index ? { ...entry, ...patch } : entry),
  }));
  const remove = (index: number) => setDraft((current) => ({
    ...current,
    mcpServers: current.mcpServers.filter((_, position) => position !== index),
  }));

  return (
    <div className="settings-shell">
      <form className="settings-card" onSubmit={(event) => { event.preventDefault(); if (problems.length === 0) onSave(draft); }}>
        <div className="settings-head">
          <span className="brand-mark"><Blocks size={19} /></span>
          <h1>插件 · MCP 服务器</h1>
          {onClose && <button type="button" className="icon-button" onClick={onClose} aria-label="关闭插件页"><X size={17} /></button>}
        </div>
        <p className="settings-intro">接入外部 MCP 服务器（只接 2026-07-28 协议版本）：浏览器宿主经同源 relay 转发请求，地址与请求头只保存在本机浏览器；保存后刷新页面生效。</p>
        {draft.mcpServers.length === 0 && <div className="settings-group"><p className="settings-empty">还没有 MCP 服务器，点击下方按钮添加。</p></div>}
        {draft.mcpServers.map((entry, index) => (
          <div className="settings-group" key={index}>
            <div className="settings-group-head">
              <h2>MCP 服务器 #{index + 1}</h2>
              <button type="button" className="icon-button" onClick={() => remove(index)} aria-label={`删除 MCP 服务器 #${index + 1}`}><Trash2 size={15} /></button>
            </div>
            <label className="settings-field">
              <span>名称（必填，唯一；工具名前缀 mcp__名称__工具）</span>
              <input value={entry.name} placeholder="workspace" autoComplete="off" spellCheck={false}
                onChange={(event) => update(index, { name: event.target.value })} />
            </label>
            <label className="settings-field">
              <span>MCP Server URL（必填，http(s)）</span>
              <input value={entry.url} placeholder="https://mcp.example.com/mcp" autoComplete="off" spellCheck={false}
                onChange={(event) => update(index, { url: event.target.value })} />
            </label>
            <label className="settings-field">
              <span>请求头（可选，每行一条 Key: Value）</span>
              <textarea value={entry.headers} rows={2} placeholder="Authorization: Bearer …" spellCheck={false}
                onChange={(event) => update(index, { headers: event.target.value })} />
            </label>
          </div>
        ))}
        <button type="button" className="secondary-button" onClick={() => setDraft((current) => ({ ...current, mcpServers: [...current.mcpServers, emptyEntry()] }))}>
          <Plus size={15} />添加 MCP 服务器
        </button>
        <div className="settings-foot">
          <button type="submit" className="primary-button" disabled={problems.length > 0}>{onClose ? '保存并刷新' : '保存并进入'}</button>
          <span className="muted">配置在宿主启动时读取，保存后刷新页面生效。</span>
        </div>
        {problems.length > 0 && <p className="settings-problems" role="status">还需处理：{problems.join('、')}</p>}
        {error && <p className="settings-problems" role="alert">保存失败：{error}</p>}
      </form>
    </div>
  );
}
