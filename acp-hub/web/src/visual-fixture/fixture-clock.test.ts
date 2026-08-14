import { expect, it } from 'vitest';
import { acquireFixtureClock } from './fixture-clock';
import { vi } from 'vitest';

it('supports nested and out-of-order fixture clock leases', () => {
  const hostNow = Date.now;
  const releaseFirst = acquireFixtureClock(101);
  const releaseSecond = acquireFixtureClock(202);
  expect(Date.now()).toBe(202);
  releaseFirst();
  expect(Date.now()).toBe(202);
  releaseSecond();
  expect(Date.now).toBe(hostNow);
});

it('makes duplicate release harmless', () => {
  const hostNow = Date.now;
  const release = acquireFixtureClock(303);
  expect(Date.now()).toBe(303);
  release();
  release();
  expect(Date.now).toBe(hostNow);
});

it('shares the original host clock and lease stack across module reloads', async () => {
  const hostNow = Date.now;
  const releaseBeforeReload = acquireFixtureClock(404);
  vi.resetModules();
  const reloaded = await import('./fixture-clock');
  const releaseAfterReload = reloaded.acquireFixtureClock(505);
  expect(Date.now()).toBe(505);
  releaseBeforeReload();
  expect(Date.now()).toBe(505);
  releaseAfterReload();
  expect(Date.now).toBe(hostNow);
});
