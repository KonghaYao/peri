# Agent 建议与实际可加载定义一致

对应 `PERI-20260913-AGENT-SUGGESTIONS`，问题原始记录保存在 `73df17f7`。

建议现在由实际 `SubAgentTool` 在 loader 失败后生成。候选按本次参数 cwd、冻结的 builtin policy、项目与 plugin 来源枚举，并逐项复用同一个 loader 验证；非法定义、被非法项目文件遮蔽的 fallback、已删除文件不会仅因出现在旧 catalog 中而被建议。默认错误 registry 不再从 snapshot 推断 Agent 可加载性。

同步与后台路径都使用实际参数 cwd。显式 MCP Agent 继续经过 activation/approval；discovery metadata 或断开的 peer 不足以让它成为本地建议候选。prompt 中的 catalog 仍是会话提示，运行期的建议核验不重写冻结 prompt。

独立审查检查了 7 个新增场景：调用目录、会话中增删文件、flat/nested/plugin/builtin、非法 frontmatter 与 shadowing、真实同步 invoke、真实后台 invoke、MCP 未激活与断开。同步真实 invoke 中 B 目录的 Agent 可以启动 EchoLLM，A 中不可见；loader 失败时 factory 计数不增长。后台失败用真实 TaskManager，但没有把这项用例表述为后台成功生命周期验证。MCP fixture 只有无 peer 的 registry metadata，无外部网络请求。

历史建议错误用于选择修复方向；这些契约测试不能给出历史任务成功率或生产中的收益。测试范围、命令、计数和源文件 hash 记录在本目录的 validation 文件中。

协调者在干净提交快照中运行整个 SubAgent 工具模块：100 passed、0 failed；既有 error_suggest 回归：29 passed、0 failed。两条命令均最终以 0 退出；上次回合中断后未获取 shell 终态的日志未作为本次验收记录，本目录保存的是重新核验的最终运行。

最终集成还发现旧 prompt 测试仍要求 catalog 是 authoritative list，且选择指南只允许静态 catalog 中的 ID。补漏明确区分冻结 hint 和本次 loader 返回的有效 suggestion；允许采用 prompt 冻结后出现的建议 ID，但仍由调用 cwd 与冻结 policy 的 loader 决定可加载性。只有 ID 的建议不能证明能力或访问权限，缺少元数据时先核验定义或保守安排执行。静态 catalog 不被描述成动态刷新、已验证的事实源。补漏的测试与最终快照验证记录在 [模型边界集成验收](../model-error-context/README.md)。

补漏已单独提交为 `9495928b`。协调者在只含这次提示词/skill 改动的隔离快照中运行 `cargo test -p peri-acp --lib prompt::tests`：105 passed、0 failed、exit 0；正常提交 hooks 全部通过。与工作区独立评审时的 107 项计数不同，隔离提交没有纳入尚未提交的两个模型诊断 prompt 测试。来源与命令记录见 [补漏验收 JSON](prompt-completion-validation.json)，文本契约测试不替代前述 loader 的运行时验证。

最终完整 Middlewares 库验证又暴露一处旧选择句断言，已在 `d370c4bf` 同步；其独立审查与 29 项模块测试通过，随后整个最终矩阵重新运行通过。此集成补漏没有改变 loader 或权限行为。
