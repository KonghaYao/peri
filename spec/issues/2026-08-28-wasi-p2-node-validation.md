# WASI Preview 2 policy Component 与 Node 验证

**状态**：Verified（2026-08-29）
**优先级**：高
**类型**：构建 / 跨运行时兼容
**创建日期**：2026-08-28
**来源**：用户请求 + `docs/design/wasi.md` 可行性探查

## 目标

为仓库提供一个可由 Rust 1.96.1 `wasm32-wasip2` 直接链接的 Component Model 产物，并用 Node.js 22、固定版本 Jco 和 Preview 2 shim 对其真实 WIT export 执行确定性断言。

本切片只承载 native Peri 已使用的纯 turn policy：消息 content 判空与 compact 阈值选择。native 与 Component 必须共享同一个 dependency-free policy kernel，不能用孤立 hello-world/probe 复制逻辑冒充完成。

## 边界

### In scope

- 内部、不发布的 `peri-turn-policy` portable kernel。
- 现有 `MessageContent::is_empty` 和 `determine_compact_action` 保持原路径/语义的委托接线。
- 内部、不发布的 `peri-wasi` WIT Component。
- 独立且锁版本的 `wasi-e2e` Node/Jco 验证包。
- Cargo 依赖闭包与最终 Component imports/exports 两层能力检查。

### Out of scope

- 完整 ACP Host、Agent loop、TUI 的 WASI 移植。
- HTTP、SQLite、进程、MCP、LSP、PTC、插件、任意文件系统或 secret capability。
- ACP wire、中间件顺序、CompactConfig 默认值或 native Smart 行为变更。
- `cargo-component` 或 runtime 私有 WASI 扩展。

## 固定 WIT 契约

package 为 `peri:turn-policy@0.1.0`，world 为 `turn-policy`，导出命名 interface `policy`：

- `classify-content(content: content-shape) -> content-classification`
- `select-compact(budget: f64, micro-threshold: f64) -> result<compact-action, policy-error>`

content classification 只表达 `MessageContent::is_empty`：空 text/blocks/raw 为 `empty`，whitespace text 与非空 blocks/raw 为 `non-empty`；它不声称包含 continuation-aware keepgoing 判定。

Component 仅导出 `skip`/`micro`，不把 legacy Smart 固化为新跨运行时契约。浮点验证顺序固定为 budget 非有限、budget 越界、threshold 非有限、threshold 越界；范围为闭区间 `[0,1]`。`wit-bindgen` 固定为 0.57.1，当前 `Cargo.lock` 实际解析到 `wit-parser 0.247.0`，WIT 使用 `f64` 语法。

## 构建与运行约束

- Rust toolchain 固定为 1.96.1，目标为 `wasm32-wasip2`，release artifact 为 `target/wasm32-wasip2/release/peri_wasi.wasm`。
- Component repository path closure 只能是 `peri-wasi -> peri-turn-policy`；禁止 Agent/ACP/model/middleware、Tokio、reqwest、SQLx、sysinfo 等 native 闭包。
- 最终 Component WIT/import 必须由 pinned 工具抽取并使用明确 allowlist；拒绝不需要的 HTTP、socket、filesystem、CLI environment/arguments 能力。
- Node 验证基线为 Node.js 22.20.0；依赖固定 `@bytecodealliance/jco@1.32.1`、`@bytecodealliance/preview2-shim@0.22.0`，npm 为 10.9.3。
- dependency acquisition 与 execution gate 分离：先运行 `npm --prefix wasi-e2e run acquire:cargo`，从已有 parent cache 生成并核对独立 closure、versioned vendor tree 与 credential-free Cargo home；再运行 `npm --prefix wasi-e2e test`，以 isolated Cargo home 执行 frozen/offline build → artifact capability check → Jco transpile → isolated Node child。
- acquisition 和 gate 只接受固定 owned、canonical、无 symlink 的路径；子进程环境使用 allowlist，执行阶段不得回退到 parent Cargo home 或读取其 credentials。
- 生成物只写入被忽略的 Cargo/`wasi-e2e/target` 目录，不提交 wasm、转译 JS 或 `node_modules`。

## 验收标准

