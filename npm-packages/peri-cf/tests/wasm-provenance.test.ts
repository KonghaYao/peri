import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { prepareArtifacts } from "../scripts/prepare-wasm";
import { assertSameInputs, artifactChecksums, buildInputs, canonical, MANIFEST_FILE,
  replaceDirectory, sourceChecksums, verifyArtifacts, withDirectoryLock, writeProvenance } from "../scripts/wasm-provenance";
import { DEFAULT_WASM_PROFILE } from "../scripts/wasm-profile";

const temporary: string[] = [];
const versions = { rustc: "fixture-rustc", cargo: "fixture-cargo", emcc: "fixture-emcc", wasmBindgen: "fixture-bindgen" };
const fixtureGlue = [
  'getDns(){return nodeSockHelpers.dnsModule??=(process.getBuiltinModule||require)("dns")}',
  'getNet(){return nodeSockHelpers.netModule??=(process.getBuiltinModule||require)("net")}',
  'if(!sock.bound&&typeof Bun==="undefined")',
  'setImmediateWrapped.mapping[id]=setImmediate(',
  'clearImmediate(handle);setImmediateWrapped.mapping[id]=undefined',
  'safeSetTimeout.mapping[id]=setTimeout(',
  'clearTimeout(handle);safeSetTimeout.mapping[id]=undefined',
  'globalThis.setTimeout(arg0,arg1>>>0)',
  'globalThis.clearTimeout(arg0)',
].join("\n");
const fixtureWasm = Uint8Array.from([0, 97, 115, 109, 1, 0, 0, 0, 5, 6, 1, 1, 128, 1, 128, 8]);

afterEach(async () => { for (const path of temporary.splice(0)) await rm(path, { recursive: true, force: true }); });

async function fixture() {
  const root = await mkdtemp(resolve(tmpdir(), "peri-cf-provenance-"));
  temporary.push(root);
  const file = async (name: string, contents: string | Uint8Array) => {
    await mkdir(dirname(resolve(root, name)), { recursive: true });
    await writeFile(resolve(root, name), contents);
  };
  await file("Cargo.toml", '[workspace]\nmembers=["peri-wasm"]\n[profile.release]\nopt-level="z"\nlto="thin"\ncodegen-units=16\n[profile.cf-wasm-size]\ninherits="release"\nopt-level="z"\nlto="thin"\ncodegen-units=16\n[profile.cf-wasm-speed]\ninherits="release"\nopt-level=3\nlto="thin"\ncodegen-units=16\n');
  for (const name of ["Cargo.lock", "peri-wasm/Cargo.toml", "peri-wasm/src/lib.rs", "peri-wasm/build.rs",
    ".cargo/config.toml", "patches/tokio.patch", "scripts/cargo-wasm.sh", "scripts/cargo-rmcp-patched.sh",
    "scripts/prepare-emscripten.sh", "npm-packages/peri-cf/shared/runtime-limits.ts",
    "npm-packages/peri-cf/scripts/build-wasm.ts", "npm-packages/peri-cf/scripts/wasm-tools.ts"])
    await file(name, name);
  await file("scripts/cargo-wasm.sh", await readFile(resolve(import.meta.dir, "../../../scripts/cargo-wasm.sh"), "utf8"));
  await file("peri-wasm/Cargo.toml", '[package]\nname="peri-wasm"\nversion="0.1.0"');
  await file("artifacts/peri-wasm.js", fixtureGlue);
  await file("artifacts/peri_wasm.wasm", fixtureWasm);
  const source = resolve(root, "artifacts");
  const destination = resolve(root, "generated");
  const inputs = await buildInputs(root, DEFAULT_WASM_PROFILE, versions);
  return { root, file, source, destination, inputs };
}

