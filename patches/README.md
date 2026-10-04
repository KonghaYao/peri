# 固定第三方补丁

仓库使用 `rmcp 3.5.0`，并通过 [补丁](rmcp-3.5.0-task-subscriptions.patch)补齐 Tasks 扩展的 `taskIds` 订阅和 `notifications/tasks`。补丁不依赖 GitHub fork，也不提交第三方源码。

从新 checkout 构建时，用仓库脚本代替直接调用 Cargo：

```bash
./scripts/cargo-rmcp-patched.sh check --locked --workspace
./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib
```

脚本下载 crates.io 的固定发布包，核验 SHA-256，用 `patch`（缺少时以独立临时 Git 仓库执行 `git apply`）应用补丁并反向校验缓存，再以 Cargo `[patch.crates-io]` 配置运行所给命令。生成的源码位于 gitignored 的 `target/peri-*-patches/`，按补丁哈希隔离；第二次运行复用缓存。`Cargo.lock` 记录本地 patched crate，因此直接运行 `cargo --locked` 不会偷偷回退到未打补丁的 registry 版本。CI、pre-release、release、Lefthook 和 `./dev.sh` 的编译命令也使用该脚本；Windows CI 用 Git Bash 运行脚本，脚本将 crate 和 Cargo 配置路径统一转换给原生 `cargo.exe`。脚本保留调用者的工作目录，让 `./dev.sh --cwd=...` 仍以指定目录启动 TUI。Linux 的 cross 构建使用 `./scripts/cargo-rmcp-patched.sh --cross build …`。更新补丁后，使用脚本执行 `update -p <crate>` 并提交更新后的 lockfile。

Emscripten 目标固定 Cloudflare 的 Mio 与 Tokio 分支。`reqwest 0.13.4` 与 `turso_serverless 0.1.3` 使用官方发布版，经 native/Hyper HTTP 后端保持远程 MCP、模型和存储的 `Send` 契约。WASM 构建用 `scripts/cargo-wasm.sh` 加上 linker 和 Tokio 事件循环参数；用法与已验证范围见 [`peri-wasm/README.md`](../peri-wasm/README.md)。

[`hyper-util 0.1.21`](hyper-util-0.1.21-emscripten-dns.patch) 的默认 GAI resolver 原本调用 `spawn_blocking`，Emscripten 无法启动 worker thread。目标专用补丁把域名查找交给 Cloudflare Tokio 的异步 DNS，原生目标仍用原解析器。脚本按固定发布包 SHA-256 获取并校验该补丁；`Cargo.lock` 锁定 patched crate。

WASM 构建还使用 [`scripts/prepare-emscripten.sh`](../scripts/prepare-emscripten.sh) 对 Emscripten 6.0.10 前端幂等应用 Cloudflare `worker-build` 的 [epoll listener](emscripten/epoll-listeners.patch) 和 [异步 DNS](emscripten/noderawsockets-dns.patch) 补丁，来源固定在 [workers-rs b57ba6e](https://github.com/cloudflare/workers-rs/tree/b57ba6ef8198c65499c2f92b1845cc2412dd6e8c/worker-build/patches/emscripten)。它们分别补足 Tokio 事件循环及 `tokio::net` 链接需要的四个导出；网络连接还需要 `-sNODERAWSOCKETS`。额外的 [Bun TCP 补丁](emscripten/noderawsockets-bun.patch)只在 Bun 宿主跳过 Emscripten 对未实现 `process.binding('tcp_wrap')` 的调用，Node 仍保留原来的同步预绑定行为。[Workers 模块 URL 补丁](emscripten/workers-module-url.patch)在 `import.meta.url` 缺失时使用 Emscripten 已有的 `Module.mainScriptUrlOrBlob` 选项；Workers 入口提供该值。更换 Emscripten 版本时须重新核对补丁并更新固定版本。脚本修改本机安装的 Emscripten 前端源码，补丁文件作为可复现的事实源。

Wrangler 本地 `workerd` 不提供模块 `import.meta.url`，也不允许字符串 `eval`。构建使用 `-sDYNAMIC_EXECUTION=0`，Peri 在 Emscripten 目标用 UTC 格式化日期，避免时区查询触发脚本求值。生成的 JS glue 不再后处理。

Workspace 的 Bash 任务通过 `tasks/get`、`tasks/cancel` 和 `subscriptions/listen` 发布状态；Peri 以 Tasks-capable MCP client 接收任务回执和完成通知。完整链路由 Workspace 线路测试与 middleware 桥接测试验证。
