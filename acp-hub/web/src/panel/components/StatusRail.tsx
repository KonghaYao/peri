// 状态 rail：连接状态机 / 心跳 / registry / 订阅 / 最近 ack / 最近错误。

import { For } from 'solid-js';
import {
  ackLog,
  connState,
  errorLog,
  globalStatus,
  heartbeatCount,
  subscribedDocs,
} from '../store';

const KIND_CLASS: Record<string, string> = {
  ok: 'bg-emerald-100 text-emerald-700',
  warn: 'bg-amber-100 text-amber-700',
  err: 'bg-red-100 text-red-700',
  neutral: 'bg-slate-200 text-slate-600',
};

export function StatusRail() {
  const connKind = () => (KIND_CLASS[connState().kind] ? connState().kind : 'neutral');
  const globalKind = () =>
    globalStatus() === 'healthy' ? 'ok' : globalStatus() === 'degraded' ? 'warn' : 'err';

  return (
    <>
      <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
        <h2 class="mb-2 text-sm font-semibold">状态</h2>
        <dl class="m-0 text-xs">
          <dt class="mt-1.5 text-slate-500">连接</dt>
          <dd class="mt-0.5">
            <span
              class={`inline-block rounded px-1.5 py-0.5 text-[11px] leading-4 ${KIND_CLASS[connKind()]}`}
            >
              {connState().text}
            </span>
          </dd>
          <dt class="mt-1.5 text-slate-500">心跳</dt>
          <dd class="mt-0.5">{heartbeatCount()}</dd>
          <dt class="mt-1.5 text-slate-500">registry</dt>
          <dd class="mt-0.5">
            <span
              class={`inline-block rounded px-1.5 py-0.5 text-[11px] leading-4 ${globalStatus() ? KIND_CLASS[globalKind()] : KIND_CLASS.neutral}`}
            >
              {globalStatus() || '—'}
            </span>
          </dd>
          <dt class="mt-1.5 text-slate-500">订阅</dt>
          <dd class="mt-0.5 break-all">{subscribedDocs()}</dd>
        </dl>
      </section>

      <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
        <h2 class="mb-2 text-sm font-semibold">最近 ack</h2>
        <ul class="m-0 list-none p-0 font-mono text-xs">
          <For each={ackLog()}>
            {(line) => (
              <li class="py-0.5 text-emerald-700">{line}</li>
            )}
          </For>
        </ul>
      </section>

      <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
        <h2 class="mb-2 text-sm font-semibold">最近错误</h2>
        <ul class="m-0 list-none p-0 font-mono text-xs">
          <For each={errorLog()}>
            {(line) => (
              <li class="py-0.5 text-red-700">{line}</li>
            )}
          </For>
        </ul>
      </section>
    </>
  );
}
