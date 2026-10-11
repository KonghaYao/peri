# Emscripten timer smoke

Run from the repository root with the Emscripten SDK, Node, the
`wasm32-unknown-emscripten` Rust target, and `wasm-bindgen-cli 0.2.129`:

```bash
./peri-time/tests/emscripten-smoke/run.sh
```

This fixture is an isolated Cargo workspace. It does not run during normal
`cargo test --workspace`. The Node host checks timer completion and expiry,
interval ticks, future-first zero-budget behavior, positive sub-millisecond
rounding, the first chunk of a long wait, and `clearTimeout` after cancellation.

`bun peri-time/tests/emscripten-smoke/run.mjs` runs the same runtime assertions
after the build. For local `workerd`, set `WRANGLER` to a Wrangler CLI path and
run `./peri-time/tests/emscripten-smoke/run-workerd.sh`. This checks the
completion, expiry, and interval path inside the Worker runtime.
