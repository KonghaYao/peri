import { loadPeriWasm, type NativeWasmAcp } from "../wasm/loader";
import { JsonRpcTransport } from "./json-rpc-transport";

export interface WasmAcpTransportOptions {
  /** Startup settings consumed by peri-acp; ACP messages remain opaque JSON-RPC frames. */
  configJson: string;
  moduleUrl?: string | URL;
  env?: Readonly<Record<string, string>>;
}

/** ACP over the in-process Emscripten port. No ACP method is handled here. */
export class WasmAcpTransport extends JsonRpcTransport {
  private constructor(private readonly native: NativeWasmAcp) {
    super((frame) => native.send(frame), async () => {
      await native.close();
      native.free?.();
    });
    void this.receiveFrames();
  }

  static async start(options: WasmAcpTransportOptions): Promise<WasmAcpTransport> {
    const wasm = await loadPeriWasm(options.moduleUrl, options.env);
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