- [x] `peri-turn-policy` 单测覆盖 content shape、空白差异、threshold 边界和 native Smart/非有限比较语义。
- [x] 现有 `peri-acp-types` content、Agent keepgoing/stage、compact 测试保持通过，公共 Rust 路径不变。
- [x] Rust 1.96.1 release build 生成合法 WASI Preview 2 Component，而不是 core Wasm/Preview 1。
- [x] Cargo tree 不包含 forbidden native closure，Component WIT/import 通过产物级 allowlist。
- [x] locked Node gate 精确调用 WIT 导出并覆盖 empty/non-empty、0/1/equality、NaN/±Infinity、双侧越界和多错误 precedence。
- [x] harness 对缺失/空/core-Wasm/损坏 artifact、工具非零退出、acquisition metadata 篡改与 vendor symlink 均确定失败。
- [x] `git diff --check`、目标 Rust tests/clippy/fmt 与适用 workspace 回归通过。
- [x] `docs/design/wasi.md` 明确这只是 deterministic policy Component probe，不宣称完整 Agent/ACP 已可移植。

## 已有验证证据

截至 2026-08-29，以下项目已在当前工作树实际通过：

- `npm --prefix wasi-e2e run acquire:cargo`：exit 0，`CARGO_VENDOR_REGISTRY_PACKAGES=34`；standalone 与 root registry closure 相同。acquisition 初始化 isolated Cargo home 时只写入固定 config；执行后允许 Cargo 创建无凭据的 cache marker，但 gate 会递归拒绝 credential/token 文件与 symlink。
- `npm --prefix wasi-e2e test`：exit 0，TAP `tests 8 / pass 8 / fail 0 / skipped 0`；完整链路中的 isolated child 输出 `WASI_P2_ASSERTIONS=28`。
- `npm --prefix wasi-e2e ci --ignore-scripts --no-audit --no-fund --offline`：exit 0，离线安装 114 packages；`package-lock.json` SHA-256 前后均为 `2ce2079557c31bcb1b7508fb2773128d0e9860d9355a1e55f614055459926248`。lockfile v3 含 219 个 package entries，218 个 registry entry 均有 `resolved` 与 `integrity`，非官方 registry URL 为 0。
- 产物：`target/wasm32-wasip2/release/peri_wasi.wasm`，header `00 61 73 6d 0d 00 01 00`；Jco 抽取的 world 仅 export `peri:turn-policy/policy@0.1.0`，imports 为空。
- `cargo test -p peri-turn-policy --lib`：8/8；`cargo test -p peri-acp-types --lib`：291/291；`cargo test -p peri-agent --lib`：696/696。
- `cargo +1.96.1 check -p peri-turn-policy --target wasm32-wasip2 --locked --offline` 与 `cargo +1.96.1 build -p peri-wasi --target wasm32-wasip2 --release --frozen`：exit 0；产物 16,897 bytes；有效 cfg 包含 `target_os="wasi"`、`target_env="p2"`、`target_feature="crt-static"`。
- `cargo clippy -p peri-turn-policy -p peri-wasi -p peri-acp-types -p peri-agent --all-targets --offline -- -D warnings` 与 `cargo clippy --workspace --all-targets --offline -- -D warnings`：exit 0。
- `cargo fmt --all -- --check`、`cargo test --workspace --doc --offline`、`git diff --check`、全部新增文件的 `git diff --no-index --check`、最终 status/ignored-artifact 审计：exit 0 或无问题输出。workspace doc tests 对仅 `cdylib` 的 `peri-wasi` 给出“不支持 doc tests”提示并跳过该 target，其余适用 doc tests 通过。

独立代码复审与最终 verifier 均给出通过结论。残余覆盖边界是：当前实测平台为 macOS aarch64、Node.js 22.20.0/npm 10.9.3；没有覆盖其他 OS/runtime，也不包含完整 Agent/ACP WASI port。acquisition 仍要求 parent Cargo cache 已有锁定 crate source，execution gate 本身保持 isolated/offline/credential-free。

## Stop conditions

- 解决编译/运行需要引入 native Host closure、改 ACP wire/中间件顺序、暴露凭据/preopen/network 或安装 `cargo-component`。
- `wit-bindgen`/Jco 无法无损投影固定 WIT，或最终 Component 出现非 allowlist capability。
- native keepgoing、whitespace、Compact Smart 行为变化。
- owned 文件出现非本任务的重叠改动。

## 相关入口

- `docs/design/wasi.md`
- `peri-acp-types/src/messages/content.rs`
- `peri-agent/src/session/exec/executor.rs`
- `peri-agent/src/agent/compact_v2/mod.rs`
- `peri-turn-policy/src/lib.rs`
- `peri-wasi/wit/world.wit`
- `wasi-e2e/acquire-cargo.mjs`
- `wasi-e2e/harness.mjs`
- `.cargo/config.toml`
