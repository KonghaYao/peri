# 本机编译缓存（sccache）使用说明

本页记录在本机复用 Rust/C 编译产物的 opt-in 方案：用法、命中条件、能力边界与回退。
它不构成仓库规则；权威构建入口仍是 `scripts/cargo-rmcp-patched.sh`，缓存能力边界以
sccache 源码为准。

- 结论摘要：
  - **清空并重建同一 target 目录**时，Rust 依赖与 C/汇编依赖可全量命中，墙钟约
    -20%；具体数值随机器负载变化，不在本文固定记录。
  - **换用新的 target 目录路径**（例如新 worktree 指向新 target）时，Rust 依赖 0 命中，
    只有 C/汇编依赖跨路径命中；收益主要体现在机器总工作量而非墙钟。
  - 首次预热（缓存为空）不省时间；成本由第一次构建支付，之后按上述条件复用。
- 是否采用：**opt-in**。不安装 sccache 的开发者与 CI 完全不受影响；本方案不修改
  `.cargo/config.toml`、不设置全局 `rustc-wrapper`，也不改动 CI workflow。

## 命中条件（决定收益场景）

- **Rust 依赖**：缓存键包含 **target 目录路径**——同一路径清空重建可全量命中，换
  target 路径则 0 命中。源码树位置不敏感：registry 依赖可跨 worktree 命中；仓库内
  patch crate 的源码展开在各自 target 目录下，跨树时不命中。
- **C / 汇编依赖**：与路径无关，跨 worktree、跨 target 稳定命中。
- **不可缓存**：proc-macro、build script、bin/test 等需要链接的目标与最终链接
  （见「能力边界」）。

## 适用场景

| 场景 | 是否受益 |
| --- | --- |
| 清空同一 target 路径后重建（`rm -rf target` 后全量构建） | 受益最大：Rust + C/汇编全命中（约 -20% 墙钟） |
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

## 收益特征（实测结论）

测量条件：macOS（Apple Silicon），dev profile，每组测量使用独立空 target 目录并共享
同一 `SCCACHE_DIR`。数值受机器并发负载影响，这里只保留可复现的结论：

- **同一 target 路径清空重建**：Rust 依赖全量命中，C/汇编依赖同样全命中；墙钟收益
  约 -20%。
- **换新 target 路径**：Rust 依赖 0 命中，C/汇编依赖全命中；墙钟变化落在并发噪声
  区间内。
- **换源码树但 target 路径不变**：绝大多数 Rust 单元命中，miss 集中在源码路径不同的
  仓库内 patch crate。
- **touch 源文件后的增量重建**：无额外收益，也未见 sccache 拖慢。

## 能力边界（为什么不能更快）

sccache 以 `rustc` 为包装单位，以下编译单元**不可缓存**：

- **cargo incremental 单元**（`reasons: incremental`）：dev profile 下 workspace 成员
  的编译默认带 `-C incremental`，sccache 直接 bypass。依赖 crate（registry / git /
  path 补丁）不带 incremental，因此可缓存。
- **crate-type 单元**：`bin`、`tests`、`proc-macro`、`cdylib` 等需要链接的目标，包括
  全部 proc-macro 与 build script 二进制。
- 最终链接步骤（`--bin peri`）。

原因分布可用 `sccache --show-adv-stats` 的 `Non-cacheable reasons` 查看。

即便全量命中，墙钟仍包含每次编译请求的命中判定与产物处理，以及上述不可缓存单元与
最终链接构成的下限。

## 与 cargo incremental 的交互

- 二者**不冲突，也不叠加**：`RUSTC_WRAPPER=sccache` 下 incremental 单元被 sccache
  跳过、由 rustc 正常处理；同一 target 目录内的增量重建不受影响，未见变慢。
- 一次性冷构建可临时 `CARGO_INCREMENTAL=0`，把 workspace 成员也纳入缓存；代价是同一
  worktree 内失去 incremental 加速。该组合未做受控复核，建议：**不要全局关闭
  incremental**。
- CI 场景（`Swatinem/rust-cache@v2` 会隐式设 `CARGO_INCREMENTAL=0`）与本文档无关。

## 与 build 脚本 / CI 的关系

- `scripts/cargo-rmcp-patched.sh`：脚本只追加 `--config <patch-config>` 后 `exec cargo`，
  不覆盖 `RUSTC_WRAPPER`，两者兼容；patch 进依赖图的 rmcp / hyper-util 同样走 sccache。
- `clippy` 子命令：兼容；编译请求计入 sccache 统计。
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
- 缓存体积随依赖规模增长。

## 回退

- 不调用 `scripts/cargo-sccache.sh` 即完全回到原状（脚本不写任何全局配置）。
- 未安装 sccache 时脚本会给出安装提示并以非零状态退出。
- 已设置环境变量的 shell：`unset RUSTC_WRAPPER`（或 `unset SCCACHE_DIR`）。
- 需要彻底移除：`sccache --stop-server && brew uninstall sccache`（或
  `cargo uninstall sccache`），随后可删除缓存目录。

## 未验证项

- `--cross` / 交叉编译腿与 sccache 的组合（本机未运行）。
- Windows / Linux 主机上的等价收益（本页数据仅 macOS）。
- `CARGO_INCREMENTAL=0` + 热缓存的组合（未做受控复核）。
- 长时间运行后的缓存命中稳定性（LRU 驱逐、并发写入时序）与 CI 场景的交互。
- sccache 与 `cargo test` 全量运行的耗时对比（未做）。
