# Wave 2 外部依赖链瘦身：屏蔽实验、结论与证据

状态：**评估完成，三项候选均判定不实施**（各有证据），无源码改动。证据来自 4 次 clean dev 构建、依赖图/feature 溯源与上游 manifest 审查。测量位置：worktree `.worktrees/w2-deps`（分支 `perf/w2-deps`，基于 `3e6c7cc1`，已含 Wave 1：thin LTO / debug=1 / clippy 收窄）。

日期：2026-10-05。机器：macOS 26.x、Apple Silicon 18 核、48GB；rustc/cargo 1.99.0。所有 cargo 命令经 `scripts/cargo-rmcp-patched.sh`；测量均用独立 `CARGO_TARGET_DIR=/tmp/peri-w2-deps-*`。

对应主报告 `spec/issues/2026-10-05-rust-build-performance-diagnosis.md` 的 §1.2、§1.4、归因 3、§四 B2/B3/B4。

## 摘要

| # | 候选 | 结论 | 一句话证据 |
| --- | --- | --- | --- |
| 1 | 屏蔽实验：aws-lc-sys 是否在关键路径 | **否** | 移除它（换 ring）后 CPU `-17.0s (-5.1%)`，墙钟 `+2.8s`（80.7→83.5s，调度噪声内）；工作区起点仅提前 1.9s |
| 2 | provider 切换 aws-lc-rs → ring | **不实施** | 单靠本仓库 feature 翻转**移不掉** aws-lc-sys（`turso_serverless` 强制 `reqwest/default → rustls`）；真正移除需 patch 第三方 crate manifest + 8 个 crate/约 18 处 provider 装机，收益为墙钟 ≈0 |
| 3 | ICU 数据 crate（url→idna）裁剪 | **不可行** | `url 2.5.8` manifest 硬编码 `idna` 的 `compiled_data` feature，下游无任何开关可关；且 Wave 1 后其编译成本已从 24.6s 降至 ~10s |
| 4 | libsqlite3-sys 换 unbundled/预编译 | **不实施** | `sqlx 0.9` 的 `sqlite` feature 硬编码 bundled；unbundled 走 `buildtime_bindgen → bindgen`（需 libclang + 系统 libsqlite3），编译成本反而更高 |

附带发现（§五）：cargo `--timings` 的单元 duration 含 I/O 等待与排队，Σduration 是实测 CPU 的 1.9~2.6 倍，不能当 CPU 用；本 worktree 当前 front window 约 36s（不是 48.6s），尾部仍由 jemalloc 与 workspace 串行链把门。

## 一、屏蔽实验：aws-lc-sys 是否真在关键路径

### 1.1 实验前必须先解决的阻塞发现

只改**本仓库**的 reqwest features 不能移除 aws-lc-sys。证据（`cargo tree -e features -i reqwest`）：

```
reqwest feature "rustls"
└── reqwest feature "default-tls"
    └── reqwest feature "default"          ← 仅由 turso_serverless 启用
        └── turso_serverless v0.1.3        （peri-resources 的生产依赖）

reqwest feature "rustls-no-provider"       ← 本项目 6 个内部 crate 的声明
```

- `reqwest 0.13.4`：`default-tls = ["rustls"]`、`rustls = ["__rustls-aws-lc-rs", "dep:rustls-platform-verifier", "__rustls"]`、`rustls-no-provider = ["dep:rustls-platform-verifier", "__rustls"]`。即 provider 由 `rustls` feature 唯一决定。
- `turso_serverless 0.1.3` 的 manifest 对 reqwest 使用**默认 features**（`reqwest = { version = "0.13", features = ["json","stream"] }`，无 `default-features = false`）——它把 `default → default-tls → rustls → aws-lc-rs` 钉死在依赖图里（Cargo feature 是加性的，下游无法关闭）。
- 排除其他嫌疑：`rmcp 3.5.0` 的 `transport-streamable-http-client-reqwest` 只启用 `__reqwest`（不再启用 `reqwest?/rustls`）；`rustls-platform-verifier 0.7.0` 无 provider feature；`oauth2` 的 reqwest 是 `default-features = false`。

