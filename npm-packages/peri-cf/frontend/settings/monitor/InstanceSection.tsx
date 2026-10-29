import { useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { MonitorSnapshot } from "../../remote";
import {
  formatBytes,
  formatDateTime,
  formatElapsed,
  formatRelativeTime,
  formatTokens,
  formatWallMs,
} from "./format";

type Wasm = MonitorSnapshot["wasm"];
type WasmInstance = Wasm["instances"][number];

const STATE_LABEL: Record<WasmInstance["state"], string> = {
  live: "空闲",
  closing: "关闭中",
  closed: "已关闭",
  "close-unconfirmed": "关闭未确认",
};

function shortId(id: string): string {
  return id.length <= 14 ? id : `${id.slice(0, 10)}…`;
}

function stateLabel(instance: WasmInstance): string {
  if (instance.state === "live" && instance.running) return "执行中";
  return STATE_LABEL[instance.state];
}

function stateName(instance: WasmInstance): string {
  if (instance.state === "live") return instance.running ? "ready" : "idle";
  if (instance.state === "closing") return "stopping";
  if (instance.state === "close-unconfirmed") return "failed";
  return "closed";
}

function Metric({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="monitor-metric">
      <span className="monitor-metric-label">{label}</span>
      <strong className="monitor-metric-value">{value}</strong>
      {hint === undefined ? null : <span className="monitor-metric-hint">{hint}</span>}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="monitor-row">
      <span>{label}</span>
      <span className="monitor-value">{value}</span>
    </div>
  );
}

function CodeRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="monitor-row">
      <span>{label}</span>
      <code className="monitor-code">{value}</code>
    </div>
  );
}

/** 选中行的详情；所有数值都来自快照，不读取实例内容。 */
function InstanceDetail({ instance, wasm, generatedAt }: {
  instance: WasmInstance;
  wasm: Wasm;
  generatedAt: string;
}) {
  const businessAvailable = instance.statsError !== true;
  return (
    <div className="instance-detail" role="region" aria-label="实例详情">
      <div className="settings-group">
        <h2>身份</h2>
        <CodeRow label="Chat" value={instance.chatId} />
        <CodeRow label="Session" value={instance.sessionId ?? "未绑定"} />
        <CodeRow label="Generation" value={instance.generationId} />
        <Row label="启动时间" value={formatDateTime(instance.startedAt)} />
        <Row label="运行时长" value={formatElapsed(instance.startedAt, generatedAt)} />
        <Row
          label="状态"
          value={instance.running
            ? "执行中"
            : STATE_LABEL[instance.state]}
        />
      </div>

      {instance.statsError === true ? (
        <p className="settings-problems">业务计数不可用（事实流订阅失败或已关闭）。</p>
      ) : null}

      <div className="settings-group">
        <h2>资源</h2>
        <Row
          label="线性内存"
          value={instance.linearMemoryBytes === null ? "不可用" : formatBytes(instance.linearMemoryBytes)}
        />
        <Row label="进程 ArrayBuffer 合计" value={formatBytes(wasm.process.arrayBufferBytes)} />
        <Row label="进程 RSS" value={formatBytes(wasm.process.rssBytes)} />
        <p className="settings-hint">
          线性内存是 WebAssembly 实例的当前容量，不是活跃堆；ArrayBuffer/RSS 是进程合计，不能拆分到实例。
        </p>
      </div>

      <div className="settings-group">
        <h2>回合</h2>
        <Row label="开始" value={String(instance.turns.started)} />
        <Row label="完成" value={String(instance.turns.completed)} />
        <Row label="取消" value={String(instance.turns.cancelled)} />
        <Row label="错误" value={String(instance.turns.error)} />
        <Row label="累计墙钟" value={formatWallMs(instance.turns.totalWallMs)} />
        <Row label="最近回合墙钟" value={formatWallMs(instance.turns.lastWallMs)} />
        <Row label="当前回合墙钟" value={formatWallMs(instance.turns.activeWallMs)} />
        <Row label="最近 stopReason" value={instance.lastStopReason ?? "—"} />
        <p className="settings-hint">墙钟不是 CPU 时间：回合大部分时间在等待 LLM 或工具。</p>
      </div>

      <div className="settings-group">
        <h2>Token</h2>
        {businessAvailable && instance.tokens !== null ? (
          <>
            <Row label="LLM 调用" value={String(instance.tokens.llmCalls)} />
            <Row label="输入 token" value={formatTokens(instance.tokens.inputTokens)} />
            <Row label="输出 token" value={formatTokens(instance.tokens.outputTokens)} />
            <Row
              label="其中子代理调用"
              value={String(instance.tokens.auxiliaryCalls)}
            />
            <Row
              label="上下文占用"
              value={instance.context === null
                ? "不可用"
                : `${formatTokens(instance.context.used)} / ${formatTokens(instance.context.size)}`}
            />
            {instance.tokens.partial ? (
              <p className="settings-hint">部分调用未上报 token 数值，累计值不完整。</p>
            ) : null}
          </>
        ) : (
          <p className="settings-empty">尚未收到 token 上报。</p>
        )}
        <p className="settings-hint">Token 是 ACP 上报的计数，不是账单。</p>
      </div>

      <div className="settings-group">
        <h2>工具</h2>
        {businessAvailable && instance.tools !== null ? (
          <>
            <Row label="开始" value={String(instance.tools.started)} />
            <Row label="完成" value={String(instance.tools.completed)} />
            <Row label="失败" value={String(instance.tools.failed)} />
            <Row label="取消" value={String(instance.tools.cancelled)} />
          </>
        ) : (
          <p className="settings-empty">尚未收到工具调用。</p>
        )}
        <p className="settings-hint">只统计次数，不采集工具标题、参数或结果。</p>
      </div>

      <div className="settings-group">
        <h2>队列</h2>
        <Row label="等待输入" value={String(instance.queueDepth)} />
        <Row label="待交互" value={String(instance.pendingInteractions)} />
        <Row label="最近活动" value={formatRelativeTime(instance.lastActivityAt, generatedAt)} />
      </div>
    </div>
  );
}

