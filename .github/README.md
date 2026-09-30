# CI 与预发布构建

`pre-release/main` 是下一个大版本的开发分支；`main` 保留现有开发与发布流程。
本文件说明工作流入口，实际触发条件与构建矩阵以 `.github/workflows/` 为准；
验证范围遵循 [测试规范](../docs/standards/testing.md)。

## CI

`workflows/ci.yml` 验证 `main`、`pre-release/main` 的 push，以及以这两个分支为
目标的 pull request。两个分支复用相同的分层检查、workspace 构建、测试和 Clippy。
`main` 保留 Linux、macOS、Windows 验证；`pre-release/main` 仅验证 Linux 和 macOS。

## 下一个大版本的构建产物

`workflows/pre-release.yml` 在 `pre-release/main` 每次 push 时构建，也支持手动
选择该分支运行；手动选择其他分支会跳过构建。同分支构建与发布串行运行，
不取消正在发布的运行，以免留下部分更新的 Release。

| 平台 | Rust target | Runner |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `ubuntu-24.04` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
| macOS Intel | `x86_64-apple-darwin` | `macos-15-intel` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `macos-14` |

仅构建上述 macOS/Linux 目标，不构建 Windows、i386、LoongArch 或 RISC-V。
各目标在原生 runner 上构建 `peri-tui` 的 `peri` release 二进制，将
产物重命名为 `peri-beta`，并运行 `peri-beta --version` 冒烟检查。
Cargo 的构建目标与 CLI 内部名称仍为 `peri`；正式版二进制不改名。
Linux 产物使用 GNU libc，不是静态 musl 二进制。

在 Actions 的 **Build Next Major** 运行页面下载 `peri-beta-<platform>-<sha>`
artifact；每个 artifact 含 `peri-beta-<platform>.tar.gz`（解压得到 `peri-beta`）、
对应的 `.sha256` 和 `build-info-<platform>.txt`，保留 14 天。
构建使用仓库版本和锁文件，不修改版本号；提交与运行身份写入构建信息。
四个平台均成功后，更新独立的 `peri-beta` prerelease（不标记为 Latest），
其 tag 指向本次构建提交。该渠道滚动替换资产，不保留每次构建的永久 Release。
资产更新并非原子操作；发布中安装可能校验失败，此时重新运行安装脚本。
此滚动渠道要求仓库允许修改 Release 资产和 tag；若启用 immutable releases，
需要先调整发布渠道设计，不能直接复用该滚动更新流程。
构建成功不代替独立的 CI 检查；合并保护仍需在仓库设置中配置 required checks。

`release-agent.yml` 的 `agent-v*` tag 发布与两个异构平台的手动构建工作流
维持原有触发条件，不自动接入 `pre-release/main`。

## 安装与更新 Beta

```bash
curl -fsSL https://raw.githubusercontent.com/konghayao/peri/pre-release/main/scripts/install-beta.sh | bash
peri-beta --version
```

`scripts/install-beta.sh` 仅支持上述四个平台，从独立的 `peri-beta` Release
下载并校验 SHA-256，安装前执行版本检查，默认原子替换 `~/.local/bin/peri-beta`，
不修改 `peri`、正式版安装目录或 shell 配置。首次使用须将安装目录加入 PATH。
可设置 `PERI_BETA_INSTALL_DIR` 自定义目录、`PERI_BETA_INSTALL_PLATFORM` 指定平台；
通过管道运行时变量应传给 `bash`，而非只传给 `curl`。

更新 Beta 请重新运行安装脚本，不使用正式版的 `peri-beta update` 更新链路。
二进制名称隔离不等于运行数据隔离：默认配置与会话仍使用 `~/.peri`；需要隔离时
使用 `--config-file` 和 `--db-path`。现有 `install.sh`、`install.ps1` 不变，
它们选择的 `agent-*` Release 不包括 `peri-beta`。

安装契约的离线验证：`python3 scripts/test-install-beta.py`。
