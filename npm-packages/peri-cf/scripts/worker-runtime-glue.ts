import { workerGlue } from "./worker-glue";

const timers = [
  ['safeSetTimeout.mapping[id]=setTimeout(',
    'safeSetTimeout.mapping[id]=(Module["periSetTimeout"]??setTimeout)('],
  ['clearTimeout(handle);safeSetTimeout.mapping[id]=undefined',
    '(Module["periClearTimeout"]??clearTimeout)(handle);safeSetTimeout.mapping[id]=undefined'],
  ['globalThis.setTimeout(arg0,arg1>>>0)',
    '(Module["periSetTimeout"]??globalThis.setTimeout)(arg0,arg1>>>0)'],
  ['globalThis.clearTimeout(arg0)',
    '(Module["periClearTimeout"]??globalThis.clearTimeout)(arg0)'],
] as const;

export function workerRuntimeGlue(source: string): string {
  source = workerGlue(source);
  for (const [original, replacement] of timers) {
    if (source.split(original).length !== 2)
      throw new Error("Unsupported WASM timer glue; review the Workers owned scheduler before preparing artifacts");
    source = source.replace(original, replacement);
  }
  return source;
}