describe("CF WASM build provenance", () => {
  test("records dirty working-tree inputs and verifies complete JS/WASM output", async () => {
    const setup = await fixture();
    await writeProvenance(setup.source, setup.inputs, 12);
    const manifest = await verifyArtifacts(setup.source, setup.inputs);
    expect(manifest.buildWallMs).toBe(12);
    expect(manifest.inputs.settings.rustflags).toContain("-sMAXIMUM_MEMORY=67108864");
    expect(manifest.inputs.settings.rustflags).toContain("-sSTACK_SIZE=8388608");
    expect(manifest.inputs.settings.rustflags).toContain("-sMEMORY_GROWTH_LINEAR_STEP=1048576");
    expect(manifest.inputs.files["peri-wasm/src/lib.rs"]).toHaveLength(64);
    expect(Object.keys(manifest.artifacts).sort()).toEqual(["peri-wasm.js", "peri_wasm.wasm"]);
  });

  for (const name of ["peri-wasm/src/lib.rs", "peri-wasm/src/new-untracked.rs", "Cargo.lock",
    "scripts/cargo-wasm.sh", "patches/tokio.patch", ".cargo/config.toml",
    "npm-packages/peri-cf/shared/runtime-limits.ts", "npm-packages/peri-cf/scripts/wasm-tools.ts"])
    test(`rejects changed or newly added input ${name}`, async () => {
      const setup = await fixture();
      await writeProvenance(setup.source, setup.inputs, 10);
      await setup.file(name, name === "scripts/cargo-wasm.sh"
        ? `${await readFile(resolve(setup.root, name), "utf8")}\n` : "changed working-tree input");
      const updated = await buildInputs(setup.root, DEFAULT_WASM_PROFILE, versions);
      await expect(verifyArtifacts(setup.source, updated)).rejects.toThrow("build inputs changed or are stale");
    });

  test("rejects deleted sources and pinned toolchain/flags/profile mismatch", async () => {
    const setup = await fixture();
    await writeProvenance(setup.source, setup.inputs, 10);
    for (const inputs of [
      { ...setup.inputs, toolchain: { ...versions, rustc: "different rustc" } },
      { ...setup.inputs, settings: { ...setup.inputs.settings, maxMemoryBytes: 2147483648 } },
      await buildInputs(setup.root, "cf-wasm-speed", versions),
    ]) await expect(verifyArtifacts(setup.source, inputs)).rejects.toThrow("stale");
    await rm(resolve(setup.root, "peri-wasm/src/lib.rs"));
    await expect(verifyArtifacts(setup.source, await buildInputs(setup.root, DEFAULT_WASM_PROFILE, versions)))
      .rejects.toThrow("stale");
  });

  for (const artifact of ["peri_wasm.wasm", "peri-wasm.js"])
    test(`rejects tampered ${artifact}`, async () => {
      const setup = await fixture();
      await writeProvenance(setup.source, setup.inputs, 10);
      await setup.file(`artifacts/${artifact}`, "tampered");
      await expect(verifyArtifacts(setup.source, setup.inputs)).rejects.toThrow("Mixed or tampered");
    });

  test("rejects missing/partial/malformed manifests instead of stamping existing artifacts", async () => {
    const setup = await fixture();
    await expect(verifyArtifacts(setup.source, setup.inputs)).rejects.toThrow("build-produced WASM provenance");
    await setup.file(`artifacts/${MANIFEST_FILE}`, "{}");
    await expect(verifyArtifacts(setup.source, setup.inputs)).rejects.toThrow("build-produced WASM provenance");
    await writeProvenance(setup.source, setup.inputs, 10);
    await rm(resolve(setup.source, "peri_wasm.wasm"));
    await expect(verifyArtifacts(setup.source, setup.inputs)).rejects.toThrow("Missing complete CF WASM artifacts");
  });

  test("includes snippets and rejects mixed or extra output files", async () => {
    const setup = await fixture();
    await setup.file("artifacts/snippets/host/index.js", "export default 1");
    await writeProvenance(setup.source, setup.inputs, 10);
    expect((await verifyArtifacts(setup.source, setup.inputs)).artifacts["snippets/host/index.js"]).toHaveLength(64);
    await setup.file("artifacts/snippets/host/index.js", "export default 2");
    await expect(verifyArtifacts(setup.source, setup.inputs)).rejects.toThrow("tampered");
    await setup.file("artifacts/stale.wasm", "extra");
    await expect(artifactChecksums(setup.source)).rejects.toThrow("Unexpected WASM artifact");
  });

  test("ignores secrets, environment values and generated/cache files", async () => {
    const setup = await fixture();
    for (const name of ["peri-wasm/.env", "peri-wasm/.env.local", "peri-wasm/.dev.vars",
      "peri-wasm/credentials.json", "peri-wasm/target/changed.rs", "peri-wasm/node_modules/new.rs"])
      await setup.file(name, "DO_NOT_CAPTURE_SECRET_VALUE");
    process.env.PERI_PROVENANCE_TEST_SECRET = "DO_NOT_CAPTURE_SECRET_VALUE";
    try {
      const current = await buildInputs(setup.root, DEFAULT_WASM_PROFILE, versions);
      expect(canonical(current)).not.toContain("DO_NOT_CAPTURE_SECRET_VALUE");
      expect(current).toEqual(setup.inputs);
    } finally { delete process.env.PERI_PROVENANCE_TEST_SECRET; }
  });

  test("includes toolchain pin files, build-script assets and new nested sources", async () => {
    const setup = await fixture();
    await setup.file("rust-toolchain.toml", '[toolchain]\nchannel="1.99.0"');
    await setup.file("peri-wasm/src/assets/schema.json", "{}");
    const files = await sourceChecksums(setup.root);
    expect(files["rust-toolchain.toml"]).toHaveLength(64);
    expect(files["peri-wasm/src/assets/schema.json"]).toHaveLength(64);
    expect(files["peri-wasm/build.rs"]).toHaveLength(64);
  });

  test("discovers transitive local path crates outside workspace members", async () => {
    const setup = await fixture();
    await setup.file("peri-wasm/Cargo.toml", '[package]\nname="peri-wasm"\nversion="0.1.0"\n[dependencies]\nhelper={path="../outside-helper"}');
    await setup.file("outside-helper/Cargo.toml", '[package]\nname="helper"\nversion="0.1.0"');
    await setup.file("outside-helper/src/lib.rs", "pub fn helper() {}");
    const files = await sourceChecksums(setup.root);
    expect(files["outside-helper/Cargo.toml"]).toHaveLength(64);
    expect(files["outside-helper/src/lib.rs"]).toHaveLength(64);
    expect(() => assertSameInputs(setup.inputs, { ...setup.inputs, files })).toThrow("stale");
  });

  test("hashes only the peri-wasm local dependency closure, never unrelated workspace members", async () => {
    const setup = await fixture();
    await setup.file("Cargo.toml", '[workspace]\nmembers=["peri-wasm","peri-tui"]\n');
    await setup.file("peri-tui/Cargo.toml", '[package]\nname="peri-tui"\nversion="0.1.0"');
    await setup.file("peri-tui/src/main.rs", "fn main() {}");
    const before = await sourceChecksums(setup.root);
    expect(Object.keys(before).some((name) => name.startsWith("peri-tui/"))).toBe(false);
    expect(before["peri-wasm/src/lib.rs"]).toHaveLength(64);
    await setup.file("peri-tui/src/main.rs", "fn main() { println!(); }");
    expect(await sourceChecksums(setup.root)).toEqual(before);
    await setup.file("peri-wasm/src/lib.rs", "changed in the CF WASM closure");
    const after = await sourceChecksums(setup.root);
    expect(after["peri-wasm/src/lib.rs"]).not.toBe(before["peri-wasm/src/lib.rs"]);
    expect(() => assertSameInputs({ ...setup.inputs, files: before }, { ...setup.inputs, files: after })).toThrow("stale");
  });

  test("follows workspace-level path dependencies declared as workspace = true", async () => {
    const setup = await fixture();
    await setup.file("Cargo.toml", '[workspace]\nmembers=["peri-wasm"]\n[workspace.dependencies]\nhelper={path="crates/helper"}\n');
    await setup.file("peri-wasm/Cargo.toml", '[package]\nname="peri-wasm"\nversion="0.1.0"\n[dependencies]\nhelper.workspace=true\n');
    await setup.file("crates/helper/Cargo.toml", '[package]\nname="helper"\nversion="0.1.0"');
    await setup.file("crates/helper/src/lib.rs", "pub fn helper() {}");
    const before = await sourceChecksums(setup.root);
    expect(before["crates/helper/src/lib.rs"]).toHaveLength(64);
    await setup.file("crates/helper/src/lib.rs", "pub fn helper() { }");
    const after = await sourceChecksums(setup.root);
    expect(after["crates/helper/src/lib.rs"]).not.toBe(before["crates/helper/src/lib.rs"]);
    expect(() => assertSameInputs({ ...setup.inputs, files: before }, { ...setup.inputs, files: after })).toThrow("stale");
  });

  test("fails loudly when the workspace does not declare peri-wasm", async () => {
    const setup = await fixture();
    await setup.file("Cargo.toml", '[workspace]\nmembers=["peri-tui"]\n');
    await setup.file("peri-tui/Cargo.toml", '[package]\nname="peri-tui"\nversion="0.1.0"');
    await expect(sourceChecksums(setup.root)).rejects.toThrow("has no peri-wasm member");
  });

  test("hashes the prebuilt bundle a closure crate embeds outside the Cargo graph", async () => {
    const setup = await fixture();
    const embedded = "npm-packages/@peri-workflow/dist/peri-workflow.js";
    await setup.file(embedded, "// embedded workflow bundle");
    const files = await sourceChecksums(setup.root);
    expect(files[embedded]).toHaveLength(64);
    await setup.file(embedded, "// changed embedded workflow bundle");
    expect((await sourceChecksums(setup.root))[embedded]).not.toBe(files[embedded]);
  });

  test("rejects source symlinks and mutations during build", async () => {
    const setup = await fixture();
    await setup.file("peri-wasm/src/lib.rs", "changed");
    expect(() => assertSameInputs(setup.inputs, { ...setup.inputs, settings: {} })).toThrow("stale");
    expect(() => assertSameInputs(setup.inputs, setup.inputs)).not.toThrow();
    expect(() => assertSameInputs(setup.inputs, { ...setup.inputs, files: {} })).toThrow("stale");
    await symlink(resolve(setup.root, "Cargo.lock"), resolve(setup.root, "peri-wasm/src/linked.rs"));
    await expect(sourceChecksums(setup.root)).rejects.toThrow("Symlinked build input");
  });
});

