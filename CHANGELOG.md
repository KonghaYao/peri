# Changelog

[GitHub Releases](https://github.com/KonghaYao/peri/releases) 记录各 `agent-v*` 发布版本的变更和下载资产。主分支的改动不等于已经发布。

以下保留早期 Perihelion Agent 的历史记录。

---

## v0.99.14 — 2026-06-02

### Performance

- 全局分配器从 mimalloc 切换到 jemalloc，碎片管理更优
- tokio worker_threads 限制为 4，18 核机器节省约 56 MB 栈空间
- list_threads 排除 cached_context 大字段，每线程内存从约 1 MB 降至约 1 KB
- LlmCallStart.messages 改为 Arc\<Vec\>，消除每次 LLM 调用的全量 clone
- history_for_cancel 用 Option\<MessageId\> 替代完整消息 clone

### Features

- **Rewind 对话回滚**：双击 ESC 弹窗选择回滚点，支持 /rewind 命令
- **/gc 命令**：手动内存回收 + RSS/jemalloc breakdown 诊断

### Bug Fixes

- PermissionRequest hook 在 Bypass/AutoMode 下不应触发
- 从 ~/.claude/settings.json 加载全局 hooks + TUI 退出时 fire SessionEnd
- /clear 时关闭旧 session 防内存泄漏
- 过滤 ACP 下发命令中与本地注册表重复的条目
- AgentResult invoke 消息优化，防止 LLM 轮询循环

### Refactoring

- CLAUDE.md 拆分为子模块文件
- 提取 ACP 共享逻辑，消除 TUI/Stdio 重复代码
- 移除 /split 命令
