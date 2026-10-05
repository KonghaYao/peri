# Rust 编译性能诊断：基线测量、归因与优化手段

状态：测量与归因完成；**Wave 1–3 优化已实施并提交**（构建配置、平台分配器、端口化解耦、本地缓存指南，见 §七）。成果位于 worktree `.worktrees/build-perf`（分支 `perf/rust-build-speedup`），已完成与 `pre-release/main` 的集成合并。报告主体为 2026-10-05 主工作区实测；标注「实验」的条目通过 `--config` 覆盖或独立 target 目录获得。优化手段为候选清单，优先级是影响判断，不是已批准实施顺序。

日期：2026-10-05。测量机器：macOS（Darwin 26.x）、Apple Silicon 18 核、48GB 内存、204GB 可用磁盘；rustc/cargo 1.99.0；项目规模 47 万行自有代码 / 25 个 workspace crate（根显式 24 + `peri-theme` 经 path 依赖隐式入组）/ 452 个外部依赖 crate（依赖树条目 1665）/ 7212 个测试（666 个测试文件）。

测量期间有其他会话在同一 target 目录并发执行 cargo 命令（test/clippy），已对时效类数据造成干扰；受影响处均已标注。所有命令经 `scripts/cargo-rmcp-patched.sh`（rmcp patch 配置）执行。

## 一、测量基线

### 1.1 全量与增量

| 场景 | 命令要点 | 实测 | 备注 |
| --- | --- | --- | --- |
| 全量 clean dev 构建 | `CARGO_TARGET_DIR=/tmp/… build -p peri-tui --bin peri --timings` | **98.2s** | 650 个编译单元，累计 CPU 863s，平均并行度 8.8（18 核） |
| 全量 clean release 构建 | 同上 + `--release` | **213.8s** | 其中 LTO/链接单元 112.1s（占 53%） |
| `--all-targets` 增量补编 | `build --workspace --all-targets` | **74.8s** | 在已有 dev 构建之上补编全部测试目标 |
| 增量：改 peri-agent 语义 | 追加一个 `pub fn` 后 `build -p peri-tui --bin peri` | **9.6s** | 重编 10 个 crate + 链接；增量健康 |
| 增量：改 peri-acp-types（最底层类型） | 同上 | **13.2s** | 全链波纹重建；增量健康 |
| 增量：仅改注释 | touch + 追加注释 | **8.3s** | rustc 增量缓存有效复用 |

结论：**日常增量场景（8–13s）健康，不是瓶颈**；成本集中在全量构建（首次 / 切分支 / 依赖变更 / CI）与 release 打包。

### 1.2 dev 全量构建的关键路径（98s 的时间构成）

```
[0 ~ 48.6s]  外部依赖编译与排队窗口（650 单元累计 CPU 863s，18 核只跑出 ~8.8 并行度）
    aws-lc-sys          32.5s   （reqwest → rustls → aws-lc-rs 默认 provider）
    tikv-jemalloc-sys   29.1s   （peri-tui 直接依赖）
    icu_properties_data 12.6s   （url → idna 链路）
    icu_normalizer_data 12.0s   （同上）
    libsqlite3-sys      ~13s    （sqlx）
    serde_json 10.9s / serde 10.8s / tokio 11.1s（含 git 源 fork 版）…
[48.6 ~ 98.0s] workspace 内部串行链 49.4s（并行度降到 1~3，18 核无法参与）
    peri-acp-types   4.8s  (48.6→53.4)
    peri-resources   9.5s  (53.4→62.9)
    peri-agent       6.8s  (59.8→66.5)
    peri-middlewares 20.9s (66.0→86.9)  ★ 关键路径最大单点
    peri-acp         7.6s  (80.7→88.3)
    peri-tui         9.4s  (84.7→94.0) + 链接 4.0s (94.0→98.0)
```

内部依赖链的关键路径合计（cargo metadata + timings 重建）：peri-tui 方向 **70.1s**，其中 peri-middlewares 独占 20.9s、peri-tui 自身 13.4s（lib 9.4 + bin 4.0）。

### 1.3 rustc 阶段分布（frontend 单线程，占 60~70%）

