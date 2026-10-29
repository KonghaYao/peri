import type { MonitorSnapshot } from "../../remote";
import { formatBytes, formatDateTime } from "./format";

function Metric({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="monitor-metric">
      <span className="monitor-metric-label">{label}</span>
      <strong className="monitor-metric-value">{value}</strong>
      {hint === undefined ? null : <span className="monitor-metric-hint">{hint}</span>}
    </div>
  );
}

/**
 * WASM 内存：只有进程级指标与活动句柄上限。
 *
 * SDK 没有公开每个运行后端的堆占用；这里如实说明，不提供单实例数值。
 */
export function WasmSection({
  wasm,
  generatedAt,
}: {
  wasm: MonitorSnapshot["wasm"];
  generatedAt: string;
}) {
  return (
    <section className="monitor-section" aria-label="WASM 内存">
      <div className="monitor-metrics">
        <Metric label="进程 RSS" value={formatBytes(wasm.process.rssBytes)} />
        <Metric
          label="JS heap 已用"
          value={formatBytes(wasm.process.heapUsedBytes)}
          hint={`总量 ${formatBytes(wasm.process.heapTotalBytes)}`}
        />
        <Metric
          label="ArrayBuffer 合计"
          value={formatBytes(wasm.process.arrayBufferBytes)}
          hint="含全部 WASM 线性内存"
        />
        <Metric label="活动 Chat 句柄" value={`${wasm.activeChats} 个`} hint="运行后端数量上限" />
      </div>

      <div className="settings-group">
        <h2>运行配置</h2>
        <div className="monitor-row">
          <span>WASM 模块</span>
          <code className="monitor-code">{wasm.moduleUrl || "—"}</code>
        </div>
        <div className="monitor-row">
          <span>采样时间</span>
          <span className="monitor-value">{formatDateTime(generatedAt)}</span>
        </div>
      </div>

      <p className="settings-intro">
        SDK 未暴露每个 WASM 运行后端的堆占用，因此没有单实例内存数值。ArrayBuffer 为进程合计，
        包含全部 WASM 线性内存，不能拆分到单个实例。
      </p>
      <p className="settings-intro">
        活动 Chat 句柄是运行后端数量的上限：句柄存在不代表运行后端已启动，运行后端在首次执行时才创建。
      </p>
    </section>
  );
}
