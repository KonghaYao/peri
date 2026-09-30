# LSP 能力下沉至 LSP MCP 裁决

> 日期：2026-09-29。状态：已裁决并实施；不 commit。

## 裁决

LSP 客户端、配置加载、协议、诊断、`LspServerPool`、LSP 工具与 builtin handler 全部归
`mcp-packages/lsp`（crate `peri-mcp-lsp`）。顶层 `peri-lsp` crate 与
`peri-resources::lsp` 门面删除，不保留 shim 或 deprecated 别名。

host 仍是生命周期边界，但不再拥有 LSP 实现：`peri_mcp_lsp::load_merged_lsp_servers`
负责 global < plugin 配置合并，`peri_mcp_lsp::create_host_lsp_pool` 在 host 装配入口构造
**一份 host 级 pool**。`LspInstanceInput` 把该 `Arc<LspServerPool>` 注入 builtin `lsp`
实例；同一 `Arc` 向上转为 `Arc<dyn LspPoolPort>` 写入 `AssemblyContext::lsp_pool`，由
`LspSyncMiddleware` 消费。因此工具面与 `Write`/`Edit` 同步不创建第二份 pool。

host shutdown 仍持有同一 pool 的端口句柄，并在会话/任务收敛后显式 `await shutdown()`；
池内所有 language server 由 LSP MCP 的 pool/client 关闭实现有界收敛。`LspSyncMiddleware: false`
的既有语义不变；pool 仍为 host 级唯一实例，多 cwd 继续共享 host cwd 的 `root_uri`。

## 覆盖与取代

本裁决覆盖/取代：

- `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-plan.md`（已压缩至 `../history/2026-09.md` 2026-09-26 条目，原文见 Git 历史）中 A11、A21、A22 的
  host 工厂归属与装配条目：保留“host 级唯一 pool、同一 pool、host shutdown、root_uri
  退化”等行为约束，但将构造/配置实现从 `peri-middlewares` 移至 `peri-mcp-lsp`。
- `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-sub-plan-l-lsp-instance.md`（已压缩至 `../history/2026-09.md` 2026-09-26 条目，原文见 Git 历史）
  中依赖上层/Resources 门面持有 LSP 实现的对应条目：保留 builtin context 注入 seam，
  改为注入 `peri-mcp-lsp` 的具体 pool。
- `docs/design/mcp-adaptation-v4-part-1.md:198` 的“LSP MCP 负责工具、格式化与配置快照，
  宿主保留 pool 生命周期”目标被具体化为“LSP MCP 负责客户端、pool、配置与工具；host
  保留装配投影、同步端口消费和 shutdown 调用”。
- `docs/design/mcp-adaptation-v4-part-1.md:221` 的“`LspServerPool` 迁移后应成为
  LSP MCP 内部状态”由目标更新为已实施状态；host 只持有注入后的共享句柄，不恢复/复制实现。

未覆盖：`LspPoolPort` / `LspSyncError` 的变体与无默认实现约束；本次仅更新实现归属注释，
契约语义保持不变。

## 文件级结果

- 新增/迁移：`mcp-packages/lsp/src/{client,config,diagnostics,error,jsonrpc,pool,protocol,uri}.rs`
  及其测试。
- 删除：`peri-lsp/`、`peri-resources/src/lsp.rs`。
- 修改：`peri-mcp-lsp` manifest/lib、host assemble/shutdown 投影、middleware context/assembly、
  workspace/Cargo.lock、代码索引与 testing 标准索引。