| crate | 行数 | frontend | codegen | 合计 |
| --- | --- | --- | --- | --- |
| peri-middlewares | 9.9万 | 14.7s | 6.3s | 20.9s |
| peri-resources | 4.6万 | 6.3s | 3.2s | 9.5s |
| peri-tui | 10.3万 | 5.7s | 3.7s | 9.4s |
| peri-acp | 5.7万 | 4.0s | 3.6s | 7.6s |
| peri-agent | 6.1万 | 4.4s | 2.4s | 6.8s |
| peri-acp-types | 2.8万 | 3.8s | 1.0s | 4.8s |

frontend（宏展开/类型检查/借用检查）在单 crate 内不可并行；只有拆分为多个 crate 才能并行化。

### 1.4 release 全量构建（213.8s）

| 单元 | 耗时 | 说明 |
| --- | --- | --- |
| peri-tui（LTO 链接） | **112.1s** | fat LTO + codegen-units=1，单线程 |
| tikv-jemalloc-sys | 37.4s | C 编译 |
| aws-lc-sys | 32.8s | C 编译 |
| peri-middlewares | 30.4s | frontend 14.9 + codegen 15.5 |
| peri-tui（编译） | 18.2s | frontend 5.1 + codegen 13.1 |
| rmcp / libsqlite3-sys / peri-agent / peri-resources / peri-acp | 15.1 / 13.1 / 12.4 / 12.3 / 10.9s | |

### 1.5 磁盘与缓存

| 项 | 体积 |
| --- | --- |
| `target/debug/incremental` | **7.4G**（peri-middlewares 单项 680M、peri-tui 559M、peri-agent 634M 两份、peri-acp 357M、rmcp 两份） |
| `target/debug` 合计 | 9.4G（开发 target 目录） |
| clean dev target（/tmp 对照） | 6.0G |
| clean release target | 1.6G（fat）/ 1.9G（thin） |
| release 二进制 | 20M（fat LTO）/ 40M（thin LTO 实验） |

## 二、归因

### 归因 1：workspace 内部串行链 —— 98s 中占 49s，是最大结构性成本

- 根因：crate 依赖深度 × 单 crate frontend 单线程。18 核在链的后半段几乎闲置（并行度 1~3）。
- 关键单点：
  - **peri-middlewares（9.9 万行、20.9s）**：关键路径最大件。其 `mcp/` 子目录 5.3 万行（54%）。
  - **peri-tui（10.3 万行、13.4s）**：其中 `kit/` 8.6 万行（83%）。
- 依赖宽度极窄：peri-tui 对 peri-middlewares 的生产引用共 **93 行、分布在 9 个文件**（`plugin` 相关 70 行、`mcp` 相关 23 行，均为装配点与面板数据源）；peri-acp 对 peri-middlewares 的使用面较宽（mcp/plugin/host_ports/tool_search/assembly/subagent/skills/permission/hitl/workflow 均有）。
- 但**直接按目录拆 peri-middlewares 会遇到内部交叉耦合**（模块级引用分析）：
  - `mcp` 被 8 个模块引用，是最大枢纽；`assembly → mcp` 20 处，而 `mcp → assembly` 1 处、`mcp → plugin` 4 处、`mcp → subagent` 1 处（互引）；
  - `subagent → hooks` 11 处、`host_ports → plugin` 11 处、`hooks → permission` 6 处。
  - 结论：拆分前需要先做依赖反转/端口化设计，不是移动文件即可。

### 归因 2：release 的 fat LTO + codegen-units=1 —— 链接单阶段 112s（占 release 总时长 53%）

- fat LTO 需要合并全部 bitcode 后重新优化，CGU=1 使后端串行。对 47 万行 + 全依赖图规模，这一配置是构建时间的最大单项。
- 已验证收益（见 §3.1）：thin LTO + CGU=16 使该阶段降至 24.7s（-78%）。

### 归因 3：外部依赖的 CPU 总量 —— 前 48s 的排队窗口

