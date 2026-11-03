import type { MonitorSnapshot } from "../../remote";

/** LLM 配置：供应商、活动档位、四档模型与去敏 Base URL；不展示 API Key。 */
export function LlmSection({ llm }: { llm: MonitorSnapshot["llm"] }) {
  return (
    <section className="monitor-section" aria-label="LLM 配置">
      <div className="settings-group">
        <h2>模型服务</h2>
        <div className="monitor-row">
          <span>Provider</span>
          <span className="monitor-value">{llm.provider}</span>
        </div>
        <div className="monitor-row">
          <span>当前档位</span>
          <span className="monitor-value">
            {llm.activeAlias} <span className="monitor-badge" data-tone="active">默认</span>
          </span>
        </div>
        <div className="monitor-row">
          <span>Base URL</span>
          <code className="monitor-code">{llm.baseUrl || "—"}</code>
        </div>
      </div>

      <div className="settings-group">
        <h2>四档模型</h2>
        <table className="monitor-table">
          <thead>
            <tr><th scope="col">档位</th><th scope="col">模型 ID</th></tr>
          </thead>
          <tbody>
            {Object.entries(llm.tiers).map(([tier, model]) => (
              <tr key={tier}>
                <td>{tier}</td>
                <td><code className="monitor-code">{model}</code></td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <p className="settings-intro">API Key 只在服务端进程内，不展示；修改模型配置需重启服务端。</p>
    </section>
  );
}
