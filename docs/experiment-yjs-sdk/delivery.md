# 交付检查

状态：实现、实验、独立对抗审查与提交前检查已完成。事实源为代码、测试与 SDK README；本文记录本次执行结果。

## 已完成验证

| 检查 | 结果 |
| --- | --- |
| `bun run build` | 通过，包含 Rust WASM release、SDK/view bundle 和声明文件 |
| `bun run typecheck` | 通过，包含 SDK 与 demo |
| 重点契约测试 | 51 passed / 0 failed（原 49 项加 2 项关闭时序回归）；包含构建后的公开入口、真实 HTTP SSE |
| `bun run test` | 137 passed / 1 failed（最终构建后重跑全部 138 项）；失败为 WASM 远端存储启动，见下文 |
| 无头 Chrome + 12k 工具真实 demo | 通过；初始 200 内容块、0 次全文读取；展开 1 次读取；流式更新保留 DOM 与展开状态；加载历史后 400 内容块；128 KiB 长文本显示尾部，下载 Blob 保留 131,072 bytes |
| benchmark 脚本 TypeScript 检查 | 通过（含独立 browser/codec 脚本） |
| `lefthook run pre-commit` | 通过 typos 与 22 条依赖规则；没有暂存 Rust 文件，Rust gates 按配置跳过 |
| Markdown 本地链接 / staged diff 空白 | 通过 |
| 文件规模 | 本次 TypeScript/HTML 文件均小于 1,000 行；仓库扫描发现 13 个未修改的 Rust 文件既有超限 |

原始输出：[完整测试](raw/delivery/sdk-suite.txt)、[浏览器 JSON](raw/delivery/browser-smoke.json)、[文件规模](raw/delivery/file-size.txt)。Chrome 检查由 [browser-smoke.ts](../../npm-packages/@peri-sdk/benchmarks/browser-smoke.ts) 重现，临时浏览器 profile 和 fixture 服务均在退出时清理。`terminal-browser` 在当前 VS Code 终端不支持开分屏，因此采用隔离的无头 Chrome。

## 最终对抗修复

[独立审查](adversarial-review.md) 发现并复验两项真实问题：admission flush 内关闭 producer 后仍接纳订阅；订阅者同步取消自身后返回 rejected Promise 未被处理。两项已修复，新增回归全部通过；双文档重入、60 次离线恢复和 30 个大载荷版本攻击也通过。

性能数字属于第三轮冻结源码。之后交付代码仅在上述两处生命周期控制路径发生源变动，另有共享 Yjs 构建配置、公开入口与浏览器验证，见 [变动记录](raw/delivery/post-benchmark-changes.json)。未重命名旧数据为新测量；这些边界修复由独立复现和最终完整测试覆盖。

[Git hooks 输出](raw/delivery/pre-commit.txt)、[新增回归输出](raw/delivery/adversarial-regression.txt) 保留。

## WASM 失败对照

失败测试：`tests/wasm-acp-integration.test.ts` 的 `Agent start, send, list and load work through WASM ACP`。错误为 `store is unavailable: remote session store server_error`，发生在 WASM ACP Host 启动时，早于本次 Yjs 投影。

将基线提交 `567640f1837771442628970a56ce68c5a8f8e256` 的冻结 TypeScript 源码用 Bun 单独构建到临时目录，复用当前同一份 WASM release artifact，运行原集成测试；仍在相同位置得到同一错误，exit 1。[对照输出](raw/delivery/wasm-baseline-control.txt) 与 [构建脚本、WASM SHA-256](raw/delivery/wasm-control.json) 保留。

这个对照隔离的是本次 TypeScript 改动，不能证明整个 Rust 基线或外部 sqld 环境均正常。未修改或跳过该测试，也未把整套测试表述为通过。共享工作树中已有及并行出现的 Rust 改动均不纳入本次提交。

## 文档路由与提交范围

- SDK README 维护 schema、载荷版本、复制协议、消费约束与 demo 使用方法；`docs/code-index/peri-ts-sdk.md` 更新模块与验证入口。
- 架构标准、根/模块 CLAUDE 和 Rust ACP 契约未变，不复制另一套规范。
- 实验目录保留原始数据、冻结源码/harness 和独立验证；[综合结论](conclusion.md) 明确内存代价、未测网络边界与基线截断。
- 只提交 `npm-packages/@peri-sdk`、上述代码索引与本实验目录；不包含其他 Rust 工作树改动。