- aws-lc-sys（32.5s）、tikv-jemalloc-sys（29.1s）、icu_*_data（24.6s）、libsqlite3-sys（13s）等 C/数据 crate 合计贡献约 100s CPU。
- 它们在 18 核上大多并行，但占满 CPU 使 workspace crate 的启动被推迟（peri-acp-types 直到 t=48.6s 才排上队）。
- aws-lc-sys 结束于 t=43.0s、reqwest 结束于 t=47.5s，与 peri-acp-types 的起跑（48.6s）相邻——**高度疑似位于关键路径上**（需一次屏蔽实验确认）。

### 归因 4：测试与检查目标规模

- 7212 个测试 / 666 个测试文件；`--all-targets` 补编 74.8s（会在 CI 与本地提交前检查中重复发生）。
- 大测试文件推高单 crate 编译量：`peri-tui/src/kit/acp_events_test.rs` 单文件 7412 行。
- pre-commit 每次提交跑**全 workspace** `check` + `clippy` + 层序门；clippy 与 check 的 artifact 不共享，缓存冷时成本接近一次全量。

### 归因 5：环境与流程

- **多会话并发共用同一 target 目录**：本次诊断中反复出现 `Blocking waiting for file lock on build directory`（一次 clippy 实测 14.3s 中 14.0s 为等锁）、`--locked` 因并发改写 Cargo.lock 而随机失败、以及一次实验因并发重建 rmcp patch 缓存而失败。
- **一个挂起 12h+ 的测试进程**（`mcp_host_policy_contract`，来自引用已删除 `peri-mcp-lsp` 的旧命令），持有订阅、占用进程槽。
- incremental 缓存 7.4G：大 crate 的增量目录 500~680M，读写本身在删除/重建时成为负担；CI 场景通常应关闭。
- 未启用任何加速设施：无 sccache、无 lld、dev 默认 `debug=2`、无 `[profile]` 调优。

## 三、已验证的优化实验（无损，未改仓库）

### 3.1 release：thin LTO + codegen-units=16（实验）

命令（独立 target，未改 Cargo.toml）：

```bash
CARGO_TARGET_DIR=/tmp/peri-release-thin2 cargo --config <patch-config>.toml \
  build --offline -p peri-tui --bin peri --release --timings
# 追加配置：[profile.release] lto = "thin"; codegen-units = 16
```

| 指标 | fat LTO + CGU=1（现状） | thin LTO + CGU=16 | 变化 |
| --- | --- | --- | --- |
| release 全量构建 | 213.8s | **116.1s** | **-46%（-97.7s）** |
| 其中 LTO/链接单元 | 112.1s | 24.7s | -78% |
| peri-tui 编译单元 | 18.2s | 9.2s | -49% |
| release 二进制 | 20M | 40M | +100% |

### 3.2 dev：debuginfo 降级（实验）

| 指标 | debug=2（现状） | debug=1 | 变化 |
| --- | --- | --- | --- |
| clean dev 构建 | 98.2s | 109.5s | 受并发负载干扰，**不可比**（同批测量 CPU 总量反而 +24%，已确认该时段有其他会话负载） |
| target 磁盘 | 6.0G | 4.8G | **-20%** |
| rustc 命令行 | `-C debuginfo=2` | `-C debuginfo=1` | 已验证生效 |

结论：磁盘收益确定；时间收益待无干扰环境复测（业界经验 10~25%，主要作用于 codegen 与链接）。

## 四、优化手段候选清单

各梯队相互独立，可分批实施；每项给出预期收益、代价与验证方式。收益估算以 §1 数据与 §3 实验为准，「未验证」处均已标注。

### 第一梯队：配置层（低风险、可回退）

| # | 手段 | 预期收益 | 代价 | 验证 |
| --- | --- | --- | --- | --- |
| A1 | `[profile.release]` 改 `lto="thin"` + `codegen-units=16` | release 全量 **-46%**（已实测） | 二进制 20M→40M | §3.1 实验复现 |
| A2 | `[profile.dev] debug=1`（或对依赖设 `debug=0`） | 磁盘 -20%（已实测）；时间 -10~20%（待复测） | 调试体验（变量/类型信息减少，行号保留） | 复测 clean dev + 增量 |
| A3 | pre-commit：clippy 只跑改动 crate（从 `git diff` 提取），全量留 CI | 提交等待从 ~15-60s 降至 ~5-15s | 流程改动；需防"绕过检查" | 模拟提交计时 |
| A4 | 屏蔽实验：确认 aws-lc-sys 是否在关键路径（换 ring / 预编译缓存） | 若在路径上可省 ~30s | 需 TLS provider 决策 | 对照实验 |
| A5 | CI 接入 sccache + 现有 cargo 缓存 | CI 冷启动 -50~70%（业界）；本机多 target/worktree 复用 | 与 incremental 有取舍；需安装 | CI 时长对比 |

