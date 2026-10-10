import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { workerRuntimeGlue } from "../scripts/worker-runtime-glue";

const raw = await readFile(resolve(import.meta.dir, "../../@peri-sdk/dist/wasm/peri-wasm.js"), "utf8");
const sites = [
  "safeSetTimeout.mapping[id]=setTimeout(",
  "clearTimeout(handle);safeSetTimeout.mapping[id]=undefined",
  "globalThis.setTimeout(arg0,arg1>>>0)",
  "globalThis.clearTimeout(arg0)",
];

describe("guarded Workers-owned WASM timer adapters", () => {
  test("routes both Emscripten and Rust timers without changing the SDK source", () => {
    const result = workerRuntimeGlue(raw);
    expect(result.match(/Module\["periSetTimeout"\]/g)?.length).toBe(2);
    expect(result.match(/Module\["periClearTimeout"\]/g)?.length).toBe(2);
    for (const site of sites) expect(result).not.toContain(site);
    expect(raw).not.toContain('Module["periSetTimeout"]');
    expect(() => workerRuntimeGlue(result)).toThrow("Unsupported SDK WASM glue");
  });

  test("fails closed when timer call sites disappear or multiply", () => {
    for (const site of sites) {
      expect(() => workerRuntimeGlue(raw.replace(site, "changed_timer_site"))).toThrow("Unsupported WASM timer glue");
      expect(() => workerRuntimeGlue(`${raw}${site}`)).toThrow("Unsupported WASM timer glue");
    }
  });

  test("Emscripten schedule and clear use the instance scheduler and retain callback behavior", () => {
    const transformed = workerRuntimeGlue(raw);
    const helpers = transformed.match(/var safeSetTimeout=.*?;var _emscripten_clear_timeout=/)?.[0];
    expect(helpers).toBeDefined();
    const scheduled: Array<{ callback: () => void; delay: number }> = [];
    const cancelled: unknown[] = [];
    let fired = false;
    const create = new Function("Module", "callUserCallback", `${helpers!.replace(/;var _emscripten_clear_timeout=$/, ";")}return {schedule:safeSetTimeout,clear:safeClearTimeout};`);
    const timer = create({
      periSetTimeout: (callback: () => void, delay: number) => { scheduled.push({ callback, delay }); return 71; },
      periClearTimeout: (handle: unknown) => cancelled.push(handle),
    }, (callback: () => void) => callback());
    const first = timer.schedule(() => { fired = true; }, 25);
    expect(scheduled[0].delay).toBe(25);
    timer.clear(first);
    expect(cancelled).toEqual([71]);
    timer.schedule(() => { fired = true; }, 100);
    scheduled[1].callback();
    expect(fired).toBe(true);
  });

  test("Rust timer imports use the instance scheduler rather than global timers", () => {
    const transformed = workerRuntimeGlue(raw);
    const create = new Function("Module", `${transformed.match(/function ___wbg_setTimeout_\w+\(arg0,arg1\)\{[^}]+\}/)![0]}\n${transformed.match(/function ___wbg_clearTimeout_\w+\(arg0\)\{[^}]+\}/)![0]}\nreturn {set:${transformed.match(/function (___wbg_setTimeout_\w+)\(/)![1]},clear:${transformed.match(/function (___wbg_clearTimeout_\w+)\(/)![1]}};`);
    const operations: unknown[] = [];
    const timer = create({
      periSetTimeout: (_callback: () => void, delay: number) => { operations.push(delay); return 91; },
      periClearTimeout: (handle: unknown) => operations.push(handle),
    });
    const handle = timer.set(() => {}, 50);
    timer.clear(handle);
    expect(operations).toEqual([50, 91]);
  });
});
