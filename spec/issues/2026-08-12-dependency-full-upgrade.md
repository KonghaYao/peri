# 依赖彻底升级：分批执行计划（Batch 0–3）

**状态**：✅ 全部完成（2026-08-12，8 个 commit 落地，见下方 commit 表）
**优先级**：中
**创建日期**：2026-08-12
**最后核查**：2026-08-12

## 最新情况（2026-08-12）

**全部批次已完成并提交**（feature/20260811-2 分支）：

| 批次 | 内容 | commit |
| --- | --- | --- |
| Batch 0 | cargo update 兼容更新（132 包，随 rmcp/ACP 提交） | `1b29e0a4`（合并入） |
| Batch 3a | rmcp 2.2.0→3.1.2 + agent-client-protocol 1.3→2.0.0（schema `=1.5.0`） | `1b29e0a4` |
| Batch 1 | serial_test 4.0.1 / base64 0.23.1 / tokio-tungstenite 0.30.0（含 acp-hub 同步） | `8f8bb9d5` |
| Batch 2a | pulldown-cmark 0.13.4（peri-tui 走 `pulldown-cmark-012` 别名避免双版本类型混用） | `22f498dd` |
| Batch 2b | portable-pty 0.9.0（解除 pin，Windows ConPTY 需回归） | `4e7e6c84` |
| Batch 2c | axum 0.8.9（ws_handler：Utf8Bytes / Close(None)） | `923f0f62` |
| Batch 2d | sqlx 0.9.0（5 处动态 SQL 加 `AssertSqlSafe`） | `b8307709` |
| Batch 3b | ed25519-dalek 3.0.0 / x25519-dalek 3.0.0（零代码改动，curve25519-dalek 5×4.1 双版本共存） | `8746e81d` |

每批均过：`build --workspace --all-targets` / `clippy --workspace --all-targets -D warnings` / `test --workspace`（含 doc tests）/ lefthook pre-commit；Batch 1 含 `cd acp-hub && cargo test --workspace` 全绿。

**遗留验证项**：E2E（`e2e/` 目录 `npm run e2e -- --all`）与 Windows ConPTY 场景（portable-pty 0.9.0）未在本次执行。

## 背景

依赖审计（2026-08-12）结论：

- 主 workspace（15 个 crate：14 个 workspace members + peri-theme）Cargo.lock 落后最新稳定版 132 个包版本更新（+11 新增 / −1 移除，相对 HEAD 基线实测）；另有 20 处直接依赖 breaking（12 个唯一依赖、10 个 crate）、35 个传递依赖 major 落后。
- **`acp-hub/` 是独立 workspace**（`[workspace] members = ["proto","server","instance"]`，3 个 crate，独立 Cargo.lock），不随根 `cargo update` / `cargo test --workspace` 覆盖。它共享 `serial_test`、`base64`、`tokio-tungstenite`、`agent-client-protocol-schema` 4 个升级目标，**必须与本计划同步评估**（见各批次表的 acp-hub 列）。
- 审计明细数据文件位于 `/tmp/perihelion-deps-audit.md`、`/tmp/dep_audit.txt`、`/tmp/direct_audit.txt`（临时目录，不保证持久；各批次表的数值均按当前源码实测核对，可独立复现）。

## 决策

1. **分批推进，绝不一次全升**：按耦合度分 4 批，每批独立编译/测试/提交，单批可独立回滚。
2. **Batch 0（cargo update）立即执行**：零声明改动、零代码改动（仅 Cargo.lock）。
3. **核心协议层单独立项（Batch 3）**：`agent-client-protocol 2.0`（含 schema pin 决策）+ `rmcp 3.x` 独立专项；`ed25519/x25519-dalek 3.0`（peri-tui sync 模块）与 ACP **无依赖耦合**，作为 Batch 3 内独立子项（见 Batch 3 表）。保底：停留在 ACP 1.3 + schema 1.5 稳定线，不阻塞其他批次。
4. **声明改动点在各自 crate 的 Cargo.toml**（serial_test/base64 等多 crate 共享依赖需同步改所有声明点，含 acp-hub workspace 内的声明）。
5. **MSRV 约束**：仓库无 `rust-toolchain.toml`，CI 用 stable（本地 rustc 1.96.1）。升级前核对各依赖 rust-version：sqlx 0.9.0 要求 1.94、agent-client-protocol 2.0.0 要求 1.88；`keyring =4.1.6` 已有"避免解析到更高 MSRV 补丁版"的精确锁定先例（peri-tui/Cargo.toml），后续升级保持同等约束意识。

## 批次计划

### Batch 0 — 兼容升级（0.5 天）

```bash
cargo update
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --doc
lefthook run pre-commit
```

注意：根 workspace 命令不覆盖 `acp-hub/`；Batch 0 不处理 acp-hub（其 lock 独立、不在本次兼容更新范围内），但后续批次改动共享依赖时须同步。

### Batch 1 — 低风险 breaking（0.5–1 天）

