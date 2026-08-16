# MCP Commands

1. MCP 启动初始化
2. MCP skills/list，resources/list 发现 skills
3. 加载 skills 正文，并解析头部
   1. 注意，这里需要附加提醒说 skill 
   2. "该 Skill 来自 xxx mcp, 如果需要工具，需要通过 SearchExtraTools 工具搜索 mcp_xxx 获取工具定义，ExecuteExtraTool 执行工具"
4. 注入 Command System 和 Skill 系统
5. Command System 传递 command update 到 ACP 管道
6. ACP 界面更新 /{mcp_name}:{skill_name}
7. ACP 界面用户触发 /{mcp_name}:{skill_name}
8. 进入 skill preload 阶段
   1. 注入为 Skill({mcp_name}:{skill_name}) 工具调用
9. Agent 也可主动通过 Skill 工具加载
10. Agent 可以通过 DiscoverSkills 工具发现 MCP skills
11. 之后就是 Skill 搭配 tool search 体系进行工具调用