因此屏蔽实验采用**本地 path 掩蔽**（仅实验，未入库）：把 `turso_serverless 0.1.3` 复制到 `/tmp/turso-mask/`，把其 reqwest 依赖改为
`default-features = false, features = ["json","stream","charset","http2","system-proxy","rustls-no-provider"]`（除 provider 外保持原有 feature 面），再用 `--config` 注入 `[patch.crates-io]`。掩蔽后 `cargo tree -i aws-lc-rs` 返回 *did not match any packages* → aws-lc-sys/aws-lc-rs 完全退出依赖图。

> 工具性注意：`--config` 的 `[patch.crates-io]` 表是**覆盖**而非合并——单独追加一个 config 会把 `cargo-rmcp-patched.sh` 的 rmcp/hyper-util/mio/tokio patch 全部顶掉（实测导致 rmcp 编译失败）。实验使用的是脚本配置 + turso 条目合并后的单一 config。

### 1.2 实验矩阵（4 次 clean dev 构建）

命令统一为 `CARGO_TARGET_DIR=/tmp/peri-w2-deps-* build -p peri-tui --bin peri --timings`（与主报告 §六 的基线命令一致）。

| # | 配置 | 并发负载 | 墙钟 | user+sys（实测 CPU） | 单元数 |
| --- | --- | --- | --- | --- | --- |
| A | 基线（现状：aws-lc-rs） | **有**（另一会话 `cargo test`，`/private/tmp/peri-ci-validation`） | 87.5s | 340.3s | 650 |
| C | 基线（同 A，无并发对照） | 无 | **80.7s** | **333.9s** | 650 |
| B | ring + 去 aws-lc（turso 掩蔽） | 无 | **83.5s** | **316.9s** | 640 |
| D | 基线 + 依赖 `debug=0`（§5.2，受污染） | 有 | 95.4s | 322.2s | 650 |

A 与 C 的差（+6.8s 墙钟）即并发负载的干扰量级，说明墙钟只在 A/C 同条件对之间可比。

### 1.3 结果（C vs B，同条件对）

**构建单元增删**（timings 单元级）：

| 变化 | 单元 | 单元时长 |
| --- | --- | --- |
| 移除 | `aws-lc-sys` build-script (run) | -26.6s |
| 移除 | `aws-lc-rs` build-script/lib (run) | -3.2s |
| 移除 | `cmake` / `jobserver` / `fs_extra` / `dunce`（aws-lc-sys 的构建工具链） | -0.4s |
| 新增 | `ring` build-script (run) + lib | +12.8s |

**指标**：

| 指标 | C（aws-lc） | B（ring） | Δ |
| --- | --- | --- | --- |
| 实测 CPU（user+sys） | 333.9s | 316.9s | **-17.0s（-5.1%）** |
| 墙钟 | 80.7s | 83.5s | +2.8s（无收益，落在调度噪声内） |
| 工作区起点（`peri-acp-types` start） | 35.8s | 33.9s | -1.9s |
| 尾部：`peri-middlewares` → `peri-tui` → link | 53.3→80.5s | 49.6→83.3s | 链长相当 |
| 最长单条依赖链 | `aws-lc-sys` 链 27.9s | `tikv-jemalloc-sys` 链 27.9s | 换了一条门 |

**解读**：