### 第二梯队：依赖削减（中风险、需运行时验证）

| # | 手段 | 预期收益 | 代价 | 验证 |
| --- | --- | --- | --- | --- |
| B1 | jemalloc 条件化（macOS 用系统分配器，Linux 保留） | clean 构建 -29s CPU；二进制 -1M+ | 运行时分配性能变化 | 分配基准 + 长会话内存对比 |
| B2 | rustls provider 评估（aws-lc-rs → ring 或平台 TLS） | clean 构建 -20~33s CPU | 安全/TLS 行为变化，需审查 | TLS 集成测试 + 编译对照 |
| B3 | ICU 数据 crate（url→idna 链路）评估可否裁剪 | -24s CPU | 可能影响 IDN/URL 语义 | 定向测试 |
| B4 | libsqlite3-sys / turso_serverless 复查 | -13s+ CPU | 存储路径依赖 | 既有契约测试 |

### 第三梯队：结构改造（高成本、需架构评审）

| # | 手段 | 预期收益 | 代价 | 前置 |
| --- | --- | --- | --- | --- |
| C1 | 拆 peri-tui 的 `kit/`（8.6 万行）为独立 crate | kit 可与 middlewares 系并行编译；改主体不重编 kit | crate 边界与管理成本 | 确认 kit 对 app/ 的依赖方向 |
| C2 | 从 peri-middlewares 抽出 peri-tui 实际使用的 `plugin` + `mcp client` 面（约 1.5 万行） | 关键路径 **-10~20s**（peri-tui 方向）；与既有 M-TUI「TUI 全量改经 ACP」任务线同向 | 需先解内部耦合（mcp↔assembly 互引）；依赖门/豁免清单需同步设计 | 模块依赖反转设计；**与 M-TUI 任务线合并评审**（见 `spec/history/2026-08.md` 2026-08-05 条目与 `scripts/import-exemptions.conf` 收紧任务对照表） |
| C3 | peri-acp 对 peri-middlewares 解耦（host_ports 端口化，既有 L5 收敛项） | 进一步把 20.9s 大件移出关键路径 | 装配面改造，影响 ACP Host 装配路径 | 读 architecture-contracts 中 ACP Host 契约 |
| C4 | 拆 peri-middlewares 的 `mcp/`（5.3 万行） | 化整为零、提升并行度 | 内部交叉耦合多（见归因 1），需端口化先行 | C2/C3 之后 |
| C5 | 测试目标治理（7212 个测试、大测试文件） | `--all-targets` 与 clippy 成本下降 | 需逐文件评估，风险低但琐碎 | 测试清单 |

### 第四梯队：环境与流程

| # | 手段 | 预期收益 | 代价 | 验证 |
| --- | --- | --- | --- | --- |
| D1 | target/incremental 治理：清理陈旧缓存；CI 关 incremental；评估大 crate 增量收益 | 磁盘回收、部分场景提速 | 需定策略 | 复测 |
| D2 | 多会话 target 隔离约定（构建密集任务用独立 `CARGO_TARGET_DIR`） | 消除锁等待与 lockfile 竞态 | 磁盘/重复编译成本 | 并发场景复测 |
| D3 | 清理挂起测试进程（12h+，含已删除 crate 引用） | 环境健康 | 需确认归属 | — |
| D4 | wasm/emscripten 构建基线补齐（本次未测） | — | 需 emsdk 环境 | `scripts/cargo-wasm.sh` 计时 |

## 五、待决事项

