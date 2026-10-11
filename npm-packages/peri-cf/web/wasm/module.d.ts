declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}

declare module "*generated/peri-wasm.js" {
  const factory: (options: Record<string, unknown>) => Promise<{
    PeriWasmAcp: {
      start(configJson: string): Promise<{
        send(frame: string): Promise<void>;
        recv(): Promise<string | null | undefined>;
        close(): Promise<void>;
        free(): void;
      }>;
    };
  }>;
  export default factory;
}
