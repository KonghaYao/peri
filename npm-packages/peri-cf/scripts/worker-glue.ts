/**
 * WASM 胶水适配：产物带 `-sNODERAWSOCKETS`（tokio::net 所需），但浏览器宿主不满足其 Node 断言。
 * 每个替换点必须精确命中一次，否则构建失败而不是产出坏运行时。
 */
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
  // 去掉 Node-only 的 NODERAWSOCKETS 门；宿主不使用 socket，误走 socket 时 getDns/getNet 会立刻失败。
  ['if(!ENVIRONMENT_IS_NODE){throw new Error("NODERAWSOCKETS is currently only supported on Node.js environment.")}',
    '/* peri-cf browser host: Node-only NODERAWSOCKETS gate removed; sockets stay unused. */'],
] as const;

export function workerGlue(source: string): string {
  for (const [original, replacement] of adapters) {
    if (source.split(original).length !== 2)
      throw new Error("Unsupported SDK WASM glue; review the host platform adapter before preparing artifacts");
    source = source.replace(original, replacement);
  }
  return source;
}