1. **release 体积/时间取舍**：用户已表态「优先构建速度」（对应 A1）；若后续需要小体积发布产物，可考虑双 profile（发布走 fat、本地走 `release-fast` 自定义 profile）——尚未实验。
2. **结构改造与既有任务线合并**：C2/C3 与既有 M-TUI（TUI 全量改经 ACP）与 L5（执行本体迁出 peri-acp）收敛任务高度重叠——两者均已压缩至 `spec/history/2026-08.md`（2026-08-05 条目），现行对照表在 `scripts/import-exemptions.conf` 头部「收紧任务对照」。应作为同一任务线评审，避免两套拆分方案。
3. **CI 真实耗时数据缺失**：GitHub Actions runner（2~4 核）下的实际时长未采集；本机数据（18 核）对 CI 的参考性有限，建议以一次真实 CI run 的 job 时长作为 CI 场景基线。
4. **测量环境**：本次多会话并发干扰了时效数据；建议在实施前后各做一次无并发基线（命令见 §六）。

## 六、复现与验证命令

```bash
# 基线（全量 dev；独立 target 目录避免污染开发缓存）
CARGO_TARGET_DIR=/tmp/peri-baseline ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --timings
# 关键路径与单元耗时：解析 target/cargo-timings/cargo-timing-*.html 内嵌 UNIT_DATA

# 基线（release）
CARGO_TARGET_DIR=/tmp/peri-baseline ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --release --timings

# 基线（测试目标，CI 等价）
./scripts/cargo-rmcp-patched.sh build --offline --locked --workspace --all-targets

# 增量场景（改一个 crate 后重建）
time ./scripts/cargo-rmcp-patched.sh build --offline --locked -p peri-tui --bin peri

# release 优化对照（不改仓库文件）
CARGO_TARGET_DIR=/tmp/peri-thin cargo --config /tmp/combined-config.toml \
  build --offline -p peri-tui --bin peri --release --timings
# combined-config.toml = rmcp patch 配置 + [profile.release] lto="thin", codegen-units=16
```

## 七、实施进展（Wave 1–3，2026-10-05）

在 worktree `.worktrees/build-perf`（分支 `perf/rust-build-speedup`）内由多个 subagent 并行推进；Wave 1–3 成果均已提交，并完成与 `pre-release/main`（b0d0b339，含 execution-ownership 移除）的集成合并。

### 7.1 Wave 1：配置层改动（`71036698` / `0f934166` / `3e6c7cc1`）

| 项 | 改动 | 实测 |
| --- | --- | --- |
| A1 | `[profile.release]` 改 `lto="thin"` + `codegen-units=16` | release clean **218→110s（-49.5%）**；LTO 单元 98.6→21.1s（-78.6%）；全单元 CPU -38.8%；二进制 20M→40M |
| A2 | 新增 `[profile.dev] debug=1` | dev clean **102→72s（-29.4%）**；dev target 6.0G→4.4G（-26.7%，其中 incremental -50%）；全单元 CPU -30.7% |
| A3 | `lefthook.yml` clippy 收窄到改动 crate（新增 `scripts/precommit-changed-crates.sh`，130 行；check 保持全量，全量 clippy 由 CI 兜底） | 脚本经 28 例模拟输入测试；真实提交计时留待端到端演练 |

- 增量健康：改 peri-agent 后重建 8.6s（原基线 9.6s），无退化；旧 target 首次增量会有一次 440 单元重编（profile 指纹变更，~69s，一次性）。
- 测量说明：各场景单次采样、主机非独占，绝对值有 ±5~10% 波动；CPU 总量指标独立于墙钟，与墙钟交叉验证结论一致。
- 与 CI 的交互：ci.yml 已设 `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0`（env 覆盖 profile），故 A2 只影响本地、不改变 CI 行为；A1 影响 pre-release/release 产物构建（更快、体积 40M）。

### 7.2 Wave 2：平台分配器与解耦（`6945efb3` / `954231ca` / `257128ff`，merge `9537e37c`）

| 项 | 提交 | 实测 / 结论 |
| --- | --- | --- |
| macOS 分配器 | 6945efb3 | jemalloc 改为平台门控（macOS 用系统分配器、Linux 保留 jemalloc）。同环境对照：分配路径 CPU **-92.7s（-70%）但墙钟仅 -1s**——分配器不在构建关键路径；macOS 上 `jemalloc-sys` 仍参与编译（build script 不可缓存） |
| TUI 死代码 | 954231ca | 删除 TUI 未使用的 MCP panel pool handle（P0 死代码清理，不放松依赖方向） |
| 外部依赖评估 | 257128ff | `aws-lc-sys`（ring 替代）不在构建关键路径：切换 TLS 栈无墙钟收益；结论**不采纳**，记录于 `spec/issues/2026-10-05-rust-build-performance-wave2-deps.md` |

