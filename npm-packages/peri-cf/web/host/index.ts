import {
  DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, createSchedulerPort, startPeriWasmHost, type WasmAcpTransport,
} from "@peri-code/sdk/wasm";
import type { BrowserConfig } from "../settings/config";
import { harnessConfigJson } from "./config";
import { loadPeriWasmModule } from "./module";

/**
 * 浏览器 ACP 宿主：每标签页一个实例，生命周期与页面一致（刷新/关闭即结束，在途执行丢失，
 * 展示态由下一次 `session/load` 重建）。配置由调用方在启动时注入（页面启动时从 VFS 读一次），
 * 改动刷新后生效；只注入调度器端口：网络与 DNS 属于 Node 宿主，浏览器的模型与存储流量经
 * fetch 直达同源 relay。
 */

export interface BrowserHostOptions {
  signal?: AbortSignal;
  /** 页面启动时读到的浏览器配置；宿主不自行读盘。 */
  config: BrowserConfig;
  /** 同源 base；测试可注入。 */
  origin?: string;
}

export async function startBrowserHost(options: BrowserHostOptions): Promise<WasmAcpTransport> {
  const configJson = harnessConfigJson(options.config, options.origin ?? globalThis.location.origin);
  const scheduler = createSchedulerPort();
  return startPeriWasmHost({
    configJson,
    signal: options.signal,
    cleanupTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
    ports: { scheduler },
    moduleFactory: (moduleOptions) => loadPeriWasmModule(moduleOptions, options.signal),
    onDiagnostic: (kind, error) => console.error("Peri browser host diagnostic", { kind }, error),
  });
}
