export function memoryInstance(): WebAssembly.Instance {
  return new WebAssembly.Instance(new WebAssembly.Module(new Uint8Array([
    0, 97, 115, 109, 1, 0, 0, 0,
    5, 4, 1, 1, 1, 8,
    7, 5, 1, 1, 109, 2, 0,
  ])));
}