1. aws-lc-sys 是图中**最长单条依赖链**（26.6s 的 build-script run），移除后最长链变成 `tikv-jemalloc-sys`（27.1s）——两条链长度几乎相同，所以 B 的墙钟没有变好。
2. 真实 CPU 只有 ~334s（见 §5.1），build 并非全程 18 核饱和（平均并发 ≈4）。移除 17s CPU 摊到 80s 的构建里不足 1s 墙钟，被调度波动（B 中 jemalloc 的 build-script run 从 23.7s 推迟到 31.2s 起跑）完全淹没。
3. 主报告「aws-lc-sys 结束于 t=43.0s，与 peri-acp-types 起跑 48.6s 相邻 → 高度疑似关键路径」的判断**不成立**：本 worktree（Wave 1 后、无并发）基线里 aws-lc-sys 结束于 31.7s，而 `peri-acp-types` 起跑 35.8s；把 aws-lc-sys 整条链删掉，起跑也只提前到 33.9s。

**结论：aws-lc-sys 不在 dev 构建的墙钟关键路径上；换 ring 不产生可测量的构建时间收益（墙钟），只减少 ~5% 的实际 CPU。**

## 二、provider 切换（aws-lc-rs → ring）：不实施

### 2.1 如果实施，改动面清单（已核实）

1. **patch 第三方 manifest**：`patches/turso-serverless-0.1.3-reqwest-features.patch` + `scripts/cargo-rmcp-patched.sh` 增加第三个下载/打补丁块。有两个额外坑：
   - 脚本的 `cargo-config.toml` 是**只在 rmcp patch 缓存目录首次创建时写入**的；新增 patch 条目后，老缓存机器不会重写该文件，会出现「补丁时灵时不灵」的构建不一致，必须同时改配置生成/失效逻辑；
   - `--config` 的 `[patch.crates-io]` 是整表覆盖，不能靠追加第二个 config 文件解决（§1.1 注）。
2. **显式 provider 初始化**：reqwest 在 `rustls-no-provider` 下 `Client::builder().build()` 直接 panic（`ClientConfig` 构造要求进程级 `CryptoProvider`，reqwest 只查 `get_default()`，不会自动安装）。需在**每个构造 reqwest Client 的进程路径**前调用 `rustls::crypto::ring::default_provider().install_default()`：`peri-model`(2 文件/4 处)、`peri-middlewares`(4 文件/5 处)、`mcp-packages/web`(2)、`mcp-packages/artifact`(1)、`langfuse-client`(1)、`peri-tui`(2)，另有 3 个测试文件直接 `Client::new()`（`peri-resources` ×2、`mcp-packages/workspace` ×1）——合计 8 个 crate、约 18 处；每个 crate 还需新增 `rustls` 直接依赖（`default-features = false, features = ["ring"]`，否则 rustls 默认 features 会把 aws-lc-rs 带回来）。
3. **wasm32-unknown-emscripten 需单独验证**：该目标当前确实编译 aws-lc-sys（`target/wasm32-unknown-emscripten/release/build/aws-lc-sys-*` 存在，`pre-release.yml` 会打 wasm 包），ring 在 emscripten 下能否编译/运行未验证；若不行，还要为 wasm 目标保留 aws-lc-rs + 分支装机代码。
4. **安全面**：ring 与 aws-lc-rs 在 rustls 0.23 下消费同一套 rustls 配置，证书校验仍走 `rustls-platform-verifier`（不改）；cipher suite / 签名算法取交集，`prefer-post-quantum` 在当前依赖组合下本来就未启用（reqwest 对 rustls 是 `default-features = false`），因此**没有后量子 KEX 回退**。安全评审面小于切 native-tls，但仍是 TLS 后端替换。

### 2.2 判定

| 维度 | 结果 |
| --- | --- |
| 墙钟收益 | ≈0（C 80.7s → B 83.5s） |
| CPU 收益 | -17.0s / -334s ≈ -5% |
| 改动面 | patch 第三方 manifest + 构建脚本缓存逻辑 + 8 crate/约 18 处装机 + wasm 分支 |
| 维护成本 | 每次 turso_serverless/reqwest 版本升级需重做 patch；构建期新增一次网络下载 |

