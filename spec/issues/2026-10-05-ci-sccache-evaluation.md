# CI 编译缓存（sccache）接入评估 —— 候选 A5

- 日期：2026-10-05
- 范围：`.github/workflows/ci.yml`（77 行）、`pre-release.yml`（195 行）、`release-agent.yml`（154 行）的缓存与构建策略
- 关联：`spec/issues/2026-10-05-rust-build-performance-diagnosis.md` §四 A5（以及 §三 A1 thin LTO / A2 debug=1 的叠加关系）
- 结论：**本轮不修改任何 workflow 文件**。给出理由、前置审计项、可直接落地的补丁与回退方式（见 §5/§7）。
- 验证限制：本任务禁止运行 cargo 构建、禁止触发 CI，因此所有与 CI 运行时相关的判断都标注为「需首次真实 CI run 确认」（§7）。

---

## 一、结论（TL;DR）

1. **A5 的收益上限被 sccache 的能力边界压窄了**（源码级核实，§4d）：可缓存的是各 crate 的 **lib 单元（rlib / rmeta，含 clippy 的 metadata 单元）**；`--all-targets` 里的 **test target、bin、proc-macro、build script、以及所有链接步骤都不可缓存**。而本仓库 CI 的 `--all-targets`（666 个测试文件、7212 个测试）恰恰是重头。
2. **rust-cache 已经把「依赖层」覆盖掉了**，sccache 的净增量只剩「workspace 各 crate 的 lib / check 单元」。而这部分是否命中，强依赖 PR 改的是哪一层 crate：改 peri-tui（叶）几乎全命中，改 peri-config / peri-model（基座）几乎全 miss。
3. **两者共用同一个 10 GB 仓库缓存池，且是 LRU 驱逐**。sccache 冷启动一轮会写入千级对象（每对象一次上传，仓库级限流 200 次/分钟），足以把 rust-cache 的依赖快照挤掉——那才是当前 CI 提速的主力。**净效果可能为负**，这是本轮不实施的核心理由。
4. 现状池占用已偏紧：`release-agent.yml` 用 `actions/cache` **原样缓存 6 个 target 目录**（release，clean target 约 1.6 GB/个，见诊断报告 §1.5），另有 pre-release 的 2 个 rust-cache + wasm。
5. 因此推荐路径：**先做 3 项只读审计（§6，约 10 分钟）→ 若通过，按 §5 的补丁做 Linux 单腿试点 → 用 2 周命中率/job 时长决定推广或回退**。补丁已写好并通过 YAML 校验，但未应用。

---

## 二、现状：CI 缓存与构建策略

### 2.1 `ci.yml`（每个 PR / push 到 main、pre-release/main）

