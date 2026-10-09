import { describe, expect, test } from 'bun:test';
import {
  failureLabels, formatBytes, formatDuration, formatStartupMs, lifetimeMs, memoryPercent,
  phaseLabels, relativeTime, startupScale, startupStages, summarize,
} from './format';
import { wasmInstancePhases } from '../../shared/resources';
import type { ChatInstanceObservation } from '../../shared/instances';
import { CF_WASM_MAX_MEMORY_BYTES } from '../../shared/runtime-limits';
import { WasmResources } from '../../worker/wasm/resources';
import { memoryInstance } from '../../tests/helpers/wasm';

const chat = { id: '00000000-0000-4000-8000-000000000001', title: '会话', updatedAt: '2026-10-09T10:00:00.000Z' };

function snapshot(overrides: Record<string, unknown> = {}) {
  const resources = new WasmResources();
  resources.markStartup('module-ready', 12);
  resources.markStartup('native-ready', 240.6);
  resources.attach(memoryInstance());
  resources.ready('generation-1');
  return { ...resources.snapshot(), startedAt: '2026-10-09T10:00:00.000Z', sampledAt: '2026-10-09T10:00:05.000Z', ...overrides };
}

describe('instance monitor formatting', () => {
  test('labels every sampler phase and observation failure', () => {
    for (const phase of wasmInstancePhases) expect(phaseLabels[phase]).toBeTruthy();
    expect(Object.keys(failureLabels).sort()).toEqual(['host-unreachable', 'invalid-observation']);
  });

  test('renders startup stages with a shared scale and never invents missing timings', () => {
    const stages = startupStages(snapshot() as never);
    expect(stages.map((stage) => [stage.key, stage.valueMs])).toEqual([
      ['module', 12], ['native', 241], ['acp', null],
    ]);
    expect(formatStartupMs(241)).toBe('241 ms');
    expect(formatStartupMs(null)).toBe('未记录');
    expect(startupScale([])).toBe(100);
    expect(startupScale([{ chat, observation: { running: false, executionBlocked: false, instance: snapshot() as never }, failure: null }])).toBe(241);
  });

  test('reports linear memory against the per-instance reservation without fake precision', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(65_536)).toBe('64.0 KiB');
    expect(formatBytes(CF_WASM_MAX_MEMORY_BYTES)).toBe('64.00 MiB');
    expect(memoryPercent(CF_WASM_MAX_MEMORY_BYTES)).toBe(100);
    expect(memoryPercent(CF_WASM_MAX_MEMORY_BYTES * 2)).toBe(100);
    expect(memoryPercent(-1)).toBe(0);
  });

  test('keeps durations, lifetime and observation age explicit', () => {
    expect(formatDuration(450)).toBe('450 ms');
    expect(formatDuration(2500)).toBe('2.5 秒');
    expect(formatDuration(90_000)).toBe('1 分 30 秒');
    expect(formatDuration(3_900_000)).toBe('1 小时 5 分');
    const now = Date.parse('2026-10-09T10:00:05.000Z');
    expect(lifetimeMs(snapshot() as never, now)).toBe(5000);
    expect(lifetimeMs(snapshot({ endedAt: '2026-10-09T10:00:02.000Z' }) as never, now)).toBe(2000);
    expect(lifetimeMs(snapshot({ startedAt: 'invalid' }) as never, now)).toBe(0);
    expect(relativeTime('2026-10-09T10:00:05.000Z', now)).toBe('刚刚');
    expect(relativeTime('2026-10-09T09:59:59.000Z', now)).toBe('6 秒前');
    expect(relativeTime('2026-10-09T09:30:00.000Z', now)).toBe('30 分钟前');
    expect(relativeTime('2026-10-09T07:00:00.000Z', now)).toBe('3 小时前');
  });

  test('summarizes observations without counting unobserved chats as idle instances', () => {
    const rows: ChatInstanceObservation[] = [
      { chat, observation: { running: true, executionBlocked: false, instance: snapshot() as never }, failure: null },
      { chat: { ...chat, id: '00000000-0000-4000-8000-000000000002' },
        observation: { running: false, executionBlocked: true, instance: null }, failure: null },
      { chat: { ...chat, id: '00000000-0000-4000-8000-000000000003' }, observation: null, failure: 'host-unreachable' },
    ];
    expect(summarize(rows)).toEqual({ chats: 3, observed: 1, running: 1, blocked: 1, unreachable: 1 });
    expect(summarize([])).toEqual({ chats: 0, observed: 0, running: 0, blocked: 0, unreachable: 0 });
  });
});
