import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { CF_WASM_MAX_MEMORY_BYTES, CF_WASM_STACK_BYTES } from "../shared/runtime-limits";

export const WASM_TARGET = "wasm32-unknown-emscripten";
export const DEFAULT_WASM_PROFILE = "cf-wasm-size";
export const WASM_PROFILES = [DEFAULT_WASM_PROFILE, "cf-wasm-speed"] as const;
export type WasmProfile = typeof WASM_PROFILES[number];
export const PINNED_TOOLCHAIN = {
  rust: "1.99.0",
  rustCommit: "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4",
  emscripten: "6.0.10",
  wasmBindgen: "0.2.129",
} as const;

export function wasmProfile(args: string[]): WasmProfile {
  if (!args.length) return DEFAULT_WASM_PROFILE;
  if (args.length !== 2 || args[0] !== "--profile" || !WASM_PROFILES.includes(args[1] as WasmProfile))
    throw new Error(`Usage: --profile ${WASM_PROFILES.join("| ")}`);
  return args[1] as WasmProfile;
}

export async function buildSettings(repoRoot: string, profile: WasmProfile) {
  const cargo = Bun.TOML.parse(await readFile(resolve(repoRoot, "Cargo.toml"), "utf8")) as {
    profile: Record<string, Record<string, unknown>>;
  };
  const settings = cargo.profile[profile];
  const expected = { inherits: "release", "opt-level": profile === "cf-wasm-size" ? "z" : 3,
    lto: "thin", "codegen-units": 16 };
  if (!settings || Object.keys(settings).length !== Object.keys(expected).length
    || Object.entries(expected).some(([key, value]) => settings[key] !== value))
    throw new Error(`Unexpected ${profile} settings; review the explicit size/speed profile contract`);
  return {
    target: WASM_TARGET,
    package: "peri-wasm",
    features: ["cloudflare"],
    defaultFeatures: true,
    locked: true,
    profile,
    cargoProfile: settings,
    inheritedProfile: cargo.profile.release,
    maxMemoryBytes: CF_WASM_MAX_MEMORY_BYTES,
    stackBytes: CF_WASM_STACK_BYTES,
    growthStepBytes: 1024 * 1024,
    pinnedToolchain: PINNED_TOOLCHAIN,
  };
}

export function cleanBuildEnvironment(base: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const result = { ...base };
  for (const name of Object.keys(result)) {
    if (/^(?:RUSTFLAGS|RUSTDOCFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_TARGET_DIR|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|EMCC_CFLAGS|EMMAKEN_CFLAGS|CFLAGS|CXXFLAGS|CPPFLAGS|LDFLAGS)$/.test(name)
      || /^CARGO_(?:PROFILE_|BUILD_|TARGET_|CONFIG|FEATURE_)/.test(name) || /^PERI_WASM_/.test(name))
      delete result[name];
  }
  return result;
}

export function wasmMemory(bytes: Uint8Array) {
  let cursor = 8;
  const take = () => {
    if (cursor >= bytes.length) throw new Error("Truncated WASM binary");
    return bytes[cursor++];
  };
  const unsigned = () => {
    let value = 0;
    for (let index = 0; index < 5; index++) {
      const next = take();
      value += (next & 127) * 2 ** (7 * index);
      if (!(next & 128)) return value;
    }
    throw new Error("Invalid WASM integer");
  };
  if (!Buffer.from(bytes.subarray(0, 8)).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0])))
    throw new Error("Invalid WASM header");
  while (cursor < bytes.length) {
    const section = take();
    const length = unsigned();
    const end = cursor + length;
    if (end > bytes.length) throw new Error("Truncated WASM section");
    if (section === 5) {
      if (unsigned() !== 1 || unsigned() !== 1)
        throw new Error("CF WASM must define one non-shared memory with an explicit maximum");
      const initialBytes = unsigned() * 65536;
      const maximumBytes = unsigned() * 65536;
      if (cursor !== end || initialBytes > maximumBytes || initialBytes < CF_WASM_STACK_BYTES
        || maximumBytes !== CF_WASM_MAX_MEMORY_BYTES)
        throw new Error("CF WASM linear-memory limits do not match the runtime budget");
      return { initialBytes, maximumBytes };
    }
    cursor = end;
  }
  throw new Error("CF WASM has no declared linear memory");
}
