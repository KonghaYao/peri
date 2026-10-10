import { loadPeriWasm, type NativeWasmAcp, type PeriWasmModule } from "../wasm/loader";
import { JsonRpcTransport } from "./json-rpc-transport";

export interface WasmAcpTransportOptions {
  /** Startup settings consumed by peri-acp; ACP messages remain opaque JSON-RPC frames. */
  configJson: string;
  moduleUrl?: string | URL;
  env?: Readonly<Record<string, string>>;
  /** 已初始化的独立模块；环境与宿主能力在实例化时注入。 */
  module?: PeriWasmModule;
}

/** ACP over the in-process Emscripten port. No ACP method is handled here. */
export class WasmAcpTransport extends JsonRpcTransport {
  readonly generationId = crypto.randomUUID();
  readonly executionKind = "wasmHost" as const;
  private terminated = false;
  private constructor(private readonly native: NativeWasmAcp) {
    super((frame) => native.send(frame), async () => {
      await native.close();
      native.free?.();
      this.terminated = true;
    });
    void this.receiveFrames();
  }
  async executionStopped(): Promise<boolean> { return this.terminated; }

  static async start(options: WasmAcpTransportOptions): Promise<WasmAcpTransport> {
    if (options.module && (options.moduleUrl !== undefined || options.env !== undefined))
      throw new TypeError("Injected WASM module cannot be combined with moduleUrl or env");
    const wasm = options.module ?? await loadPeriWasm(options.moduleUrl, options.env);
    if (!wasm.PeriWasmAcp?.start)
      throw new Error("peri-wasm artifact does not expose the ACP Host port; rebuild peri-wasm");
    return new WasmAcpTransport(await wasm.PeriWasmAcp.start(options.configJson));
  }

  /** Injection point for a wire-contract test without a Rust Host. */
  static fromPort(native: NativeWasmAcp): WasmAcpTransport {
    return new WasmAcpTransport(native);
  }

  private async receiveFrames(): Promise<void> {
    try {
      while (true) {
        const frame = await this.native.recv();
        if (frame == null) {
          this.fail(new Error("ACP WASM port closed"));
          await super.close().catch(() => {});
          return;
        }
        this.acceptFrame(frame);
      }
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error("ACP WASM port failed"));
      await super.close().catch(() => {});
    }
  }
}
