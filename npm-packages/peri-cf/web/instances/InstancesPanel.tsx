import { useEffect, useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Activity, ArrowLeft, MessageSquare, RefreshCw } from 'lucide-react';
import { InstancesApi } from './api';
import { createInstanceQueries } from './queries';
import {
  failureLabels, formatBytes, formatClock, formatDuration, formatStartupMs, lifetimeMs,
  memoryPercent, phaseLabels, relativeTime, startupScale, startupStages, summarize,
} from './format';
import { errorText } from '../api/client';
import { CF_WASM_MAX_MEMORY_BYTES } from '../../shared/runtime-limits';
import type { ChatInstanceObservation } from '../../shared/instances';
import type { WasmResourceSnapshot } from '../../shared/resources';

const REFRESH_INTERVAL_MS = 3000;

export function InstancesPanel({ token, onClose, onOpenChat }: {
  token: string;
  onClose: () => void;
  onOpenChat: (chatId: string) => void;
}) {
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [now, setNow] = useState(() => Date.now());
  const api = useMemo(() => new InstancesApi(token), [token]);
  const queries = useMemo(() => createInstanceQueries(api), [api]);
  const list = useQuery({
    ...queries.list(autoRefresh ? REFRESH_INTERVAL_MS : false),
    enabled: Boolean(token),
  }, queries.client);

  useEffect(() => {
    queries.client.mount();
    return () => {
      queries.client.unmount();
      void queries.dispose();
    };
  }, [queries]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const observations = list.data?.instances ?? [];
  const summary = summarize(observations);
  const scale = startupScale(observations);

  return (
    <section className="instances-panel" aria-label="实例监控">
      <header className="instances-topbar">
        <button type="button" className="icon-button" onClick={onClose} aria-label="返回会话"><ArrowLeft size={19} /></button>
        <div className="instances-heading"><Activity size={17} /><strong>实例监控</strong>
          <span>WASM 实例 · 启动耗时 · 线性内存 · 挂载会话</span></div>
        <label className="instances-auto"><input type="checkbox" checked={autoRefresh}
          onChange={(event) => setAutoRefresh(event.target.checked)} />每 3 秒自动刷新</label>
        <button type="button" className="icon-button" aria-label="立即刷新" disabled={!token || list.isFetching}
          onClick={() => void list.refetch()}><RefreshCw size={17} className={list.isFetching ? 'spinning' : ''} /></button>
      </header>
      {!token && <div className="error-banner" role="status"><span>尚未配置访问令牌，无法读取实例观测。</span></div>}
      {list.error && <div className="error-banner" role="alert"><span>{errorText(list.error)}</span></div>}
      <div className="instances-body">
        <div className="instances-summary">
          <Stat label="会话" value={summary.chats} />
          <Stat label="有实例观测" value={summary.observed} />
          <Stat label="执行中" value={summary.running} tone={summary.running > 0 ? 'active' : undefined} />
          <Stat label="准入阻塞" value={summary.blocked} tone={summary.blocked > 0 ? 'blocked' : undefined} />
          <Stat label="读取失败" value={summary.unreachable} tone={summary.unreachable > 0 ? 'blocked' : undefined} />
        </div>
        {list.isLoading && token
          ? <div className="loading-state" role="status"><RefreshCw className="spinning" size={20} /><p>正在读取实例观测…</p></div>
          : observations.length === 0
            ? <div className="instances-empty">{token ? '还没有可观测的会话。' : '配置访问令牌后可查看实例观测。'}</div>
            : observations.map((entry) => <InstanceCard key={entry.chat.id} entry={entry} now={now}
              scale={scale} onOpenChat={onOpenChat} />)}
        <p className="instances-footnote">观测读取各聊天 DO 的内存采样器：会话从未启动过宿主、宿主不可达或 DO 已回收时不显示实例，也不把历史观测当作存活状态。</p>
      </div>
    </section>
  );
}

function Stat({ label, value, tone }: { label: string; value: number; tone?: 'active' | 'blocked' }) {
  return <div className={`instances-stat ${tone ?? ''}`}><strong>{value}</strong><span>{label}</span></div>;
}

function InstanceCard({ entry, now, scale, onOpenChat }: {
  entry: ChatInstanceObservation;
  now: number;
  scale: number;
  onOpenChat: (chatId: string) => void;
}) {
  const instance = entry.observation?.instance ?? null;
  return (
    <article className="instance-card">
      <div className="instance-head">
        <div className="instance-identity">
          <h3>{entry.chat.title}</h3>
          <code title={entry.chat.id}>{entry.chat.id.slice(0, 8)}</code>
        </div>
        <div className="instance-badges">
          {entry.observation?.running && <span className="instance-badge running">执行中</span>}
          {entry.observation?.executionBlocked && <span className="instance-badge blocked">准入阻塞</span>}
          {instance && <span className={`instance-badge phase-${instance.phase}`}>{phaseLabels[instance.phase]}</span>}
        </div>
        <button type="button" className="text-button" onClick={() => onOpenChat(entry.chat.id)}>
          <MessageSquare size={14} />打开会话</button>
      </div>
      {entry.failure
        ? <p className="instance-missing">{failureLabels[entry.failure]}</p>
        : !instance
          ? <p className="instance-missing">该会话尚无 WASM 实例观测。</p>
          : <div className="instance-details">
            <div className="instance-startup">
              {startupStages(instance).map((stage) => <div className="startup-row" key={stage.key}>
                <span className="startup-label">{stage.label}</span>
                <span className="startup-track" role="presentation">
                  <span className="startup-fill" style={{ width: `${stage.valueMs === null ? 0 : Math.max(1.5, (stage.valueMs / scale) * 100)}%` }} />
                </span>
                <span className="startup-value">{formatStartupMs(stage.valueMs)}</span>
              </div>)}
            </div>
            <div className="instance-facts">
              <MemoryFact instance={instance} />
              <span>实例 <code>{instance.instanceId.slice(0, 8)}</code></span>
              <span>SDK 代次 <code>{instance.generationId ? instance.generationId.slice(0, 12) : '未就绪'}</code></span>
              <span>启动于 {formatClock(instance.startedAt)} · 生命周期 {formatDuration(lifetimeMs(instance, now))}
                {instance.endedAt ? `（结束于 ${formatClock(instance.endedAt)}）` : ''}</span>
              <span>最后观测 {relativeTime(instance.sampledAt, now)}</span>
              {instance.memory?.observation === 'last-observed' && <span className="instance-note">内存为关闭前最后观测，不代表当前存活</span>}
            </div>
          </div>}
    </article>
  );
}

function MemoryFact({ instance }: { instance: WasmResourceSnapshot }) {
  const memory = instance.memory;
  if (!memory) return <span>线性内存未采样</span>;
  return (
    <span className="instance-memory">
      线性内存 {formatBytes(memory.allocatedBytes)}
      <span className="memory-track" role="presentation">
        <span className="memory-fill" style={{ width: `${memoryPercent(memory.allocatedBytes)}%` }} />
        <span className="memory-peak" style={{ left: `${memoryPercent(memory.peakObservedBytes)}%` }} />
      </span>
      峰值 {formatBytes(memory.peakObservedBytes)} · 预留上限 {formatBytes(CF_WASM_MAX_MEMORY_BYTES)}
    </span>
  );
}