### 7.3 Wave 3：端口化解耦与本地缓存（`e2a6b65a` / `1bf085b1` / `617d74c4`，merge `d72e1f32` + 集成合并）

| 项 | 提交 | 实测 / 结论 |
| --- | --- | --- |
| C-1 端口化（首批） | e2a6b65a | `McpPoolPort` +22 方法（集成后净 19）、`PluginManagerPort` +4；`downcast_arc` 10 处 + `downcast_ref` 3 处全部清零；peri-acp 生产引用 101→63 行（集成后 62）；14 文件 +642/−169；merge 前 peri-acp 725 + middlewares 1555 测试全绿。设计文档余项与集成状态见 `spec/issues/2026-10-05-middlewares-decoupling-design.md` §2.4.2 |
| 本地缓存指南 | 1bf085b1 | 受控矩阵 E11a–E11e（31 份证据归档于 `/tmp/peri-w3-e11-evidence`）：**Rust 缓存键含 target 目录路径**——跨 target 路径 0 命中（0/469）；同路径全命中下限 ~97s（proc-macro / build script / 链接不可缓存）；opt-in 指南 `docs/reference/dev-build-cache.md` + `scripts/cargo-sccache.sh` |
| 文档同步 | 617d74c4 | `environment-variables.md` / `peri-tui.md` 分配器平台口径修正 |

### 7.4 评估结论与设计产出

| 项 | 结论 | 产出文件 |
| --- | --- | --- |
| A5 CI sccache | **本轮不实施**（证据：test/bin 目标不可缓存、与 rust-cache 争同一 10GB 池且可能挤掉后者；补丁草案与 3 项前置审计已备） | `spec/issues/2026-10-05-ci-sccache-evaluation.md` |
| C1 拆 kit | **不建议纯 C1**（终端叶子切分净收益 ~1~2%；严格闭合子集仅 3,833 行 = kit 的 4.5%）；先治理 `atoms`（849 行 / 152 pub 项 / 135 入边），C1 降级为 C2/C3 之后的收口 | `spec/issues/2026-10-05-peri-tui-kit-extraction-design.md` |
| C2/C3/C4 解耦 | **C2 单独实施收益 ≈0**（middlewares 另经 `peri-acp` 硬依赖进入闭包，101 行）；端口件已具备（McpPoolPort 等），阻碍在装配面外移与 downcast 逃生口；排序 P0 删死代码 → P1 C3 → P2 C2 → P4 C4；与 M-TUI/L5 为同一任务线，应合并评审 | `spec/issues/2026-10-05-middlewares-decoupling-design.md` |

### 7.5 提交链与集成合并

提交链（自 `c6b6ce70` 起）：`71036698` → `0f934166` → `3e6c7cc1` →（Wave 2）`6945efb3` / `954231ca` / `257128ff` → merge `9537e37c` →（Wave 3）`e2a6b65a` / `1bf085b1` / `617d74c4` → merge `d72e1f32` → **集成合并 `pre-release/main`（b0d0b339）**。

交付形态：`perf/rust-build-speedup` 最终压缩为**单提交**（parent = merge-base `b0d0b339`，树内容与压缩前一致），便于并入 main；上表全部 hash 与 Wave 1–3 的 merge 结构归档于 tag `archive/perf-build-speedup-pre-squash`（压缩前 tip `9637d73b`，16 提交）。各 Wave 分支 `perf/w2-mw`（`e2a6b65a`）/ `perf/w2-jemalloc`（`6945efb3`）/ `perf/w2-deps`（`257128ff`）/ `perf/w3-cache`（`1bf085b1`）/ `perf/w3-profile`（`81222fb5`）ref 仍在。

