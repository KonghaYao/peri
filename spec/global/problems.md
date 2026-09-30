# 历史问题检索

本文件只提供历史检索路由，不保存逐 issue 摘要、实施日志或事故副本。当前行为始终以代码、契约测试、`docs/standards/`、模块 `CLAUDE.md` 和 `docs/design/` 为准；未关闭工作见 `spec/issues/`。

## 压缩日志（2026-09-26 及以前）

2026-09-26 及以前的 issue 已压缩为 [spec/history/](../history/) 月志（[2026-07](../history/2026-07.md)、[2026-08](../history/2026-08.md)、[2026-09](../history/2026-09.md)）：普通条目为一行式摘要，定级事故（P0/P1/P2）附少量描述。月志只用于检索与回顾，不构成现行事实；另注：2026-09-27 起的新 issue 仍保留在 `spec/issues/`。

```bash
# 定位某份原文的删除提交（文件名见月志条目）
git log --diff-filter=D -- spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md

# 读取删除前的完整原文
git show <删除提交>^:spec/issues/2026-09-17-p0-workspace-validation-blocks-input.md
```

## 按主题查当前事实

| 主题 | 当前事实源 | Git 中的旧归档路径 |
| --- | --- | --- |
| Agent loop、Compact、session runtime | `peri-agent/CLAUDE.md`、`docs/code-index/peri-agent.md` | `spec/archive-issues/agent-core/` |
| ACP、事件、transport、session | `peri-acp/CLAUDE.md`、`docs/code-index/peri-acp.md`、`docs/standards/architecture-contracts.md` | `spec/archive-issues/acp-protocol/`、`spec/archive-issues/architecture/` |
| Middleware、工具、SubAgent、MCP | `peri-middlewares/CLAUDE.md`、`docs/code-index/peri-middlewares.md`、`docs/design/tool-system.md` | `spec/archive-issues/tools/`、`spec/archive-issues/subagent/` |
| Workflow 与 JavaScript host | `docs/code-index/peri-workflow.md`、`docs/code-index/peri-js-runtime.md`、`docs/design/workflow.md`、`npm-packages/@peri-workflow/` | `spec/archive-issues/workflow/` |
| Controller、Runtime、取消与会话工作区 | `docs/standards/architecture-contracts.md`、`docs/code-index/peri-controller.md`、`docs/code-index/peri-runtime.md`、`docs/code-index/peri-resources.md`、`docs/code-index/peri-process.md` | `spec/archive-issues/architecture/` |
| Model provider 与 prompt cache | `docs/code-index/peri-model.md`、`docs/design/model-adapters.md`、`docs/design/system-prompt.md` | `spec/archive-issues/llm-provider/` |
| TUI 渲染、输入、面板、交互 | `peri-tui/CLAUDE.md`、`docs/code-index/peri-tui.md`、`docs/design/tui-acp-data-flow.md`、`docs/design/user-input-queue.md` | `spec/archive-issues/tui-*/` |
| Langfuse | `docs/reference/langfuse-data-integrity.md`、`docs/code-index/peri-controller.md`、`docs/code-index/langfuse-client.md` | `spec/archive-issues/langfuse/` |
| 测试、CI 与跨平台证据 | `docs/standards/testing.md`、`e2e/CLAUDE.md` | `spec/archive-issues/code-quality/`、`spec/reviews/` |

## 在 Git 中查完整记录

```bash
# 查看某个历史目录的提交
git log --all -- spec/archive-issues/agent-core/

# 按文件名或关键词定位变更
git log --all --name-only -- '*compact*'
git log -S '关键词' --all

# 读取删除前的文件
git show <revision>:spec/archive-issues/<domain>/<file>.md
```

`spec/archive-issues/`、`spec/reviews/` 与旧 `spec/global/domains/` 已退出当前文档集，但完整内容仍在 Git 历史中。不要为已关闭事项重新建立过程文档；只有仍需实施或验收的工作才进入 `spec/issues/`，稳定结论应更新对应 standard、design 或 code-index。
