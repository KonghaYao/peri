# Agent 建议与实际可加载定义一致

对应 `PERI-20260913-AGENT-SUGGESTIONS`，问题原始记录保存在 `73df17f7`。

建议现在由实际 `SubAgentTool` 在 loader 失败后生成。候选按本次参数 cwd、冻结的 builtin policy、项目与 plugin 来源枚举，并逐项复用同一个 loader 验证；非法定义、被非法项目文件遮蔽的 fallback、已删除文件不会仅因出现在旧 catalog 中而被建议。默认错误 registry 不再从 snapshot 推断 Agent 可加载性。

同步与后台路径都使用实际参数 cwd。显式 MCP Agent 继续经过 activation/approval；discovery metadata 或断开的 peer 不足以让它成为本地建议候选。prompt 中的 catalog 仍是会话提示，运行期的建议核验不重写冻结 prompt。

独立审查检查了 7 个新增场景：调用目录、会话中增删文件、flat/nested/plugin/builtin、非法 frontmatter 与 shadowing、真实同步 invoke、真实后台 invoke、MCP 未激活与断开。同步真实 invoke 中 B 目录的 Agent 可以启动 EchoLLM，A 中不可见；loader 失败时 factory 计数不增长。后台失败用真实 TaskManager，但没有把这项用例表述为后台成功生命周期验证。MCP fixture 只有无 peer 的 registry metadata，无外部网络请求。

历史建议错误用于选择修复方向；这些契约测试不能给出历史任务成功率或生产中的收益。测试范围、命令、计数和源文件 hash 记录在本目录的 validation 文件中。

协调者在干净提交快照中运行整个 SubAgent 工具模块：100 passed、0 failed；既有 error_suggest 回归：29 passed、0 failed。两条命令均最终以 0 退出；上次回合中断后未获取 shell 终态的日志未作为本次验收记录，本目录保存的是重新核验的最终运行。