收益与风险/改动面明显不成比例 → **不实施**。建议：
1. 向上游 `turso_serverless` 提 issue/PR，把 reqwest 依赖改为 `default-features = false`（一行，可让本仓库的 feature 翻转立即生效）；
2. 若 CI 冷构建仍是痛点，优先做 **sccache/缓存**（主报告 A5）与 jemalloc 条件化（B1，本 wave 另一条线），而不是 TLS 后端替换；
3. 若未来一定要换 provider，按 §2.1 清单实施，验证点 = `cargo check --workspace` + TLS 相关集成测试（真实 HTTPS 请求）+ wasm 目标构建。

### 2.3 回退方式

本 worktree **未落地任何改动**（所有实验性编辑已还原，`git status` 干净），无需回退。若未来实施，回退 = revert 该提交（patch 文件 + 脚本 + 装机调用点 + feature 声明），依赖图自动回到 aws-lc-rs。

## 三、ICU 数据 crate（url → idna → icu_*_data）：不可行

**依赖链**（`cargo tree -i icu_normalizer_data`）：`icu_normalizer_data ← icu_normalizer ← idna_adapter ← idna ← url ← {workspace 直接依赖, oauth2 ← rmcp}`。

**为什么裁不掉**（上游 manifest 证据）：

- `url 2.5.8` 对 idna 的声明是
  `[dependencies.idna] version = "1.1.0"`, `default-features = false`, `features = ["alloc", "compiled_data"]` —— **feature 只增不减**，下游无法关闭；
- `idna 1.1.0` 的 `compiled_data = ["idna_adapter/compiled_data"]`；
- `idna_adapter 1.2.2` 的 `compiled_data = ["icu_normalizer/compiled_data", "icu_properties/compiled_data"]`；
- 这两个 `*/compiled_data` 就是把 `icu_normalizer_data` / `icu_properties_data` 两个数据 crate 拉进构建的唯一开关，没有运行时 data provider 的替代路径可用（除非改 url/idna 源码或替换 IDN 实现，属产品语义变更）；
- url 也无法整体绕开：`rmcp → oauth2 5.0 → url 2.5.8` 是生产路径。

**成本现状（重要修正）**：主报告 §1.2 的 24.6s 是 `debug=2` 时代数字。Wave 1（`debug=1`）之后本 worktree 实测：

| 单元 | 时长 |
| --- | --- |
| `icu_normalizer_data` build-script (run) | 4.8~5.1s |
| `icu_properties_data` build-script (run) | 5.2~5.3s |

即合计 ~10s（build-script run，含排队成分），**已比主报告腰斩**，可优化空间进一步缩小。

结论：**不可行（在当前 `url 2.5.8` / `idna 1.1.0` 约束下）**，且继续投入优先级低。定向测试影响面：IDN/UTS-46 语义（`url` 的 domain 解析、`peri-resources`/`mcp-packages` 里的 URL 解析测试）——本轮无改动，无需测试变更。

## 四、libsqlite3-sys / sqlx：不实施

**证据（上游 manifest）**：

- `sqlx 0.9.0`：`sqlite = ["sqlite-bundled", "sqlite-deserialize", "sqlite-load-extension", "sqlite-unlock-notify"]`；`sqlite-bundled = ["_sqlite", "sqlx-sqlite/bundled", ...]`；`sqlite-unbundled = ["_sqlite", "sqlx-sqlite/unbundled", ...]`。
- `sqlx-sqlite 0.9.0`：`bundled = ["libsqlite3-sys/bundled"]`（`cc` 编译 amalgamation），`unbundled = ["libsqlite3-sys/buildtime_bindgen"]`。
- `libsqlite3-sys 0.30.1`：`buildtime_bindgen = ["bindgen", "pkg-config", "vcpkg"]`；`bundled = ["cc", "bundled_bindings"]`。**没有预编译/缓存选项**。

**为什么 unbundled 是负优化**：

