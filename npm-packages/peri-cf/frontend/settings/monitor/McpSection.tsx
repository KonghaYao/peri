import type { MonitorSnapshot } from "../../remote";

/**
 * MCP 配置：已配置远程 server（只展示 header 名称）与运行中的 sandbox MCP。
 *
 * 没有连通性探测，因此不声称 server 可用；sandbox MCP 只来自池快照。
 */
export function McpSection({ mcp }: { mcp: MonitorSnapshot["mcp"] }) {
  return (
    <section className="monitor-section" aria-label="MCP 配置">
      <div className="settings-group">
        <h2>远程 MCP server</h2>
        {mcp.remote.length === 0 ? (
          <p className="settings-empty">未配置远程 MCP server。</p>
        ) : (
          <table className="monitor-table">
            <thead>
              <tr>
                <th scope="col">名称</th>
                <th scope="col">URL</th>
                <th scope="col">Header 名称</th>
              </tr>
            </thead>
            <tbody>
              {mcp.remote.map((server) => (
                <tr key={server.name}>
                  <td>{server.name}</td>
                  <td><code className="monitor-code">{server.url || "—"}</code></td>
                  <td>{server.headerNames.length === 0 ? "无" : server.headerNames.join(", ")}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="settings-hint">
          远程 MCP 在创建会话时注入；修改配置需重启服务端。此处只展示配置，未做连通性探测。
        </p>
      </div>

      <div className="settings-group">
        <h2>会话 sandbox MCP</h2>
        {!mcp.sandboxEnabled ? (
          <p className="settings-empty">每会话 sandbox 已禁用。</p>
        ) : mcp.sandbox.length === 0 ? (
          <p className="settings-empty">当前没有运行中的 sandbox MCP。</p>
        ) : (
          <table className="monitor-table">
            <thead>
              <tr>
                <th scope="col">sandboxId</th>
                <th scope="col">状态</th>
                <th scope="col">URL</th>
                <th scope="col">名称</th>
              </tr>
            </thead>
            <tbody>
              {mcp.sandbox.map((server) => (
                <tr key={server.sandboxId}>
                  <td><code className="monitor-code">{server.sandboxId}</code></td>
                  <td><span className="monitor-badge" data-state={server.state}>{server.state}</span></td>
                  <td><code className="monitor-code">{server.url ?? "—"}</code></td>
                  <td>{server.name}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="settings-hint">
          状态来自 sandbox 句柄快照；读取不会启动 sandbox，也未做连通性探测。
        </p>
      </div>
    </section>
  );
}
