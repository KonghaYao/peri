# rmcp Tasks 订阅补丁

仓库使用 `rmcp 3.5.0`，并通过 [补丁](rmcp-3.5.0-task-subscriptions.patch)补齐 Tasks 扩展的 `taskIds` 订阅和 `notifications/tasks`。补丁不依赖 GitHub fork，也不提交第三方源码。

从新 checkout 构建时，用仓库脚本代替直接调用 Cargo：

```bash
./scripts/cargo-rmcp-patched.sh check --locked --workspace
./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib
```

脚本下载 crates.io 的 `rmcp 3.5.0` 发布包，核验固定 SHA-256，用 `patch`（缺少时以独立临时 Git 仓库执行 `git apply`）应用补丁并反向校验缓存，再以 Cargo `[patch.crates-io]` 配置运行所给命令。生成的源码位于 gitignored 的 `target/peri-rmcp-patches/`，按补丁哈希隔离；第二次运行复用缓存。`Cargo.lock` 记录本地 patched crate，因此直接运行 `cargo --locked` 不会偷偷回退到未打补丁的 registry 版本。CI、pre-release、release、Lefthook 和 `./dev.sh` 的编译命令都使用该脚本；脚本保留调用者的工作目录，让 `./dev.sh --cwd=...` 仍以指定目录启动 TUI。Linux 的 cross 构建使用 `./scripts/cargo-rmcp-patched.sh --cross build …`。更新补丁后，使用脚本执行 `update -p rmcp` 并提交更新后的 lockfile。

Workspace 的 Bash 任务通过 `tasks/get`、`tasks/cancel` 和 `subscriptions/listen` 发布状态；Peri 以 Tasks-capable MCP client 接收任务回执和完成通知。完整链路由 Workspace 线路测试与 middleware 桥接测试验证。
