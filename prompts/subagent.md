请使用 subagent say hello

> 验证 bg agent 消息回调触发会话

请使用同步 subagent  用 shell say hello， 但是它要先 sleep 10s

请使用 bg subagent  say hello， 但是它要先 sleep 2s

> 验证 agent 树状聚合

请直接派出三个同步非 bg 的 hello agent say hello

> 验证 bg fork agent 消息回调触发会话

请使用 bg fork subagent  say hello， 但是它要先 sleep 10s

> 验证 bg subagent 的分 agent 情况查看

请使用 bg fork subagent  say hello， 但是它要先 sleep 60s

> 验证 bg agent 消息并发回调触发会话

请直接派出三个 bg 的 hello agent say hello

> 验证 bg agent 的能力

派出 bg agent 执行下面的任务 ”说一句 1 然后调用两次 read， 然后说 2,两次 read， 重复直到 4“

> 验证 workflow 的

/ultracode 请派发一个简单的 workflow， 其中有三个阶段，三个阶段串行使用 agent sleep 5s

