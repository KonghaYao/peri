import { expect, test } from "bun:test";
import { WasmBudget, WasmCapacityError } from "../worker/wasm/budget";

test("one maximal host reserves the isolate budget until confirmed cleanup", () => {
  const budget = new WasmBudget();
  const first = budget.acquire();
  expect(budget.reservedBytes).toBe(67_108_864);
  expect(() => budget.acquire()).toThrow(WasmCapacityError);
  first.releaseAfterCleanup();
  expect(budget.reservedBytes).toBe(0);
  const second = budget.acquire();
  first.releaseAfterCleanup();
  expect(budget.reservedBytes).toBe(67_108_864);
  second.releaseAfterCleanup();
});

test("independent isolate accounting does not release another isolate's lease", () => {
  const first = new WasmBudget();
  const second = new WasmBudget();
  const firstLease = first.acquire();
  const secondLease = second.acquire();
  firstLease.releaseAfterCleanup();
  expect(() => second.acquire()).toThrow(WasmCapacityError);
  secondLease.releaseAfterCleanup();
});
