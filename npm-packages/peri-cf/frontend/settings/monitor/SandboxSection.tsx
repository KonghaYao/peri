import type { MonitorSnapshot } from "../../remote";
import { formatElapsed } from "./format";

const SETTINGS_SOURCE_LABEL: Record<MonitorSnapshot["sandbox"]["settings"]["source"], string> = {
  file: "已读取设置文件",
  defaults: "设置文件不存在，使用内置默认值",
  disabled: "sandbox 已禁用",
  error: "设置读取失败",
};

/** Sandbox：配置、生效 profile、磁盘工作区与运行中实例；全部只读。 */
export function SandboxSection({
  sandbox,
  generatedAt,
}: {
  sandbox: MonitorSnapshot["sandbox"];
  generatedAt: string;
}) {
  const { settings } = sandbox;
  return (
    <section className="monitor-section" aria-label="Sandbox 情况">
      <div className="settings-group">
        <h2>配置</h2>
        <div className="monitor-row">
          <span>每会话 sandbox</span>
          <span className="monitor-badge" data-state={sandbox.enabled ? "ready" : "closed"}>
            {sandbox.enabled ? "已启用" : "已禁用"}
          </span>
        </div>
        <div className="monitor-row">
          <span>根目录</span>
          <code className="monitor-code">{sandbox.root}</code>
        </div>
        <div className="monitor-row">
          <span>设置文件</span>
          <code className="monitor-code">{settings.path}</code>
        </div>
        <div className="monitor-row">
          <span>设置来源</span>
          <span className="monitor-value">{SETTINGS_SOURCE_LABEL[settings.source]}</span>
        </div>
        <div className="monitor-row">
          <span>生效 profile</span>
          <span className="monitor-value">{settings.profileName ?? "—"}</span>
        </div>
        <div className="monitor-row">
          <span>磁盘工作区</span>
          <span className="monitor-value">
            {sandbox.workspacesOnDisk === null ? "不可用" : `${sandbox.workspacesOnDisk} 个`}
          </span>
        </div>
        {settings.error === undefined ? null : (
          <p className="settings-problems">不可用：{settings.error}</p>
        )}
        {sandbox.workspacesError === undefined ? null : (
          <p className="settings-problems">工作区读取不可用：{sandbox.workspacesError}</p>
        )}
        {!sandbox.enabled ? <p className="settings-empty">每会话 sandbox 已禁用。</p> : null}
      </div>

      <div className="settings-group">
        <h2>生效 profile</h2>
        {settings.profile === null ? (
          <p className="settings-empty">
            {settings.source === "error" ? "设置读取失败，无法确定生效 profile。" : "未读取到生效 profile。"}
          </p>
        ) : (
          <>
            <div className="monitor-row">
              <span>memoryMax</span>
              <span className="monitor-value">{settings.profile.memoryMax}</span>
            </div>
            <div className="monitor-row">
              <span>tasksMax</span>
              <span className="monitor-value">{settings.profile.tasksMax}</span>
            </div>
            <div className="monitor-row">
              <span>cpuQuota</span>
              <span className="monitor-value">{settings.profile.cpuQuota}</span>
            </div>
            <div className="monitor-row">
              <span>允许域名</span>
              <span className="monitor-value">{settings.profile.allowedDomainsTotal} 个</span>
            </div>
            <div className="monitor-row">
              <span>运行时只读路径</span>
              <span className="monitor-value">{settings.profile.runtimeReadPathsCount} 个</span>
            </div>
            {settings.profile.allowedDomainsTotal === 0 ? null : (
              <details className="monitor-details">
                <summary>查看允许域名</summary>
                <ul className="monitor-list">
                  {settings.profile.allowedDomains.map((domain) => (
                    <li key={domain}><code className="monitor-code">{domain}</code></li>
                  ))}
                </ul>
                {settings.profile.allowedDomainsTotal > settings.profile.allowedDomains.length ? (
                  <p className="settings-hint">
                    仅展示前 {settings.profile.allowedDomains.length} 条，共
                    {" "}{settings.profile.allowedDomainsTotal} 条。
                  </p>
                ) : null}
              </details>
            )}
          </>
        )}
      </div>

      <div className="settings-group">
        <h2>运行实例</h2>
        {sandbox.active.length === 0 ? (
          <p className="settings-empty">当前没有运行中的 sandbox。</p>
        ) : (
          <div className="monitor-table-scroll">
            <table className="monitor-table">
              <thead>
                <tr>
                  <th scope="col">sandboxId</th>
                  <th scope="col">状态</th>
                  <th scope="col">引用数</th>
                  <th scope="col">运行时长</th>
                  <th scope="col">工作区</th>
                  <th scope="col">MCP</th>
                  <th scope="col">最近错误</th>
                </tr>
              </thead>
              <tbody>
                {sandbox.active.map((entry) => (
                  <tr key={entry.sandboxId}>
                    <td><code className="monitor-code">{entry.sandboxId}</code></td>
                    <td><span className="monitor-badge" data-state={entry.state}>{entry.state}</span></td>
                    <td>{entry.refs}</td>
                    <td>{formatElapsed(entry.startedAt, generatedAt)}</td>
                    <td><code className="monitor-code">{entry.workspacePath}</code></td>
                    <td>
                      <code className="monitor-code">{entry.mcpUrl ?? "—"}</code>
                      {entry.mcpServerName === null ? null : (
                        <span className="monitor-muted">（{entry.mcpServerName}）</span>
                      )}
                    </td>
                    <td>
                      {entry.lastError === null
                        ? "无"
                        : <span className="monitor-error-text">{entry.lastError.code}：{entry.lastError.message}</span>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="monitor-row">
          <span>启动中</span>
          <span className="monitor-value">{sandbox.starting} 个</span>
        </div>
      </div>
    </section>
  );
}
