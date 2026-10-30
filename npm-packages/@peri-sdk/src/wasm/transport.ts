import { loadPeriWasm, type NativeWasmAcp, type PeriWasmModule } from "./loader";
import { JsonRpcTransport } from "../transport/json-rpc-transport";

export interface WasmAcpTransportOptions {
  /** Startup settings consumed by peri-acp; ACP messages remain opaque JSON-RPC frames. */
  configJson: string;
  moduleUrl?: string | URL;
  env?: Readonly<Record<string, string>>;
  /** 已初始化的独立模块；环境与宿主能力在实例化时注入。 */
  module?: PeriWasmModule;
}

/** 运行后端自有的只读资源句柄；不参与 ACP 线协议。 */
export interface WasmTransportResources {
  /** Emscripten 导出的线性内存；缺失时 `linearMemoryBytes()` 返回 null。 */
  memory?: WebAssembly.Memory;
}

/** ACP over the in-process Emscripten port. No ACP method is handled here. */
export class WasmAcpTransport extends JsonRpcTransport {
  readonly generationId = crypto.randomUUID();
  readonly executionKind = "wasmHost" as const;
  private terminated = false;
  private constructor(private readonly native: NativeWasmAcp,
    private readonly memory?: WebAssembly.Memory) {
    super((frame) => native.send(frame), async () => {
      await native.close();
      native.free?.();
      this.terminated = true;
    });
    void this.receiveFrames();
  }
  async executionStopped(): Promise<boolean> { return this.terminated; }

  /**
   * 当前线性内存容量（字节）；不是活跃堆，也不含 JS 堆。
   *
   * 已在关闭/终止路径或宿主未暴露 memory 时返回 null；detached buffer 同样降级，
   * 不伪造 0。
   */
  linearMemoryBytes(): number | null {
    if (this.terminated || this.memory === undefined) return null;
    try {
      const bytes = this.memory.buffer.byteLength;
      return Number.isFinite(bytes) ? bytes : null;
    } catch {
      return null;
    }
  }

  static async start(options: WasmAcpTransportOptions): Promise<WasmAcpTransport> {
    if (options.module && (options.moduleUrl !== undefined || options.env !== undefined))
      throw new TypeError("Injected WASM module cannot be combined with moduleUrl or env");
    const wasm = options.module ?? await loadPeriWasm(options.moduleUrl, options.env);
    if (!wasm.PeriWasmAcp?.start)
      throw new Error("peri-wasm artifact does not expose the ACP Host port; rebuild peri-wasm");
    return new WasmAcpTransport(await wasm.PeriWasmAcp.start(options.configJson), wasm.wasmMemory);
  }

  /**
   * 运行后端构造入口：由 `src/wasm/runtime.ts` 用运行后端自有的端口包装出 transport，
   * 统一实例化与 `start` 的关闭/接收帧路径。线协议契约测试也用它注入假端口。
   */
  static fromPort(native: NativeWasmAcp, resources: WasmTransportResources = {}): WasmAcpTransport {
    return new WasmAcpTransport(native, resources.memory);
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