1. 需要 `bindgen`（+ `clang-sys`）编译，属于新增的重依赖，其自身编译成本与 bundled 的 `sqlite3.c` 编译（本 worktree C 构建实测两段 build-script (run) 合计 ≈12s）同量级甚至更高；
2. 需要构建机上存在 libclang 与系统 libsqlite3 开发包；本仓库 CI（`.github/workflows/ci.yml`：ubuntu / macos / windows 三平台，未安装 `libsqlite3-dev`/LLVM）与 i386/loongarch64 Docker 交叉构建都要同步改，portability 成本高；
3. 系统 SQLite 版本差异会改变运行时行为（绑定来自本机头文件，`persist`/锁语义可能漂移），违背景观「契约测试」的稳定前提。

结论：**保持 bundled**。收益路径不在 feature，而在缓存复用（sccache/cargo cache）与「同一 target 目录不重复编译」（主报告 D1/D2/A5）。

## 五、附带发现

### 5.1 方法学修正：单元 duration ≠ CPU

同一批构建里，`--timings` 的 Σ单元时长与 `/usr/bin/time` 实测 CPU 之比为 1.9~2.6×：

| 构建 | Σ单元时长 | 实测 CPU（user+sys） | 比值 |
| --- | --- | --- | --- |
| C（无并发） | 627.9s | 333.9s | 1.9 |
| A（有并发） | 734.5s | 340.3s | 2.2 |
| B（无并发） | 791.9s | 316.9s | 2.5 |
| D（有并发） | 854.4s | 322.2s | 2.7 |

单元 duration 含 build-script/rustc 的 I/O 等待与 jobserver 排队，**不能累加当 CPU**，也不能据此推算并行度。建议后续测量：CPU 用 `/usr/bin/time`（user+sys），调度用单元的 start/增删，墙钟只在同条件对之间比较。主报告归因 3 的「650 单元累计 CPU 863s / 平均并行度 8.8」应修正为「Σ单元时长 863s；同口径实测 CPU 约 334s，平均真实并发 ≈4」——**dev 构建并不是 CPU 饱和型，而是受依赖链长度与尾部串行链支配**，这解释了为什么「删掉大 CPU 单元」换不来墙钟收益（§1.3）。

### 5.2 候选：依赖 crate `debug=0`（未采信，待干净复测）

用 `--config` 注入 `[profile.dev.package."*"] debug = false`（workspace 成员保持 `debug=1`）做了一次构建（D，受并发污染）：

| 指标 | C（现状，deps debug=1） | D（deps debug=0） |
| --- | --- | --- |
| 实测 CPU | 333.9s | 322.2s（-3.5%，但 D 受并发干扰） |
| target 磁盘 | 4.4G | **3.9G（-11%）** |
| 墙钟 | 80.7s | 95.4s（**不可比**：D 期间另一会话 `cargo test` 在跑） |

磁盘收益确定；CPU 小幅下降；**墙钟因并发干扰未验证**。代价是依赖代码的 backtrace/单步调试信息消失（workspace 自身仍有 `debug=1`）。建议在无并发时复测后再决定是否采纳（与 Wave 1 的 `debug=1` 决策属同一议题）。

### 5.3 当前 worktree 的尾部门（供其他线对齐）

无并发基线 C：front window 结束（`peri-acp-types` 起跑）35.8s，之后 `peri-resources` 7.6s → `peri-middlewares` 16.9s（53.3→70.2）→ `peri-acp` 4.7s → `peri-tui` 6.9s + link 1.9s，总计 80.5s。其中 `tikv-jemalloc-sys`（build-script run 23.7→49.4s）与 `rmcp`（43.0s 结束）是尾部门的前置；这与 w2-jemalloc 线、workspace 串行链（主报告归因 1）互相印证——**本 wave 的结论是：外部依赖链的「CPU 大户」（aws-lc-sys/ICU/sqlite）都不是墙钟关键路径，继续在此处动刀的收益趋近于零。**