describe("verified WASM preparation publication", () => {
  test("publishes only verified artifacts and guarded app-only glue", async () => {
    const setup = await fixture();
    await setup.file("generated/old.txt", "previous");
    await writeProvenance(setup.source, setup.inputs, 10);
    await prepareArtifacts(setup.source, setup.destination, setup.inputs);
    expect((await readdir(setup.destination)).sort()).toEqual(["peri-wasm.js", "peri_wasm.wasm"]);
    expect(await readFile(resolve(setup.destination, "peri-wasm.js"), "utf8")).toContain('Module["periNet"]');
    expect(await readFile(resolve(setup.source, "peri-wasm.js"), "utf8")).toBe(fixtureGlue);
    expect(await readFile(resolve(setup.destination, "peri_wasm.wasm"))).toEqual(Buffer.from(fixtureWasm));
  });

  test("keeps previous destination intact after stale, incompatible or concurrently changed inputs", async () => {
    const setup = await fixture();
    await setup.file("generated/old.txt", "previous");
    await expect(prepareArtifacts(setup.source, setup.destination, setup.inputs)).rejects.toThrow("provenance");
    await writeProvenance(setup.source, setup.inputs, 10);
    await expect(prepareArtifacts(setup.source, setup.destination, setup.inputs, async () => {
      throw new Error("source changed during preparation");
    })).rejects.toThrow("source changed");
    await setup.file("artifacts/peri-wasm.js", "unsupported glue");
    await writeProvenance(setup.source, setup.inputs, 10);
    await expect(prepareArtifacts(setup.source, setup.destination, setup.inputs)).rejects.toThrow("Unsupported SDK WASM glue");
    expect(await readFile(resolve(setup.destination, "old.txt"), "utf8")).toBe("previous");
    expect((await readdir(setup.root)).some((name) => name.startsWith("generated.tmp-"))).toBe(false);
  });

  test("rolls back a failed directory replacement", async () => {
    const setup = await fixture();
    await setup.file("generated/old.txt", "previous");
    await expect(replaceDirectory(resolve(setup.root, "missing-staging"), setup.destination)).rejects.toThrow("publish");
    expect(await readFile(resolve(setup.destination, "old.txt"), "utf8")).toBe("previous");
  });

  test("rejects concurrent publishers and always releases its lock", async () => {
    const setup = await fixture();
    const lock = resolve(setup.root, "build.lock");
    await withDirectoryLock(lock, async () => {
      await expect(withDirectoryLock(lock, async () => {})).rejects.toThrow("locked");
    });
    await expect(withDirectoryLock(lock, async () => { throw new Error("failed"); })).rejects.toThrow("failed");
    expect(await withDirectoryLock(lock, async () => "released")).toBe("released");
  });
});