集成合并要点：main 的 `9b5b257d` 移除了 execution-ownership 机制（`lease.rs` / `supervisor.rs` / `owner_catalog.rs` / `ExecutionOwnerToken`），与本分支的端口化在同一批调用点冲突；解决方向为**保留 main 的无 ownership 架构 + 重新施加端口路由**（`requests.rs` / `session_restore.rs` / `session_close.rs` 改经 `McpPoolPort`；端口删 4 个随 main 失效的方法、新增 `reconcile_closing_workspace_scope`）。

已知红灯（**非本次合并引入，且已在 main 修复**；两处，均由 `9c08e7fa` 修复）：

1. `peri-middlewares::mcp::client::subscription::tasks::task_projection_tests::test_unconfirmed_terminal_settlement_is_retryable`：`de85d87a` 将 `NoopTaskManager::settle_external` 由 `Ok` 改为 fail-closed 的 `Err("external terminal delivery is unavailable")`，而测试仍断言旧错误串（测试文件自 merge-base 未变）；已在 clean worktree（`b0d0b339`）实跑复现（1 failed）。
2. `peri-agent/tests/compact_pressure_adversarial_test.rs` 全部 10 个用例：`SessionResourceError::Conflict("session identity has different immutable creation facts")`——测试用未规范化的 `repo.path()` 创建会话，与资源层的 canonical cwd 冲突；同在 clean worktree（`b0d0b339`）实跑复现（10 failed）。

修复提交 `test(ci): align session and delivery fixtures with current contracts` 已落在 `pre-release/main`（`9c08e7fa`）上；主工作区合并本分支时测试文件自动取 main 版即恢复绿色。

### 7.6 数据修正与遗留

- workspace 成员数修正：**25**（根显式 24 + `peri-theme` 经 path 依赖隐式入组）；本文原「26 个」为旧计数（`peri-mcp-lsp` 已删除）。A3 脚本已按 25 个成员核对。
- 两份结构设计指出本文 §1.2 时间线存在口径差：peri-tui 起跑早于 peri-acp 结束属 cargo pipelining 的合理现象；链上加总与总时长差 7~11s 待复核。设计内收益推演均为线性外推、未实测，须按 §六 复测。
- 依赖剥离 debug info（`[profile.dev.package."*"] debug = false`；Wave 2 报告 §5.2「未采信、待干净复测」的正式复测，分支 `perf/w3-profile`，提交 `81222fb5`）：**语义**（`cargo build -v`，9 轮交叉验证）——依赖（含 proc-macro / build script）`debuginfo=0`、成员 24/24 = 1，test profile 自动继承（`cargo check --tests` 实测），无需额外配置。**确定性收益**（同配置多轮逐字节一致）：clean dev target 4.3G→3.8G（-11.6%，其中 `debug/deps` 2.8G→2.3G / -17.9%）、dev 二进制 224,382,184→215,253,800 B（-9.13MB / -4.07%）、编译单元数不变（642）。**CPU/墙钟无法分离**：外部并发构建反复出现，同配置跨轮极差 CPU base +28.7% / new +17.0%、墙钟两配置均约 +78%；3 组背靠背配对 CPU Δ -17.3%~+11.5% 方向不一致——不足以支持或否定 ≤4% 量级预期收益，待独占机器复测。CI 行为不变（env 全局 `CARGO_PROFILE_*_DEBUG=0` 已覆盖），本地 `cargo test` 依赖同步 `debug=0`、与 CI 更一致。调试影响：依赖栈 backtrace 无符号/行号（成员代码不受影响，可按需对单依赖 `debug = 1` 恢复）；回退 = 删除 3 行配置。完整轮次数据、语义探针与复现命令见 `spec/issues/2026-10-05-rust-build-performance-wave3-profile.md`。
- 待办：pre-commit 端到端计时、A4（aws-lc 屏蔽实验）、B 系列（依赖削减）、C 系列评审后的实施、Linux 真机验证与 macOS 分配器内存基准。

## 附：报告范围与限制

- 本文只覆盖本机（macOS/18 核）测量；Windows/Linux、CI runner、wasm 目标均未测。
- 时效类数据在并发会话负载下采集，绝对值偏保守；相对对比（如 §3.1 实验）使用同机同条件，结论可信。
- 未改动任何仓库文件（§3 实验均通过 `--config` 与独立 target 目录完成）；Cargo.toml 保持原状。
