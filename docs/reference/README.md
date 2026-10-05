# 参考资料索引

本目录保存有长期使用价值、但不构成仓库设计或工程规则的资料。发生冲突时，服从
代码、`docs/standards/` 与 `docs/design/`。

- [mcp-ecosystem.md](mcp-ecosystem.md)：MCP 生态背景与外部互通参考。
- [artifact-remote-storage.md](artifact-remote-storage.md)：artifact 上传的远程存储配置与对接契约（使用者视角）。
- [langfuse-data-integrity.md](langfuse-data-integrity.md)：Langfuse 数据检查手册。
- [tui-manual-verification.md](tui-manual-verification.md)：可重复执行的 TUI 手工验证清单。
- [i386-static-build.md](i386-static-build.md)：cargo-zigbuild 32 位 x86 Linux 静态构建与容器验证。
- [loongarch64-build.md](loongarch64-build.md)：cargo-zigbuild 64 位 LoongArch Linux 静态构建与模拟器验证。
- [dev-build-cache.md](dev-build-cache.md)：本机 sccache 编译缓存（opt-in）的实测收益、能力边界、用法与回退。
- [oxidizer-fetch-upstream-research.md](oxidizer-fetch-upstream-research.md)：Oxidizer `fetch` 的 HTTP 抽象与 Wasm 适配约束，以及 Peri 请求边界评估。
- [time-platform-research.md](time-platform-research.md)：时间库的 Emscripten 支持范围与计时器行为；权威目标见[时间能力边界](../design/time-runtime.md)。

参考资料不记录某次执行的勾选状态、临时日志或 active issue 进度。
