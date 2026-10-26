import { useEffect, useState } from "react";
import { Sparkles, X } from "lucide-react";
import type { ServerInfo } from "../remote";

/**
 * 只读服务端信息：模型、存储与 MCP 由后端环境变量配置，前端不再保存任何凭据。
 * 端点只返回去敏后的地址（无 token、query 与 userinfo）。
 */
export function ServerInfoDialog({ onClose }: { onClose: () => void }) {
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    fetch("/api/info", { headers: { accept: "application/json" } })
      .then(async (response) => {
        const body = await response.json().catch(() => null) as (ServerInfo & { error?: string }) | null;
        if (!response.ok || !body) throw new Error(body?.error ?? `HTTP ${response.status}`);
        if (active) setInfo(body);
      })
      .catch((failure: unknown) => {
        if (active) setError(failure instanceof Error ? failure.message : "无法读取服务端信息");
      });
    return () => { active = false; };
  }, []);

  return (
    <div className="settings-shell">
      <div className="settings-card" role="dialog" aria-label="服务端信息">
        <div className="settings-head">
          <span className="brand-mark"><Sparkles size={19} /></span>
          <h1>服务端信息</h1>
          <button type="button" className="icon-button" onClick={onClose} aria-label="关闭设置"><X size={17} /></button>
        </div>
        <p className="settings-intro">模型、存储与 MCP 凭据由后端环境变量管理，浏览器不保存密钥。此处只展示去敏后的连接信息。</p>
        {error && <div className="error-banner" role="alert"><span>{error}</span></div>}
        {!info && !error && <p className="settings-intro">正在读取服务端配置…</p>}
        {info && <>
          <div className="settings-group">
            <h2>模型服务</h2>
            <label className="settings-field"><span>Provider</span><input readOnly value={info.model.provider} /></label>
            {Object.entries(info.model.tiers).map(([tier, model]) => (
              <label className="settings-field" key={tier}>
                <span>模型 ID（{tier}）</span><input readOnly value={model} />
              </label>
            ))}
            <label className="settings-field"><span>Base URL</span><input readOnly value={info.model.baseUrl} /></label>
          </div>
          <div className="settings-group">
            <h2>存储服务</h2>
            <label className="settings-field"><span>Store</span><input readOnly value={info.storage.url} /></label>
            <label className="settings-field"><span>工作区</span><input readOnly value={`${info.workspace}（会话 ${info.sessionCwd}）`} /></label>
          </div>
          <div className="settings-group">
            <h2>远程 MCP</h2>
            {info.mcp.length === 0
              ? <p className="settings-intro">未配置远程 MCP server；本轮执行没有文件、终端或联网工具。</p>
              : info.mcp.map((server) => (
                <label className="settings-field" key={server.name}>
                  <span>{server.name}</span><input readOnly value={server.url} />
                </label>
              ))}
          </div>
        </>}
      </div>
    </div>
  );
}
