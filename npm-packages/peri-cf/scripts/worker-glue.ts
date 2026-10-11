/**
 * 宿主平台适配：peri-cf 的 wasm 产物来自带 `-sNODERAWSOCKETS` 的链接配置（tokio::net 链路所需），
 * 该配置面向 Node 兼容宿主（Bun/Node/workerd + nodejs_compat）。这里按宿主注入端口改造胶水，
 * 使浏览器宿主可用；每个替换点必须精确命中一次，否则构建直接失败而不是产出坏运行时。
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
  // `-sNODERAWSOCKETS` 是 tokio::net 链路所需（见 patches/README.md），但它断言运行时是 Node：
  // 浏览器宿主不满足该断言，模块在加载期即抛错。宿主不使用 socket（Store 与模型都走 host fetch），
  // 去掉这道环境门；一旦有代码真的走到 socket 路径，getDns/getNet 的后备分支会因缺少 Node 模块立刻失败。
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
