# 本机编译缓存（sccache）使用说明

本页记录在本机复用 Rust/C 编译产物的 opt-in 方案：用法、受控实测数据、命中条件、
能力边界与回退。它不构成仓库规则；权威构建入口仍是 `scripts/cargo-rmcp-patched.sh`，
缓存能力边界以 sccache 源码为准。

- 结论摘要（2026-10-05 同窗口受控复核）：
  - **清空并重建同一 target 目录**（含从其他 worktree 预热缓存）时，Rust 依赖可全量
    命中：墙钟 120s → 97s（约 -19%），C/汇编依赖同样全命中。
  - **换用新的 target 目录路径**（例如新 worktree 指向新 target）时，Rust 依赖 0 命中，
    只有 C/汇编依赖跨路径命中；墙钟 108–116s（-3% ~ -10%，落在并发噪声区间内），
    收益主要体现在机器总工作量而非墙钟。
  - 首次预热（缓存为空）不省时间；成本由第一次构建支付，之后按上述条件复用。
- 是否采用：**opt-in**。不安装 sccache 的开发者与 CI 完全不受影响；本方案不修改
  `.cargo/config.toml`、不设置全局 `rustc-wrapper`，也不改动 CI workflow。

## 命中条件（决定收益场景）

- **Rust 依赖**：缓存键包含 **target 目录路径**——同一路径清空重建可全量命中，换
  target 路径则 0 命中。源码树位置不敏感：registry 依赖可跨 worktree 命中；仓库内
  patch crate 的源码展开在各自 target 目录下，跨树时不命中。
- **C / 汇编依赖**：与路径无关，跨 worktree、跨 target 稳定命中（本轮 100%）。
- **不可缓存**：proc-macro、build script、bin/test 等需要链接的目标与最终链接
  （见「能力边界」）。

## 适用场景

| 场景 | 是否受益 |
| --- | --- |
| 清空同一 target 路径后重建（`rm -rf target` 后全量构建） | 受益最大：Rust + C/汇编全命中（本轮 120s → 97s） |
| 从其他 worktree 预热缓存、复用于**相同** target 路径 | 同上（Rust 命中与源码树位置无关） |
| 新 target 目录路径（新 worktree、更换 `CARGO_TARGET_DIR`） | 仅 C/汇编依赖命中，Rust 0 命中 |
| 同一 target 目录内的日常增量迭代（改代码 → `cargo build`） | 无额外收益；cargo incremental 仍然生效（见下） |
| release / LTO 构建 | 收益有限，最终 bin 的 LTO+链接不可缓存 |
| `--cross` 交叉编译（容器内编译） | 不适用，容器内没有 sccache |

## 快速开始

```bash
# 安装（任选其一）
brew install sccache          # macOS
cargo install sccache --locked

# 用法一：包装脚本（推荐；不改变任何全局配置）
./scripts/cargo-sccache.sh build --locked -p peri-tui --bin peri
./scripts/cargo-sccache.sh test --locked -p peri-agent

# 用法二：手工设置环境变量
RUSTC_WRAPPER=sccache ./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
```

`sccache` 服务端会常驻后台；查看命中率与容量：

```bash
sccache --show-stats        # 命中率、容量、上限
sccache --show-adv-stats    # 追加 Non-cacheable reasons 分布
```

## 实测数据

测量机器：macOS（Apple Silicon 18 核 / 48GB），rustc+cargo 1.99.0，sccache 0.15.0；
命令均为 `./scripts/cargo-rmcp-patched.sh build -p peri-tui --bin peri`（dev profile），
每行使用独立空 target 目录（`/tmp` 下），共享同一 `SCCACHE_DIR`。E11 系列为
2026-10-05 15:18–15:29 同一窗口的受控复核；窗口内同机有其他会话并发构建
（E11a/E11b 以 5s 间隔采样 load 探针，其余行未采样）。

| 实验 | 条件 | 墙钟 | Rust 命中 | C/汇编命中 | 探针 load |
| --- | --- | --- | --- | --- | --- |
| E11a | 无 sccache，全新 target | 120s | — | — | 7.8–10.5 |
| E11b | 热缓存，新 target 路径 | 116s | 0 / 469 | 375 / 376 | 8.3–11.6 |
| E11b′ | 热缓存，另一新 target 路径 | 108s | 0 / 469 | 375 / 376 | 未采样 |
| E11c | 热缓存，**原 target 路径**清空重建 | **97s** | 469 / 469 | 376 / 376 | 未采样 |
| E11d | 热缓存，另一源码树 + 原 target 路径 | 116s | 465 / 469 | 376 / 376 | 未采样 |
| E11e | touch `peri-tui/src/main.rs` 后增量重建 | 3s / 2s | — | — | — |

- E11a 另有一次重载样本（三路并发构建、load 峰值 16.7）222s，不纳入对比。
- E11b 与 E11c 只差 target 路径：换路径 Rust 0 命中、同路径清空重建 100% 命中，
  即 **Rust 缓存键包含 target 目录路径**。
- E11d 把源码树换成 `/tmp/peri-w3-copy`（同一提交）：465 / 469 命中，4 个 miss 为
  两树中源码路径不同的单元（仓库内 patch crate 展开在各自 target 下）。
