# 32 位 x86 Linux 静态构建

本文是操作参考，不是工程规则；验证证据要求以
[testing.md](../standards/testing.md) 为准。构建配置事实源为
[.cargo/config.toml](../../.cargo/config.toml) 与
[build-i386.sh](../../scripts/build-i386.sh)。

产物使用 `i686-unknown-linux-musl`：ELF32、Intel 80386 machine ID、静态 musl。
这里的 i386 指 32 位 x86 Linux 发行平台，CPU 基线仍是 Rust i686 目标，
不承诺原始 80386 CPU 可用。静态链接消除共享库/动态加载器依赖，
不打包 Git、Bash、Node.js 或插件/MCP 程序；相关能力仍需要对应外部命令。
Git 不是创建普通目录会话的前置条件：找不到 Git 可执行文件时，以当前目录建立
目录工作区；Git 可用时自动探测仓库/worktree。仓库权限、信任或损坏错误不会被
忽略。已有 Git 绑定缺少 Git 时拒绝执行；目录绑定后来被识别为仓库时也不会
自动改绑。身份规则见 [工作区身份设计](../design/session-workspace-identity.md)。

## 构建

先安装 Rust、Zig（验证过 0.15.2）与 cargo-zigbuild（0.23.4）：

```bash
cargo install cargo-zigbuild --version 0.23.4 --locked
rustup target add i686-unknown-linux-musl
./scripts/build-i386.sh
```

脚本从仓库根目录执行 `cargo zigbuild --locked --release`，仅构建 `peri-tui`
的 `peri` 二进制。目标配置显式启用 `+crt-static`，覆盖仓库通用配置的
`-crt-static`。脚本拒绝非空 `RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS`，
防止环境变量绕过目标配置。上游用法见
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)。

默认产物：`target/i686-unknown-linux-musl/release/peri`。
自定义 `CARGO_TARGET_DIR` 时，验证命令需显式传入对应产物路径。

## GitHub Actions 手动构建

独立工作流 [build-i386.yml](../../.github/workflows/build-i386.yml) 仅监听
`workflow_dispatch`，不接入正式版本的构建或发布流水线，也不创建 tag / Release。
工作流检出手动触发时选择的分支或 tag，安装固定版本的 Zig / cargo-zigbuild，
编译并打包后上传 `peri-linux-i386-<commit SHA>` artifact，保留 14 天。
工作流不运行容器验证；需要时可手动执行下述本地验证命令。
下载内容包含 `peri-linux-i386.tar.gz`、`checksums.txt` 与工具链版本/commit 信息。

按 [GitHub 手动运行文档](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)，
工作流需先进入仓库默认分支，才能从 Actions 页面手动触发：选择
**Build i386 Static Binary → Run workflow → 目标分支**。
也可以使用 GitHub CLI：

```bash
gh workflow run build-i386.yml --ref <branch-or-tag>
```

## 验证

需要 Python 3、Docker 与 `linux/386` 执行支持。在 Apple Silicon 上使用
Docker 的 x86 模拟；模拟结果不等于物理旧 CPU 兼容性验证。

```bash
docker build --platform linux/386 -f scripts/i386-smoke.Dockerfile \
  -t peri-i386-smoke:local scripts
python3 scripts/test-i386.py
# 不安装 Git 的精简环境也必须通过同一套会话生命周期测试：
python3 scripts/test-i386.py --image alpine:3.22
# 或指定产物 / 测试镜像：
python3 scripts/test-i386.py /absolute/path/to/peri --image peri-i386-smoke:local
```

Git 模式的镜像构建需要网络安装 Git/Bash；普通 Alpine 镜像验证 Git 缺失模式。
实际测试使用 `--network none`，临时 HOME、
配置和 SQLite 数据库，不读取开发者配置或真实凭据。验证包括：

- ELF32 / Intel 80386 / executable，存在 LOAD 且没有 INTERP 或 DYNAMIC segment。
- `--version`、`--help` 成功，以及非法参数返回 exit 2。
- ACP `initialize`、`session/new`、未知方法错误、SQLite 创建与 stdin EOF 后正常退出。
- ACP 退出后启动新的 `meta session --json` 进程，验证同一会话的 ID 与 cwd 已持久化。

任一断言失败或子进程超时均返回非零状态，且清理测试容器。此 smoke 验证
覆盖静态产物启动和基本 ACP 生命周期，不覆盖交互式 TUI、真实模型网络调用、
外部插件或全部 workspace 测试。

## 打包

验证成功后可生成部署归档；二进制不加入 Git：

```bash
mkdir -p target/dist
tar -czf target/dist/peri-linux-i386.tar.gz \
  -C target/i686-unknown-linux-musl/release peri
shasum -a 256 target/dist/peri-linux-i386.tar.gz
```
