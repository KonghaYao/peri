const adapters = [
  ['getDns(){return nodeSockHelpers.dnsModule??=(process.getBuiltinModule||require)("dns")}',
    'getDns(){return Module["periDns"]??(nodeSockHelpers.dnsModule??=(process.getBuiltinModule||require)("dns"))}'],
  ['getNet(){return nodeSockHelpers.netModule??=(process.getBuiltinModule||require)("net")}',
    'getNet(){return Module["periNet"]??(nodeSockHelpers.netModule??=(process.getBuiltinModule||require)("net"))}'],
  ['if(!sock.bound&&typeof Bun==="undefined")',
    'if(!sock.bound&&typeof Bun==="undefined"&&!Module["periWorkerSockets"])'],
  ['setImmediateWrapped.mapping[id]=setImmediate(',
    'setImmediateWrapped.mapping[id]=(Module["periSetImmediate"]??setImmediate)('],
  ['clearImmediate(handle);setImmediateWrapped.mapping[id]=undefined',
    '(Module["periClearImmediate"]??clearImmediate)(handle);setImmediateWrapped.mapping[id]=undefined'],
] as const;

export function workerGlue(source: string): string {
  for (const [original, replacement] of adapters) {
    if (source.split(original).length !== 2)
      throw new Error("Unsupported SDK WASM glue; review the Workers platform adapter before preparing artifacts");
    source = source.replace(original, replacement);
  }
  return source;
}