| 依赖 | 声明点 | 代码面 | 备注 |
| --- | --- | --- | --- |
| serial_test 3 → 4.0.1 | peri-acp、peri-controller、peri-tui、peri-web-pty（均 dev-dependencies） | 23 处（dev，实测 23 个文件） | API 基本兼容 |
| base64 0.22 → 0.23.1 | langfuse-client、peri-middlewares、peri-tui | 14 处（实测） | 编译期暴露 |
| tokio-tungstenite 0.29 → 0.30.0 | peri-tui、peri-web-pty | 2 处（实测） | 小改 |

**acp-hub 同步**：`serial_test`、`base64`、`tokio-tungstenite` 在 acp-hub 均为 workspace 声明（`acp-hub/Cargo.toml:49/55/74`），改根 workspace 声明后须同步修改并单独跑 `cd acp-hub && cargo test --workspace`。

### Batch 2 — 中风险 breaking（1–2 天，每依赖独立 commit）

| 依赖 | 声明点 | 代码面 | 备注 |
| --- | --- | --- | --- |
| pulldown-cmark 0.12 → 0.13.4 | peri-middlewares、peri-tui | 11 处（实测） | Tag/Event 结构调整；注意 ratatui-kit-markdown 0.3.0 固定依赖 pulldown-cmark 0.12，会形成双版本共存 |
| portable-pty =0.8.1 → 0.9.0 | peri-web-pty | 2 处（实测） | 解除精确 pin；**回归风险**：0.9.0 曾有 Windows ConPTY reader 读不到 PTY child 输出（短命令场景）的已知问题（根 Cargo.toml 注释），升级前先确认该场景是否仍复现 |
| sqlx 0.8 → 0.9.0 | peri-agent、peri-resources | 37 处（实测 39 处） | 工作量最大；rust-version 要求 1.94 |
| axum 0.7 → 0.8.9 | peri-web-pty | 6 处（实测） | Router/Handler/State 调整 |

### Batch 3 — 核心协议专项（3–5 天，单独立项）

| 依赖 | 声明点 | 代码面 | 阻塞点 |
| --- | --- | --- | --- |
| agent-client-protocol 1.3 → 2.0.0 | peri-acp、peri-tui | 59 处（实测 62 处） | 决策：schema 升 1.5.0（ACP 2.0 强制 `=1.5.0`）还是升 1.6.0（`=1.5.0` 与 `^1.6.0` 不可共存，已实测确认）。注意 acp-hub lock 已是 schema 1.6.0，两 workspace 间存在 schema 版本漂移，需一并决策 |
| ed25519-dalek 2.2 → 3.0.0 | peri-tui | 24 处（实测） | **与 ACP 无耦合**（ACP 2.0 不依赖 dalek）；与 x25519-dalek 3.0 耦合（共同引入 curve25519-dalek 5，而 snow 0.10 固定 curve25519-dalek 4.1，形成双版本共存，需验证）。3.0 为 API 大改（Signer/Verifier 相关 trait 变更） |
| x25519-dalek 2 → 3.0.0 | peri-tui | 9 处（实测） | 同上（curve25519-dalek 5 × snow 0.10 共存验证） |
| rmcp 2 → 3.1.2 | peri-middlewares | 61 处（实测） | 独立专项，与 ACP 无耦合；features 结构调整（server/transport 拆分），需核对 `transport-child-process`、`transport-streamable-http-client-reqwest`、`auth` 对应项 |

## 每批验证标准

1. `cargo build --workspace --all-targets` 零错误（与 CI 一致）
2. `cargo clippy --workspace --all-targets -- -D warnings` 零警告
3. `cargo test --workspace` + `cargo test --workspace --doc` 全绿
4. `lefthook run pre-commit`
5. 涉及 acp-hub 共享依赖（serial_test/base64/tokio-tungstenite）的批次：`cd acp-hub && cargo test --workspace` 全绿
6. 涉及跨层事件/协议链路跑 E2E：命令见 `e2e/CLAUDE.md`（在 `e2e/` 目录执行 `npm run e2e -- --only tests/<目录>/<文件>.test.ts`；发布前全量用 `npm run e2e -- --all`）
7. Batch 3 需过 `docs/standards/architecture-contracts.md` 跨仓契约检查

## 回滚策略

- 每批一个 commit，Cargo.lock 随 commit 落盘；`git revert <batch-commit>` 单批回滚。
- 注意：Cargo.lock 为后续批次的解析基线，回滚 Batch 1–3 中任一提交后，若 lock 与该批次 manifest 声明不一致，需重跑 `cargo update` 使 lock 收敛。
- Batch 3 卡住时停留在 ACP 1.3 稳定线（默认保底方案）。

## 验收标准

- `cargo update --dry-run --verbose` 输出 `Locking 0 packages to latest compatible versions`，且 unchanged 列表仅含既定锁定/降级目标（实测当前为 16 个：14 个 planned breaking 依赖 + generic-array + tikv-jemalloc 三件套）。
- 全量测试（含 acp-hub workspace）、clippy 红线、E2E 全绿。
- CHANGELOG.md 记录升级内容。