## 六、未验证项

1. **release 构建**未做屏蔽实验（thin LTO 后 release 约 116s，其中 aws-lc-sys 32.8s / jemalloc 37.4s 为旧数据）；结论可能相同，但未实测。
2. wasm32-unknown-emscripten 下 ring 的可行性未验证（无 emsdk 之外的环境依赖、未跑 `cargo-wasm.sh`）。
3. D（deps `debug=0`）的墙钟收益未在无并发环境复测。
4. 上述实验均在 dev profile、`-p peri-tui --bin peri` 目标集下；`--all-targets` / 测试目标集的数字未测。

## 附录 A：未来实施 provider 切换的精确步骤与回退

（仅供决策参考，本轮未实施）

1. 依赖图侧：新增 `patches/turso-serverless-0.1.3-reqwest-features.patch`（把 reqwest 依赖改为 `default-features = false, features = ["json","stream","charset","http2","system-proxy","rustls-no-provider"]`），并在 `scripts/cargo-rmcp-patched.sh` 增加对应的下载/校验/应用块 + **无条件重写 `cargo-config.toml`**（修掉 §2.1 的缓存不一致坑）。
2. 仓库侧：根 `Cargo.toml` 的 reqwest 改为 `features = ["json","rustls-no-provider"]`，workspace 增加 `rustls = { version = "0.23", default-features = false, features = ["ring"] }`。
3. 装机：在 §2.1 列出的 8 个 crate / 约 18 处 Client 构造前调用 `rustls::crypto::ring::default_provider().install_default()`（`let _ =` 吞掉重复安装错误），并在对应 crate 增加 `rustls` 直接依赖；`peri` 二进制 `main()` 亦加一处兜底（覆盖 turso 内部构造路径）。
4. 验证：`cargo check --workspace`；`cargo test`（TLS/HTTP 定向：`peri-model`、`langfuse-client`、`peri-middlewares`、`mcp-packages/web`）；真实 HTTPS 冒烟；`--all-targets` 无 panic（漏装 provider 会在 Client 构造时 panic）。
5. 回退：整提交 revert（无数据迁移、无持久化副作用）。

## 附录 B：复现命令

```bash
cd .worktrees/w2-deps

# A/C 基线（现状）
CARGO_TARGET_DIR=/tmp/peri-w2-deps-ctrl bash -c 'time ./scripts/cargo-rmcp-patched.sh \
  build -p peri-tui --bin peri --timings'

# B ring 实验：仓库改为 rustls-no-provider + rustls/ring，并用合并 config 掩蔽 turso
#   /tmp/combined-patch.toml = 脚本 cargo-config.toml + turso_serverless path patch
CARGO_TARGET_DIR=/tmp/peri-w2-deps-ring3 bash -c 'time ./scripts/cargo-rmcp-patched.sh \
  build --config /tmp/combined-patch.toml -p peri-tui --bin peri --timings'

# 依赖图证据
./scripts/cargo-rmcp-patched.sh tree -e features -i reqwest --depth 3
./scripts/cargo-rmcp-patched.sh tree --config /tmp/combined-patch.toml -i aws-lc-rs   # 应为空

# 单元级分析：解析 target/cargo-timings/cargo-timing.html 内嵌 UNIT_DATA
#   Σduration 与 start/end 用于调度分析；CPU 以 time 的 user+sys 为准（§5.1）
```

## 附：报告范围与限制

- 所有结论仅对 dev profile、`-p peri-tui --bin peri` 目标集成立；墙钟数据受并行 owner 干扰，A/D 已标注，C/B 为同条件对（无其他 cargo 运行，构建前后均有 `ps` 快照）。
- 实验用的 turso 掩蔽只存在于 `/tmp` 与 `--config`，未进入仓库；工作树保持干净（无源码改动）。