| 项 | 内容 |
| --- | --- |
| 矩阵 | ubuntu-latest(x86_64-unknown-linux-gnu)、macos-latest(aarch64-apple-darwin)、windows-latest(x86_64-pc-windows-msvc) |
| env | `CARGO_TERM_COLOR=always`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0` |
| 缓存 | `Swatinem/rust-cache@v2`，`key: ${{ matrix.target }}`（每个平台一份快照） |
| 构建 | `scripts/cargo-rmcp-patched.sh build --locked --workspace --all-targets`（dev profile） |
| 测试 | 两段 `cargo test`（第二段 `--test-threads=1`） |
| 检查 | 层序门（仅 Linux）+ `cargo clippy --workspace --all-targets -- -D warnings` |
| 无 | 无 `CARGO_INCREMENTAL` 显式设置（由 rust-cache 隐式置 0）、无 `RUSTC_WRAPPER`、无 sccache、无 `CARGO_TARGET_DIR` |

### 2.2 `pre-release.yml` / `release-agent.yml`

| 维度 | pre-release.yml | release-agent.yml |
| --- | --- | --- |
| 触发 | push 到 `pre-release/main`、workflow_dispatch | tag `agent-v*` |
| 矩阵 | linux-x86_64、macos-aarch64、wasm32-unknown-emscripten | 6 个 target（含 aarch64/riscv64 交叉） |
| 缓存 | `Swatinem/rust-cache@v2`（按 target 一份） | `actions/cache@v4` ×2：`~/.cargo/registry*`+`~/.cargo/git/db`，以及 **整个 `target`**；无裁剪、无 save-if |
| profile | release（当前 `lto=true`、`codegen-units=1`、`opt-level="z"`、`strip="symbols"`） | 同左 |
| 交叉编译 | — | Linux 腿走 `cargo-rmcp-patched.sh --cross`（容器内编译） |
| timeout | 60 / 90 分钟 | 无 |

**要点**：release-agent 的整 `target` 缓存是仓库 10 GB 池里体量最大、且从不做有效清理（依赖/非依赖、workspace crate、incremental 目录全在里面）的条目。任何新增的写入型缓存都要和它抢同一个池子。

### 2.3 关键机制（决定 A5 能否成立）

- `Swatinem/rust-cache@v2` 缓存 `~/.cargo` 与 `./target`，但**只保留依赖产物**：落盘前会删除「不再使用的依赖」「非依赖文件」「incremental 产物」，并**明确不缓存 workspace 各 crate 自身**；同时它**自动设置 `CARGO_INCREMENTAL=0`**（README 原文："this action automatically sets `CARGO_INCREMENTAL=0`"）。
- 缓存 key 自动包含 job id、rustc 版本、`Cargo.lock`/`Cargo.toml`/`rust-toolchain`/`.cargo/config.toml` 哈希与环境变量前缀（`CARGO CC CFLAGS CXX CMAKE RUST`）；`key:` 输入是「附加」维度。
- 后端是 GitHub Actions cache 服务：**仓库级 10 GB 上限**，超限按**最后访问时间 LRU 驱逐**；超过 7 天未访问的条目会被删除；**上传限流 200 次/分钟/仓库、下载 1500 次/分钟/仓库**。
- 分支可见性：PR run 可以恢复 base 分支与默认分支的缓存，但 **PR 自己写入的缓存 scope 是 `refs/pull/N/merge`，只有该 PR 的后续 run 能恢复**，不会分享给其他 PR，也不会写进默认分支 scope。

这两条（LRU 驱逐 + PR 写入不可共享）是 §3a 容量分析的依据。

---

## 三、sccache 接入方式核实（含出处）

| 项 | 结论 | 出处 |
| --- | --- | --- |
| 推荐 action | `mozilla-actions/sccache-action`，**最新 tag `v0.0.11`**（node24 runtime；inputs 仅 `version` / `token` / `disable_annotations`） | <https://github.com/Mozilla-Actions/sccache-action/releases>（v0.0.11 为 Latest）、<https://github.com/Mozilla-Actions/sccache-action/blob/v0.0.11/action.yml> |
| 安装内容 | 默认拉取「最新 sccache release」（当前最新为 v0.18.0），可用 `with: version:` 固定；**低于 v0.11.0 的 sccache 与新版 Actions cache 服务不兼容** | <https://github.com/Mozilla-Actions/sccache-action/blob/v0.0.11/README.md>（"Versions prior to sccache v0.11.0 probably will not work."）、<https://github.com/mozilla/sccache/releases> |
| 需要 workflow 自己设的环境变量 | `SCCACHE_GHA_ENABLED: "true"` 与 `RUSTC_WRAPPER: "sccache"`（action 只做安装与 stats，不代设） | 同上 README「Rust code」小节 |
| 后端启用与凭据 | GHA 后端由 `SCCACHE_GHA_ENABLED=on/true` 开启，依赖 runner 提供的 `ACTIONS_RESULTS_URL` / `ACTIONS_RUNTIME_TOKEN` | <https://github.com/mozilla/sccache/blob/main/docs/GHA.md> |
| 清理/只读开关 | `SCCACHE_GHA_VERSION`（改值即让旧对象不可达）、`SCCACHE_GHA_RW_MODE=READ_ONLY` | 同上 |
| 限流行为 | "In case sccache reaches the rate limit of the service, the build will continue, but the storage might not be performed."（**静默丢失写入**） | 同上 |
| stats 查看 | action 的 post 步骤自动输出统计与注解；手动 `${SCCACHE_PATH} --show-stats` | sccache-action README |
| 与 2025 缓存服务迁移的关系 | 2025-03 的 Actions cache 服务下线波及 `mozilla/sccache` 与 `Mozilla-Actions/sccache-action`，需升级到最新版本 | <https://github.blog/changelog/2025-03-20-notification-of-upcoming-breaking-changes-in-github-actions> |
| 低信任触发的只读缓存 | `pull_request` **不受影响**（其 scope 本就是 merge ref）；`push` 保持读写。`pull_request_target`/`issue_comment`/`workflow_run` 才是只读 | <https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows>、<https://github.blog/changelog/2026-06-26-read-only-actions-cache-for-untrusted-triggers> |
| 容器 runtime | GitHub 已移除 node20（2026-09-23），v0.0.11 已是 node24 → 无兼容风险 | <https://github.blog/changelog/2026-09-23-node-20-is-no-longer-available-in-github-actions> |

---

## 四、关键分析

### a. sccache 与 rust-cache 的分工与共存

| 维度 | `Swatinem/rust-cache@v2` | sccache（GHA 后端） |
| --- | --- | --- |
| 缓存粒度 | 整棵 `target` 依赖产物 + `~/.cargo` 快照，按 **key** 命中 | 每次编译调用一个对象，按**内容哈希**命中 |
| 覆盖 | 依赖的 build 产物；不覆盖 workspace crate | 依赖 + workspace 的 **lib/check 单元**；不覆盖链接类（§4d） |
| 失效形态 | key 变化（Cargo.lock / rustc / 环境哈希）→ **整体 miss** | 单对象失效，可跨 key 部分命中 |
| 存储后端 | GitHub Actions cache | 同一个 GitHub Actions cache（**同一个 10 GB 池**） |
| 冷启动成本 | 一次性下载一个大 blob | 逐对象按需下载（命中即省编译，miss 则边编译边上传） |

- **重叠是主要矛盾**：依赖的编译对象会被存两份（rust-cache 的 target 快照里一份、sccache 对象里一份）。在 10 GB 池里这是实打实的容量竞争，而不是「配置冲突」。
- 三种共存策略：
  1. **保守（推荐先走）**：保留 rust-cache 现状，sccache 只作为「workspace 层 + 抗驱逐」的增量层，并先限制在单腿。
  2. **激进**：`rust-cache` 设 `cache-targets: false`，只保留 `~/.cargo`，编译产物全交给 sccache。省一份存储，但 CI 编译时间完全押在 sccache 命中率与逐对象下载延迟上，**在没有 CI 实测数据前不建议**。
  3. **要避免**：在 release-agent 的整 target 缓存未治理前继续加大写入量——那只会加速互相驱逐。
- 需要明确的归属：**`~/.cargo` 归 rust-cache；`target/` 的依赖产物归 rust-cache；「可缓存编译单元的对象」归 sccache**。三者中真正新增的只有第三项的 workspace 部分。

### b. 与 cargo incremental、`cargo-rmcp-patched.sh` 的关系

- **incremental**：sccache 源码中对 `-C incremental` 明确 `cannot_cache!("incremental")`；`docs/Rust.md` 也要求关闭。`ci.yml` 下 rust-cache 已自动 `CARGO_INCREMENTAL=0`，**现状已满足**。
  注意一个隐性依赖：一旦将来去掉 rust-cache（或换掉它），必须显式设置 `CARGO_INCREMENTAL=0`，否则 dev profile 下 workspace crate 默认 incremental → 这些单元全部静默 bypass，只在 sccache stats 的 "non-cacheable" 里体现。
- **`scripts/cargo-rmcp-patched.sh`**：
  - `RUSTC_WRAPPER` 是 cargo 读的环境变量，脚本只做 `exec cargo --config <patch-config> ...`，不覆盖也不清除它 → **兼容**，patch 进去的 path 依赖（rmcp / hyper-util）同样走 sccache。
  - 被 patch 的 crate 源码位于 `target/peri-rmcp-patches/<sha>/…`（在 `target/` 内）：sccache 会把源码路径与内容纳入哈希，路径稳定则 key 稳定。若该目录被 rust-cache 的文件级清理裁掉，脚本会重新下载重建（内容相同 → key 不变，只多一次网络下载）。这是现状问题，不因 sccache 改变。
  - **clippy 分支**：脚本走 `exec cargo clippy --config ...`；cargo 会给 workspace 成员加 `RUSTC_WORKSPACE_WRAPPER=clippy-driver`，外层仍是 `RUSTC_WRAPPER=sccache`，实际命令形态为 `sccache clippy-driver rustc …`。sccache 源码对此有专门处理（见 §4d 证据 3）→ **兼容，且 clippy 的 metadata 单元可缓存**。
  - **唯一不兼容点**：`--cross` 分支 `exec cross --config ...` 会在容器内调 cargo，而容器里没有 sccache。若把 `RUSTC_WRAPPER` 带进这条路径，交叉编译腿有失败风险（cross 是否透传该变量需实测）。**这正是 `release-agent.yml` 不应启用 sccache 的硬理由之一。**

### c. 与 thin LTO（A1）叠加

- `ci.yml` 是 dev/test/clippy，不含 LTO → **A1 不改变本文件的缓存键，两者无交集**。
- release 侧：`lto` / `codegen-units` / `opt-level` / `debug` 都参与 rustc 参数哈希 → **A1 落地瞬间，所有 release 对象一次性失效**（一次冷启动成本）。之后：
  - fat LTO 下 53% 的时间在最终 bin 的 LTO+link（实测 112.1s），而 **bin 不可缓存**；thin LTO + CGU=16 把这一块压到 24.7s，把工作量挪回「逐 crate 前端 + codegen」（可缓存）→ **sccache 可覆盖的时间占比上升，A1 与 A5 同向，不冲突**。
  - 但 release 侧 sccache 的增量同样只到「workspace lib 的 rlib」层，最终 `--bin peri` 仍不可缓存。
- **上线顺序建议**：A1（thin LTO）先落地并观测稳定，再评估 release 侧 sccache；A2（`debug=1`）同理。否则多组参数变更叠加，缓存全量失效与收益数据互相污染，无法归因。

### d. sccache 的能力边界（源码级核实，决定收益上限）

依据 `mozilla/sccache` 主分支 `src/compiler/rust.rs` 的 `parse_arguments`：

1. **test / bench target 不缓存（两条独立路径都会拒绝）**：
   `cannot_cache!("crate-type", "No crate-type passed")` —— cargo 对 test / bench target 不传 `--crate-type`（`--test` 自行推断为 bin）；即便某些 cargo 版本传了 `--crate-type bin`，也会命中另一条 `if !others.is_empty() { cannot_cache!("crate-type", …) }`。所以 `--all-targets` 下的测试目标**在 build 与 check（clippy）两种模式下都不能缓存**。
2. **链接类 crate 不缓存**：
   `cannot_cache!("crate-type", others)` 与 Known Caveats 的 "Crates that invoke the system linker cannot be cached. This includes `bin`, `dylib`, `cdylib`, and `proc-macro` crates." → 本仓库的 `peri-tui`/`peri-wasm` bin、依赖中的 proc-macro、build script 均不可缓存。
3. **clippy / `cargo check` 是支持的**：
   - `let profile = if emit.contains("link") { profile } else { None };` 注释写着 "which means we are running `cargo check`"；
   - 另有 `emit_generates_only_metadata` 分支专门处理 `--emit=metadata,dep-info` 的输出（保留 `.rlib`/`.rmeta`）；
   - 首参特判：`if idx == 0 && value == "rustc" { /* likely called via clippy-driver */ continue; }`。
   → **clippy 的 lib metadata 单元可缓存**（`docs/Rust.md` 里 "link must be present" 的旧描述已与源码不一致，以源码为准）。
4. **incremental 不缓存**：`("incremental", _) => cannot_cache!("incremental")`。

落到本仓库 CI 的覆盖表：

| 编译单元 | 可缓存 | 现由谁覆盖 | 说明 |
| --- | --- | --- | --- |
| 依赖 lib（rlib/rmeta） | ✅ | rust-cache（已有） | sccache 会重复存一份 |
| **workspace lib（rlib/rmeta）** | ✅ | **无人覆盖** | **sccache 的主要增量** |
| workspace test target（`--all-targets`） | ❌ | — | 链接类/无 crate-type；666 个测试文件、报告 §1.4「补编 74.8s」 |
| bin（peri-tui / peri-wasm） | ❌ | — | 链接 |
| proc-macro / build script | ❌ | — | 链接 |
| 依赖中的 C 编译（aws-lc-sys、jemalloc-sys…） | ✅ | rust-cache（已有） | 重复 |

---

## 五、为什么不实施（决策与触发条件）

| # | 反对理由（证据） |
| --- | --- |
| 1 | 收益天花板：可缓存增量只剩 workspace lib / check 单元，test target 与所有链接步骤被硬排除（§4d 源码级）。 |
| 2 | 命中面随 PR 内容剧烈波动：改 peri-tui（叶）近乎全命中；改 peri-config / peri-model（基座）→ 上游链路全 miss（诊断报告 §一「依赖宽度极窄、串行链」）。 |
| 3 | 10 GB 池 LRU 驱逐（官方文档），sccache 冷启动写入千级对象 → **可能把当前最大收益来源（rust-cache 依赖快照）挤掉**，净效果为负。 |
| 4 | 仓库级限流 200 上传/分钟，超限时 sccache **静默跳过写入**（GHA.md），冷启动那几轮的对象很可能存不全。 |
| 5 | PR 写入 scope 为 `refs/pull/N/merge`，跨 PR 不可复用；共享池的「热源」只能靠 main 的 push run。PR 数量一多就是纯写入压力。 |
| 6 | 现状池占用已偏紧（release-agent 6 个整 target 缓存 + pre-release 2 个 + ci.yml 3 个）。 |
| 7 | 本任务无法本地跑 cargo / CI，任何写入型改动只能靠首次真实 run 检验；若触发驱逐，代价是**三平台同时变慢**。 |

**推进试点的触发条件（满足 ≥2 条）**：

- a) 缓存审计显示仓库总占用 **< 约 6 GB**（给 sccache 留 ≥4 GB 余量）；
- b) 近 30 天 ci.yml 的 rust-cache 命中正常（job 日志中低频出现 "Cache not found"）；
- c) 一次真实 CI run 的分步耗时显示 `build`/`clippy` 里 workspace 层占比可观（用于估算上限）；
- d) （可选，见 §9）先完成 release-agent 的缓存治理。

---

## 六、前置审计（只读，10 分钟）

1. 仓库缓存总量与条目明细：repo → **Actions → Caches**（可按 Size 排序），或 `gh cache list --limit 100`。重点看 `release-agent` 的 6 个 `*-build-*`、`Linux-cargo-*` 与 rust-cache 的条目各占多少。
2. ci.yml 最近 10 次 run 中 rust-cache 步骤的日志（是否 "Cache restored from key"，还是 "Cache not found"）。
3. 一次 CI run 的分步耗时（Build / Test ×2 / Clippy 各多少秒），作为 A5 收益上限的基线。

---

## 七、待落地的补丁（未应用，已通过 YAML 校验）

> 形态：**Linux 单腿试点**。写入量降到 1/3，风险有界、可对照（另外两腿是天然对照组），且可在 2 周内给出「推广 / 回退」的数据。
> 注意：环境变量必须**逐腿注入**。若把 `RUSTC_WRAPPER=sccache` 写进 job 级 `env`，而没有安装 sccache 的腿（macOS/Windows）会因找不到该程序导致 cargo **直接失败**。

```diff
--- .github/workflows/ci.yml
+++ .github/workflows/ci.yml
@@ -47,6 +47,23 @@
                   targets: ${{ matrix.target }}
                   components: clippy