/**
 * WASM 实例：macOS 系统设置式列表 + 详情。
 *
 * 只展示服务端已去敏的数值与标识；不显示 prompt、消息、工具内容或错误原文。
 * 线性内存是容量、回合时长是墙钟、token 不是账单，三处均在 UI 中标注。
 */
export function InstanceSection({
  wasm,
  generatedAt,
}: {
  wasm: Wasm;
  generatedAt: string;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const keyOf = (instance: WasmInstance): string => `${instance.chatId}:${instance.generationId}`;
  const effective = wasm.instances.find((instance) => keyOf(instance) === selected)
    ?? wasm.instances[0]
    ?? null;
  const running = wasm.instances.filter((instance) => instance.running).length;
  const memoryHint = wasm.linearMemoryUnknownInstances > 0
    ? `${wasm.linearMemoryUnknownInstances} 个实例不可读`
    : "当前容量，不是活跃堆";

  const select = (instance: WasmInstance): void => setSelected(keyOf(instance));
  const onKeyDown = (event: ReactKeyboardEvent<HTMLTableRowElement>, instance: WasmInstance): void => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    select(instance);
  };

  return (
    <section className="monitor-section" aria-label="WASM 实例">
      <div className="monitor-metrics">
        <Metric
          label="运行实例数"
          value={`${wasm.instances.length} 个`}
          hint={wasm.truncated ? "超过上限，仅展示前 200 条" : "每个 Chat 最多一个当前实例"}
        />
        <Metric label="执行中" value={`${running} 个`} />
        <Metric
          label="线性内存合计"
          value={wasm.linearMemoryTotalBytes === null
            ? "不可用"
            : formatBytes(wasm.linearMemoryTotalBytes)}
          hint={memoryHint}
        />
        <Metric
          label="进程 RSS"
          value={formatBytes(wasm.process.rssBytes)}
          hint="全部实例共享，仅作上下文"
        />
      </div>

      {wasm.instancesError === undefined ? null : (
        <p className="settings-problems" role="alert">实例指标读取失败：{wasm.instancesError}</p>
      )}
      {wasm.unconfirmed ? (
        <p className="settings-problems" role="alert">
          有实例关闭未确认；对应行会保留到进程关闭前。
        </p>
      ) : null}
      {wasm.startingInstances > 0 ? (
        <p className="settings-hint">正在启动 {wasm.startingInstances} 个实例；启动完成后才会出现归属行。</p>
      ) : null}

      {wasm.instances.length === 0 ? (
        <div className="settings-group">
          <p className="settings-empty">当前没有运行中的 WASM 实例。</p>
          <p className="settings-hint">
            实例在首次执行（创建/加载会话）时懒启动。当前有 {wasm.chatHandles} 个 Chat 句柄。
          </p>
        </div>
      ) : (
        <div className="instance-layout">
          <div className="instance-list">
            <div className="settings-group">
              <h2>实例列表</h2>
              <div className="monitor-table-scroll">
                <table className="monitor-table" aria-label="WASM 实例">
                  <thead>
                    <tr>
                      <th scope="col">实例</th>
                      <th scope="col">状态</th>
                      <th scope="col">线性内存</th>
                      <th scope="col">运行时长</th>
                      <th scope="col">最近活动</th>
                      <th scope="col">轮次</th>
                    </tr>
                  </thead>
                  <tbody>
                    {wasm.instances.map((instance) => {
                      const key = keyOf(instance);
                      return (
                        <tr
                          key={key}
                          className="instance-row"
                          aria-selected={effective !== null && keyOf(effective) === key}
                          tabIndex={0}
                          onClick={() => select(instance)}
                          onKeyDown={(event) => onKeyDown(event, instance)}
                        >
                          <td>
                            <code className="monitor-code">
                              {shortId(instance.sessionId ?? "未绑定会话")}
                            </code>
                            <span className="monitor-muted"> · {shortId(instance.chatId)}</span>
                          </td>
                          <td>
                            <span className="monitor-badge" data-state={stateName(instance)}>
                              {stateLabel(instance)}
                            </span>
                          </td>
                          <td>
                            {instance.linearMemoryBytes === null
                              ? "不可用"
                              : formatBytes(instance.linearMemoryBytes)}
                          </td>
                          <td>{formatElapsed(instance.startedAt, generatedAt)}</td>
                          <td>{formatRelativeTime(instance.lastActivityAt, generatedAt)}</td>
                          <td>{`${instance.turns.started} / ${instance.turns.error}`}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
              <p className="settings-hint">
                轮次列为开始/错误数；点击行查看详情。列表不含 prompt、消息或工具内容。
              </p>
            </div>
          </div>

          {effective === null ? null : (
            <InstanceDetail instance={effective} wasm={wasm} generatedAt={generatedAt} />
          )}
        </div>
      )}
    </section>
  );
}
