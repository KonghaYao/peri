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
选择该分支运行；手动选择其他分支会跳过构建。同分支的新运行会取消旧构建。

| 平台 | Rust target | Runner |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `ubuntu-24.04` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
| macOS Intel | `x86_64-apple-darwin` | `macos-15-intel` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `macos-14` |

仅构建上述 macOS/Linux 目标，不构建 Windows、i386、LoongArch 或 RISC-V。
各目标在原生 runner 上构建 `peri-tui` 的 `peri` release 二进制，并运行
`peri --version` 冒烟检查。Linux 产物使用 GNU libc，不是静态 musl 二进制。

在 Actions 的 **Build Next Major** 运行页面下载 `peri-next-major-<platform>-<sha>`
artifact；每个 artifact 含 `peri-<platform>.tar.gz`（解压得到 `peri`）、
`checksums.txt` 和 `build-info.txt`，保留 14 天。构建使用仓库版本和锁文件，
不修改版本号、不创建 tag、不发布 GitHub Release；提交与运行身份写入构建信息。
构建成功不代替独立的 CI 检查；合并保护仍需在仓库设置中配置 required checks。

`release-agent.yml` 的 `agent-v*` tag 发布与两个异构平台的手动构建工作流
维持原有触发条件，不自动接入 `pre-release/main`。