- E11e 为增量对照：2.69s（无 sccache）vs 1.61s（sccache）；同窗口、含 cargo 包缓存
  锁等待噪声，未见 sccache 拖慢增量重建。

对早期数据的修正：本页早期版本曾记录「另一 worktree 预热 + 全新 target →
Rust 469/469、113.5s → 86.1s」的探索性数据；受控复核未能复现该场景（换 target 路径
即 0 命中），结论以上表为准。

## 能力边界（为什么不能更快）

sccache 以 `rustc` 为包装单位，以下编译单元**不可缓存**：

- **cargo incremental 单元**（`reasons: incremental`，每轮约 26 个）：dev profile 下
  workspace 成员的编译默认带 `-C incremental`，sccache 直接 bypass。依赖 crate
  （registry / git / path 补丁）不带 incremental，因此可缓存。
- **crate-type 单元**（每轮约 94 个）：`bin`、`tests`、`proc-macro`、`cdylib` 等需要
  链接的目标，包括全部 proc-macro 与 build script 二进制。
- 最终链接步骤（`--bin peri`）。

原因分布可用 `sccache --show-adv-stats` 的 `Non-cacheable reasons` 查看（本轮四轮
构建累计：crate-type 377、incremental 104、其他 49）。

即便全量命中（E11c），本轮墙钟仍在 97s（vs 基线 120s）：sccache 仍须对每次编译请求
做命中判定与产物处理，且上述不可缓存单元与最终链接构成构建下限。

## 与 cargo incremental 的交互

- 二者**不冲突，也不叠加**：`RUSTC_WRAPPER=sccache` 下 incremental 单元被 sccache
  跳过、由 rustc 正常处理；同一 target 目录内的增量重建不受影响（E11e：touch 后
  重建 2.69s → 1.61s，含锁竞争噪声，未见变慢）。
- 一次性冷构建可临时 `CARGO_INCREMENTAL=0`，把 workspace 成员（每轮约 26 个）也纳入
  缓存；代价是同一 worktree 内失去 incremental 加速。本轮未复测该组合。
  建议：**不要全局关闭 incremental**。
- CI 场景（`Swatinem/rust-cache@v2` 会隐式设 `CARGO_INCREMENTAL=0`）与本文档无关，见
  `spec/issues/2026-10-05-ci-sccache-evaluation.md`。

## 与 build 脚本 / CI 的关系

- `scripts/cargo-rmcp-patched.sh`：脚本只追加 `--config <patch-config>` 后 `exec cargo`，
  不覆盖 `RUSTC_WRAPPER`，两者兼容；patch 进依赖图的 rmcp / hyper-util 同样走 sccache。
- `clippy` 子命令：兼容（复核：`./scripts/cargo-sccache.sh clippy -p peri-time -- -W clippy::all`
  通过，编译请求计入 sccache 统计）。
- `--cross`：容器内没有 sccache，`scripts/cargo-sccache.sh` 会移除 wrapper 后再调用。
- CI：本方案不在 workflow 中启用；CI 是否引入 sccache 由上面的评估报告另行决策。

## 缓存管理与清理

```bash
sccache --show-stats          # 命中率 / 容量
sccache --zero-stats          # 仅清零统计
sccache --stop-server         # 停止后台服务（不影响磁盘缓存）
rm -rf "$(sccache --show-stats | awk -F'"' '/Local disk/{print $2}')"   # 清空磁盘缓存
```

- 缓存目录默认由 sccache 决定；可用 `SCCACHE_DIR` 指定（多 worktree 会共享同一目录）。
- 容量上限默认 10GiB，可用 `SCCACHE_CACHE_SIZE`（如 `20G`）调整。
- `SCCACHE_BASEDIRS` 只影响 C/C++ 预处理输出的路径归一化，对 Rust 命中没有帮助。
- 体积：一轮完整 dev 构建写入约 350MB（复核：两轮 Rust 写入后缓存由 1.8GB 增至
  2.5GB，sccache 自报「3 GiB」口径；上限默认 10GiB）。

## 回退

- 不调用 `scripts/cargo-sccache.sh` 即完全回到原状（脚本不写任何全局配置）。
- 未安装 sccache 时脚本会给出安装提示并以非零状态退出（复核通过）。
- 已设置环境变量的 shell：`unset RUSTC_WRAPPER`（或 `unset SCCACHE_DIR`）。
- 需要彻底移除：`sccache --stop-server && brew uninstall sccache`（或
  `cargo uninstall sccache`），随后可删除缓存目录。

## 未验证项

- `--cross` / 交叉编译腿与 sccache 的组合（本机未运行）。
- Windows / Linux 主机上的等价收益（本页数据仅 macOS）。
- `CARGO_INCREMENTAL=0` + 热缓存的组合（早期探索数据未在受控条件下复现）。
- 长时间运行后的缓存命中稳定性（LRU 驱逐、并发写入时序）与 CI 场景的交互。
- sccache 与 `cargo test` 全量运行的耗时对比（本轮只做了小规模兼容性验证）。
