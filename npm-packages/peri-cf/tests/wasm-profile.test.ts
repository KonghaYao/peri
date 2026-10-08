import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import { buildSettings, cleanBuildEnvironment, DEFAULT_WASM_PROFILE, wasmMemory, wasmProfile } from "../scripts/wasm-profile";
import { CF_WASM_MAX_MEMORY_BYTES, CF_WASM_STACK_BYTES } from "../shared/runtime-limits";
import { capture } from "../scripts/wasm-tools";

const repoRoot = resolve(import.meta.dir, "../../..");

function unsigned(value: number): number[] {
  const bytes = [];
  do {
    const next = value % 128;
    value = Math.floor(value / 128);
    bytes.push(next | (value ? 128 : 0));
  } while (value);
  return bytes;
}

function memoryBinary(minimum: number, maximum?: number, flags = maximum === undefined ? 0 : 1) {
  const section = [1, flags, ...unsigned(minimum), ...(maximum === undefined ? [] : unsigned(maximum))];
  return Uint8Array.from([0, 97, 115, 109, 1, 0, 0, 0, 5, ...unsigned(section.length), ...section]);
}

describe("explicit CF size/speed build profiles", () => {
  test("defaults to conservative size settings and requires an explicit speed choice", async () => {
    expect(wasmProfile([])).toBe(DEFAULT_WASM_PROFILE);
    expect(wasmProfile(["--profile", "cf-wasm-speed"])).toBe("cf-wasm-speed");
    for (const args of [["--release"], ["--profile", "release"], ["--profile", "cf-wasm-size", "extra"]])
      expect(() => wasmProfile(args)).toThrow("Usage");
    const size = await buildSettings(repoRoot, "cf-wasm-size");
    const speed = await buildSettings(repoRoot, "cf-wasm-speed");
    expect(size.cargoProfile).toEqual({ inherits: "release", "opt-level": "z", lto: "thin", "codegen-units": 16 });
    expect(speed.cargoProfile).toEqual({ inherits: "release", "opt-level": 3, lto: "thin", "codegen-units": 16 });
    expect(size.maxMemoryBytes).toBe(CF_WASM_MAX_MEMORY_BYTES);
    expect(size.stackBytes).toBe(CF_WASM_STACK_BYTES);
    expect(size.features).toEqual(["cloudflare"]);
    expect(size.locked).toBe(true);
  });

  test("does not alter the generic SDK release profile", async () => {
    const size = await buildSettings(repoRoot, "cf-wasm-size");
    expect(size.inheritedProfile["opt-level"]).toBe("z");
    expect(size.inheritedProfile.lto).toBe("fat");
    expect(size.inheritedProfile["codegen-units"]).toBe(1);
  });

  test("generic canonical builder keeps its original unbounded default and fixed 8MiB stack", async () => {
    const flags = await capture(["bash", resolve(repoRoot, "scripts/cargo-wasm.sh"), "--print-rustflags"], repoRoot,
      cleanBuildEnvironment(process.env));
    expect(flags).toContain("-sSTACK_SIZE=8388608");
    expect(flags).toContain("-sALLOW_MEMORY_GROWTH");
    expect(flags).not.toContain("-sMAXIMUM_MEMORY=");
    expect(flags).not.toContain("-sMEMORY_GROWTH_LINEAR_STEP=");
  });

  test("removes inherited compiler/profile overrides without copying environment into provenance", () => {
    const original = { PATH: "/toolchain", RUSTFLAGS: "-Copt-level=0", CARGO_ENCODED_RUSTFLAGS: "override",
      CARGO_PROFILE_CF_WASM_SIZE_LTO: "false", CARGO_BUILD_RUSTFLAGS: "override", EMCC_CFLAGS: "-sMAXIMUM_MEMORY=2GB",
      CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER: "other", PERI_WASM_MAX_MEMORY: "2147483648",
      APP_AUTH_TOKEN: "private-server-value" };
    const result = cleanBuildEnvironment(original);
    expect(result).toEqual({ PATH: "/toolchain", APP_AUTH_TOKEN: "private-server-value" });
    expect(original.RUSTFLAGS).toBe("-Copt-level=0");
  });
});

describe("binary memory budget validation", () => {
  test("validates actual memory declaration against the shared runtime constants", () => {
    expect(wasmMemory(memoryBinary(CF_WASM_STACK_BYTES / 65536, CF_WASM_MAX_MEMORY_BYTES / 65536)))
      .toEqual({ initialBytes: CF_WASM_STACK_BYTES, maximumBytes: CF_WASM_MAX_MEMORY_BYTES });
  });

  test("rejects generic 2GiB memory, implicit max, shared memory and oversized initial memory", () => {
    for (const binary of [memoryBinary(420, 32768), memoryBinary(420), memoryBinary(420, 1024, 3),
      memoryBinary(2048, 1024), memoryBinary(1, 1024)])
      expect(() => wasmMemory(binary)).toThrow();
  });

  test("rejects invalid, truncated and missing memory sections", () => {
    for (const binary of [Uint8Array.from([0]), Uint8Array.from([0, 97, 115, 109, 1, 0, 0, 0]),
      memoryBinary(128, 1024).slice(0, -1)])
      expect(() => wasmMemory(binary)).toThrow();
  });
});
