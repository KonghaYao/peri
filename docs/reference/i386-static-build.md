# 32 位 x86 Linux 静态构建

本文是操作参考，不是工程规则；验证证据要求以
[testing.md](../standards/testing.md) 为准。构建配置事实源为
[.cargo/config.toml](../../.cargo/config.toml) 与
[build-i386.sh](../../scripts/build-i386.sh)。

产物使用 `i686-unknown-linux-musl`：ELF32、Intel 80386 machine ID、静态 musl。
这里的 i386 指 32 位 x86 Linux 发行平台，CPU 基线仍是 Rust i686 目标，
不承诺原始 80386 CPU 可用。静态链接消除共享库/动态加载器依赖，
不打包 Git、Bash、Node.js 或插件/MCP 程序；相关能力仍需要对应外部命令。

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

## 验证

需要 Python 3、Docker 与 `linux/386` 执行支持。在 Apple Silicon 上使用
Docker 的 x86 模拟；模拟结果不等于物理旧 CPU 兼容性验证。

```bash
docker build --platform linux/386 -f scripts/i386-smoke.Dockerfile \
  -t peri-i386-smoke:local scripts
python3 scripts/test-i386.py
# 或指定产物 / 测试镜像：
python3 scripts/test-i386.py /absolute/path/to/peri --image peri-i386-smoke:local
```

镜像构建需要网络安装 Git/Bash；实际测试使用 `--network none`，临时 HOME、
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
