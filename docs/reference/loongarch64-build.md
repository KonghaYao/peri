# 64 位 LoongArch Linux 静态构建

本文是操作参考，不是工程规则；验证证据要求以
[testing.md](../standards/testing.md) 为准。构建配置事实源为
[.cargo/config.toml](../../.cargo/config.toml) 与
[build-loongarch64.sh](../../scripts/build-loongarch64.sh)。

产物使用 `loongarch64-unknown-linux-musl`：ELF64、LoongArch machine ID、LP64D
（双精度浮点）ABI、静态 musl。静态链接消除共享库与动态加载器依赖，不打包 Git、
Bash、Node.js 或插件/MCP 程序；相关能力仍需要对应外部命令。这里的 loongarch64
指 64 位 LoongArch Linux 发行平台与 LP64D ABI，不承诺 LP64S（软浮点）ABI 的
机器可用。Git 与工作区身份的行为与 i686 发行版一致，见
[32 位 x86 Linux 静态构建](i386-static-build.md)。

## 构建

先安装 Rust、Zig（验证过 0.15.2）与 cargo-zigbuild（0.23.4）：

```bash
cargo install cargo-zigbuild --version 0.23.4 --locked
rustup target add loongarch64-unknown-linux-musl
./scripts/build-loongarch64.sh
```

脚本从仓库根目录执行 `cargo zigbuild --locked --release`，仅构建 `peri-tui`
的 `peri` 二进制。目标配置显式启用 `+crt-static`，覆盖仓库通用配置的
`-crt-static`。脚本拒绝非空 `RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS`，
防止环境变量绕过目标配置。上游用法见
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)。

默认产物：`target/loongarch64-unknown-linux-musl/release/peri`。
自定义 `CARGO_TARGET_DIR` 时，验证命令需显式传入对应产物路径。

## GitHub Actions 手动构建

独立工作流 [build-loongarch64.yml](../../.github/workflows/build-loongarch64.yml)
仅监听 `workflow_dispatch`，不接入正式版本的构建或发布流水线，也不创建 tag /
Release。工作流检出手动触发时选择的分支或 tag，安装固定版本的 Zig /
cargo-zigbuild，编译并打包后上传 `peri-linux-loongarch64-<commit SHA>` artifact，
保留 14 天。工作流不运行模拟器验证；需要时可手动执行下述本地验证命令。
下载内容包含 `peri-linux-loongarch64.tar.gz`、`checksums.txt` 与工具链版本/commit 信息。

按 [GitHub 手动运行文档](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)，
工作流需先进入仓库默认分支，才能从 Actions 页面手动触发：选择
**Build LoongArch64 Static Binary → Run workflow → 目标分支**。
也可以使用 GitHub CLI：

```bash
gh workflow run build-loongarch64.yml --ref <branch-or-tag>
```

## 验证

需要 Python 3 与 LoongArch 用户态模拟（`qemu-loongarch64-static`）。产物静态链接，
没有解释器或共享库需要解析，因此用户态模拟器本身即可运行，不需要 Docker 或
目标机 sysroot：

```bash
python3 scripts/test-loongarch64.py
# 或指定产物 / 模拟器：
python3 scripts/test-loongarch64.py /absolute/path/to/peri --qemu qemu-loongarch64-static
```

测试使用临时 HOME、配置与 SQLite 数据库，不读取开发者配置或真实凭据。验证包括：

- ELF64 / LoongArch / ET_EXEC，存在 LOAD 且没有 INTERP 或 DYNAMIC segment。
- `--version`、`--help` 成功，以及非法参数返回 exit 2。
- ACP `initialize`、`session/new`、未知方法错误、SQLite 创建与 stdin EOF 后正常退出。
- ACP 退出后启动新的 `meta session --json` 进程，验证同一会话的 ID 与 cwd 已持久化。

任一断言失败或子进程超时均返回非零状态。此 smoke 验证覆盖静态产物启动和基本
ACP 生命周期，不覆盖交互式 TUI、真实模型网络调用、外部插件或全部 workspace 测试。
模拟结果不等于物理 LoongArch 机器上的兼容性验证。

用户态模拟器只翻译被测二进制自身：它派生的子进程必须是同架构的二进制才有意义，
因此本流程不覆盖 Git 仓库发现路径（宿主 `git` 是 x86_64，无法在模拟器内执行）。
测试运行在普通目录上，实际走的是「找不到可用 Git 时以当前目录建立目录工作区」
分支；要覆盖 Git 绑定的仓库/worktree 场景，需要在完整 LoongArch 用户态
（chroot 或原生机器）中运行。

## 打包

验证成功后可生成部署归档；二进制不加入 Git：

```bash
mkdir -p target/dist
tar -czf target/dist/peri-linux-loongarch64.tar.gz \
  -C target/loongarch64-unknown-linux-musl/release peri
shasum -a 256 target/dist/peri-linux-loongarch64.tar.gz
```
