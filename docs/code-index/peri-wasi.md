# peri-wasi 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码、产物 WIT 与 Node gate 为准。更新：2026-08-29
> 依据：`docs/design/wasi.md`、`spec/issues/2026-08-28-wasi-p2-node-validation.md`、源码（本 crate 无 CLAUDE.md）

## 架构速览

- 定位：内部、`publish = false` 的 WASI Preview 2 `cdylib` adapter，把 `peri-turn-policy` 的确定性策略暴露为 Component Model export。
- 数据流：WIT caller → wit-bindgen guest bindings → `Component` adapter → `peri-turn-policy` → WIT result/variant；业务 exports 之外没有 imports。
- 构建：Rust 1.96.1 `wasm32-wasip2` 由 `wasm-component-ld` 直接产出 `target/wasm32-wasip2/release/peri_wasi.wasm`，不使用 `cargo-component`。
- 稳定边界：WIT package `peri:turn-policy@0.1.0`、world `turn-policy`、sole business export `policy`；它不是 Agent loop、ACP protocol 或 Headless ACP Host port。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 Component WIT 契约 | `wit/world.wit` | package `peri:turn-policy@0.1.0`；interface `policy`；world `turn-policy` | `classify-content` 导出 content 判空；`select-compact` 导出 `result<compact-action, policy-error>`。当前 world 只 export `policy` 且没有 imports；改名或增 capability 必须同步 adapter、Jco gate 与文档 |
| 改 WIT/Rust adapter | `src/lib.rs` | `wit_bindgen::generate!`；`impl Guest for Component`；`export!(Component)` | 把 WIT `content-shape` 映射到共享 shape；把共享 Skip/Micro 映射回 WIT。Smart 在边界恒禁用，匹配分支仅作不可达防护 |
| 改 compact 输入错误契约 | `src/lib.rs::select_compact` + `wit/world.wit::policy-error` | `BudgetNotFinite`、`BudgetOutOfRange`、`MicroThresholdNotFinite`、`MicroThresholdOutOfRange` | 验证顺序固定：budget 非有限 → budget 不在 `[0,1]` → threshold 非有限 → threshold 不在 `[0,1]`；通过后才调用共享 selector |
| 改构建闭包/绑定版本 | `Cargo.toml` + workspace 根 `Cargo.toml` / `Cargo.lock` | `crate-type = ["cdylib"]`；path dependency `peri-turn-policy`；`wit-bindgen = 0.57.1` | local path closure 只能是 `peri-wasi → peri-turn-policy`；当前 `Cargo.lock` 的 `wit-parser` 为 0.247.0。禁止引入 Agent/ACP/model/middleware 或 native capability closure |
| 改 Node/Jco 验证 | `wasi-e2e/harness.mjs`、`wasi-e2e/exercise.mjs`、`wasi-e2e/wasi-p2.test.mjs` | `buildComponent`；`assertComponentInterface`；isolated child 的 28 项断言 | gate 验证 toolchain/Cargo tree、Component header、WIT sole export/empty imports、Jco 转译 shape 与实际执行；负例拒绝 missing/empty/core/corrupt artifact、Jco failure、metadata 篡改和 vendor symlink |
| 准备隔离 Cargo 供应链 | `wasi-e2e/acquire-cargo.mjs` + `wasi-e2e/package.json` | `npm --prefix wasi-e2e run acquire:cargo` | 比较 standalone 与 root registry closure，从已有 parent cache vendor 当前 34 个 registry packages，在 `wasi-e2e/target` 生成 credential-free Cargo home；执行 gate 不回退到 parent Cargo home |
| 运行跨运行时门禁 | `wasi-e2e/package.json` | `npm --prefix wasi-e2e test` | 验证基线 Node 22.20.0/npm 10.9.3、Jco 1.32.1、Preview 2 shim 0.22.0；当前 TAP 8/8，isolated child 28 assertions；依赖 acquisition 必须先单独运行 |

## 产物契约

| 项目 | 事实 |
| --- | --- |
| release artifact | `target/wasm32-wasip2/release/peri_wasi.wasm` |
| Component header | `00 61 73 6d 0d 00 01 00`；不得退化为 core Wasm `01 00 00 00` |
| WIT export | `peri:turn-policy/policy@0.1.0` |
| imports | 空 |
| generated Node output | `wasi-e2e/target/wasi-p2-node/`，ignored |
| acquisition/vendor/Cargo home | `wasi-e2e/target/` 下固定 owned 目录，ignored |

## 跨模块契约

- Shared policy：adapter 必须调用 `peri-turn-policy`，不得复制 native policy；native wiring 见 `docs/code-index/peri-turn-policy.md`。
- Content：WIT `classify-content` 只投影 `MessageContent::is_empty`，不等于 `peri-agent` continuation-aware keepgoing（ARC-KEEPGOING-001）。
- Compact：WIT 只承诺 Skip/Micro 和显式输入错误；native Smart 与未校验浮点比较仍留在共享 kernel/native caller，不扩展为 WIT ABI。
- Host boundary：完整 ACP Host 仍依赖 HTTP、SQLite、filesystem、process、MCP/LSP/PTC 等 native 能力；本 crate 的成功不能作为 Agent/ACP 已可移植的证据，完整边界见 `docs/design/wasi.md`。
