# Workflow artifact 运行时离线化验收记录

- 状态：验收记录（2026-11-22）
- 分支：`fix/remove-workflow-npm-fallback`，基线 `origin/main@1b7d3557b`
- 范围：移除 Peri 运行时对 npm/registry 的 Workflow artifact 回退；解析只保留显式注入、
  本机缓存与随包 sidecar。
- 不在范围：构建期 `bun install` 的依赖下载；Rust 零 JS 嵌入红线保持不变。

## 1. 结论

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| registry/网络回退代码删除 | 通过 | `peri-workflow/src/runner/artifact.rs` 不再包含 `RegistryArtifact`、`WORKFLOW_REGISTRY_*`、`fetch_and_publish_from_registry`、`PERI_WORKFLOW_ALLOW_NPX_FALLBACK` |
| 解析仅使用已校验本地来源 | 通过 | `resolve_artifact` 顺序为显式注入 → 本机缓存 → 随包 sidecar；三者统一由 `WORKFLOW_ENTRY_SHA256` 校验 |
| 无可用 artifact fail closed 且不再提示联网 | 通过 | `prepare_fails_closed_without_artifact` 断言错误包含 sidecar 路径与 `PERI_WORKFLOW_ARTIFACT_DIR`，不包含旧开关 |
| 旧内容身份缓存不被复用 | 通过 | `resolve_ignores_cache_entries_outside_the_bundled_content_identity` |
| 权威文档同步 | 通过 | `docs/design/workflow.md`、`workflow-mcp.md`、`architecture-contracts.md`、`environment-variables.md`、`code-index/peri-workflow.md`、npm README/DESIGN、ultracode SKILL |

## 2. 验证命令与结果

- `./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-workflow --lib -- --test-threads=1`：
  284 passed / 0 failed。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-workflow -- --check`：通过。
- 根因复现：`npm view @peri-code/workflow@0.3.0` 返回 `periProtocolVersion=2`，源码 pin
  期望 3；旧 fallback 因此必然校验失败并误报 stale/tampered。

## 3. 环境与局限

- 并行全量跑 `peri-workflow --lib` 时，`cli::tests::workflow_cli_spawns_prepared_artifact_for_help`
  偶发返回 1；单测与 `--test-threads=1` 全量通过，已记录于
  `workflow-codemode-tool-2026-11-22.md`，判定为既有 HOME 隔离问题。
- 未在 Windows 验证；未运行真实发布归档缺 sidecar 的场景。
- 构建期 `bun install --frozen-lockfile` 仍从 npm 拉取 JS 依赖（lockfile 固定），
  本次只移除运行时的 npm 路径。