+            # 编译对象缓存（Linux 单腿试点）：sccache 走 GitHub Actions cache 后端。
+            # 仅 Linux 写入，避免三平台同时写入挤占 10GB 仓库缓存池。
+            # 环境变量必须逐腿注入：若在 job 级 env 全局开启而某个腿没有安装
+            # sccache，cargo 会因找不到 RUSTC_WRAPPER 指定的程序而直接失败。
+            - name: Set up sccache (Linux)
+              if: runner.os == 'Linux'
+              uses: mozilla-actions/sccache-action@v0.0.11
+
+            - name: Point cargo at sccache (Linux)
+              if: runner.os == 'Linux'
+              shell: bash
+              run: |
+                  {
+                      echo "SCCACHE_GHA_ENABLED=true"
+                      echo "RUSTC_WRAPPER=sccache"
+                  } >> "$GITHUB_ENV"
+
             - name: Cache cargo registry and build
               uses: Swatinem/rust-cache@v2
               with:
```

- 位置：`ci.yml` 第 49 行之后、`Cache cargo registry and build` 之前（`Install Rust toolchain` 之后）。
- 版本核实：`mozilla-actions/sccache-action@v0.0.11` = 该仓库 Releases 首项（Latest），action.yml 在该 tag 下 runtime 为 `node24`；出处见 §三。
- 可选加强（按需再加，非必需）：`cache-on-failure` 无关；如需固定 sccache 版本，`with: version: "v0.18.0"`（≥ v0.11.0 即可）。
- 若审计通过且试点收益确认为正，再推广到全腿（届时可改为 job 级 `env` + 三腿都装 action）；`pre-release.yml` / `release-agent.yml` 本轮**不改**，理由：release 侧收益仅到 workspace lib 层、且与 A1 的失效窗口叠加；release-agent 还有 cross 容器无 sccache 的兼容问题。

**已完成的本地校验**：5 个 workflow 与上述补丁版本均用 `python3 + PyYAML 6.0.2` `yaml.safe_load` 解析通过；补丁应用后 step 序列为 `Checkout → Check §0 → Install toolchain → Set up sccache(Linux) → Point cargo at sccache(Linux) → Cache cargo registry → Build → Test ×2 → Clippy`，`if: runner.os == 'Linux'` 条件正确。

---

## 八、回退方式

- **最小回退**：删除补丁中带 `+` 的 15 行（2 个 step）。rust-cache、构建命令均未改动，无需其他动作。
- **清掉已写入的 sccache 对象**：把 `SCCACHE_GHA_VERSION` 设为新值即可让旧对象不可达（随后 7 天无访问自动清理 + LRU 驱逐）；或在 Caches 页面 / `gh cache delete` 按 key 前缀清理。
- **临时止血（不改文件）**：给该步骤加 `if: false`，或把 `RUSTC_WRAPPER` 去掉只留安装步骤。

---

## 九、相邻发现（不在本任务范围，仅记录）

`release-agent.yml` 的两条 `actions/cache` 把整个 `target` 原样缓存（6 个 target × release），既无依赖级裁剪、也无 `save-if` 限制，是 10 GB 池里体量最大且最不「经济」的占用者，也是 ci.yml 缓存被驱逐的头号嫌疑。建议**单独一次改动**评估：改用 `Swatinem/rust-cache@v2`（按 target 分 key），或至少限定到 `target/${{ matrix.target }}/release` 并加保存条件；同时确认其 Linux 交叉编译腿不受 `RUSTC_WRAPPER` 影响。此项需要独立的验证与回退设计，本轮不动。

---

## 十、未验证项清单（必须首次真实 CI run 才能确认）

1. sccache 在 GHA 后端的实际命中率 / 未命中数 / 写入数 / `non-cacheable` 计数（action post 步骤的注解与 stats）。
2. **clippy 单元是否真的产生命中**：源码支持（§4d），但需在 stats 中确认 clippy 调用进入 Compile requests 且有 cache hits。
3. 是否触发 **200 上传/分钟**限流（表现：写入数明显低于未命中数，且日志无报错——sccache 静默跳过）。
4. rust-cache 是否因 sccache 写入被 LRU 驱逐：观察 rust-cache 步骤日志的 restored key 与 `cache-hit`，以及 Caches 页面总量变化。
5. Linux 腿（试点）与 macOS/Windows 腿（对照）的 Build/Clippy 分步耗时差。
6. 首次启用后的一轮必然全 miss；需要第 2~3 轮才能看出真实命中率（PR scope 只对本 PR 复现，共享池靠 main push 预热）。
7. `RUSTC_WRAPPER` 生效路径：日志中应能看到 `sccache rustc …`（或 `sccache clippy-driver rustc …`），以及 `cargo-rmcp-patched.sh` 的 patch 依赖同样被包裹。
8. 缓存池治理动作（§九）尚未执行——它的收益/代价同样是估算，未实测。

---

## 十一、CI 上关注点（怎么看）

| 观察对象 | 位置 | 判读 |
| --- | --- | --- |
| sccache 命中率 | job summary / annotation（action post 步骤自动输出），或加一步 `${SCCACHE_PATH} --show-stats` | 命中率 = hits /(hits+misses)；`non-cacheable` 计数即 §4d 的 test/bin/proc-macro 部分，**不要把它算进分母** |
| sccache 写入是否被限流 | 同上 stats：`cache writes` 与 `cache misses` 的比例 | 写入数远小于未命中数 → 触发限流，说明需要缩范围 |
| rust-cache 是否仍命中 | 该步骤日志的 restored key / `Cache not found`，以及 job summary | 出现 "Cache not found" 频次上升 = 被驱逐，是回退信号 |
| 10 GB 池 | repo → Actions → Caches（按 size 排序），或 `gh cache list` | 总量逼近 10 GB 时先治理 §九 再谈推广 |
| 分步耗时 | 每个 job 的 step 时间（Build / Test / Clippy） | 试点腿 vs 对照腿；Clipy 步骤的收益最能反映 workspace check 单元的命中情况 |
