# peri-wasm

`peri-wasm` embeds the existing Peri ACP Host in an Emscripten module. Its
`PeriWasmAcp` export accepts and emits raw JSON-RPC frames through
`peri-acp::transport::WireBridge` and the in-memory transport. ACP request
handling, sessions, prompts, and model calls remain in `peri-acp` and
`peri-agent`.

## Build

Use the repository's mise environment to install Rust with the
`wasm32-unknown-emscripten` target, stock Emscripten 6.0.10, Python 3.12,
and `wasm-bindgen-cli 0.2.129`. Node or Bun is needed for the runtime probes.
`scripts/cargo-wasm.sh` applies the repository's Cloudflare epoll listener
and asynchronous DNS backports, plus Bun socket and Workers module URL
patches, to Emscripten 6.0.10 when needed. It then selects the pinned Mio and
Tokio forks, patches Hyper's Emscripten DNS resolver, and enables the
wasm-bindgen Tokio runtime flags. The Emscripten source patch uses
`Module.mainScriptUrlOrBlob` when Workers does not provide `import.meta.url`.
The linker uses `-sDYNAMIC_EXECUTION=0`; Peri formats dates in UTC on
Emscripten so the runtime does not need timezone script evaluation. Native
targets keep local date formatting. The generated JS and WASM are copied
without modification.

```bash
MISE_ENV=wasm mise install
MISE_ENV=wasm mise exec -- ./scripts/cargo-wasm.sh build --locked -p peri-wasm --target wasm32-unknown-emscripten
```

The output is `target/wasm32-unknown-emscripten/debug/peri-wasm.js` with
`peri_wasm.wasm` beside it. For a release build, add `--release`.

Run `node scripts/smoke-wasm-acp.mjs` after a debug build, or set
`PERI_WASM_PROFILE=release` after a release build. The smoke starts a local
sqld and model fixture, then verifies ACP prompt, Turso persistence and Host
restart. `scripts/smoke-wasm-remote-mcp.mjs` verifies an ACP-carried MCP tool.

## ACP port

```js
import Module from './peri-wasm.js';

const wasm = await Module();
const acp = await wasm.PeriWasmAcp.start(JSON.stringify({
  cwd: '/workspace',
  settings: { config: { /* provider and model configuration */ } },
  storage: { url: 'https://your-turso-endpoint', authToken: 'token' },
  machineId: '9c64df8f-f145-4f67-9ee8-ebacaa667f50',
}));

await acp.send(JSON.stringify({
  jsonrpc: '2.0', id: 1, method: 'initialize',
  params: { protocolVersion: 1, clientCapabilities: {} },
}));
console.log(JSON.parse(await acp.recv()));
await acp.close();
acp.free();
```

The startup settings use the same injected document as native ACP stdio.
`cwd` must be absolute and `machineId` must be a persistent UUID so
sessions can recover after a module restart. The JavaScript host supplies
workspace paths as identities; WASM creates matching empty directories in
its virtual filesystem so ACP resolves the same configuration scope.
Workspace file tools require a separately connected remote MCP server.
Builtin MCP instances are unavailable in this deployment.

`@peri-code/sdk` uses this export through `WasmAcpTransport` while retaining
the existing Agent and Session interfaces. Its HTTP demo is
`npm-packages/@peri-sdk/examples/demo/demo-wasm.ts`.

## Local Cloudflare Workers check

The probe lives in `@peri-code/sdk` and uses its Bun dependencies with
Wrangler's local `workerd`.
It starts a real local sqld, a simulated model HTTP endpoint, and the existing
ACP Host. It checks initialize, session creation, prompt completion, model
HTTP, session listing, and loading after the Host closes.

```bash
cd npm-packages/@peri-sdk
bun install --frozen-lockfile
bun run smoke:workers
```

The probe's `wrangler.toml` enables `nodejs_compat`, keeps the generated ES
module separate from Wrangler's bundler, and imports the precompiled WASM as
a module. It supplies `mainScriptUrlOrBlob` to the Emscripten module. This
check runs locally; deployment to Cloudflare's hosted Workers,
production Turso/model endpoints, resource limits, and the TypeScript SDK
entrypoint on Workers still need separate acceptance.
