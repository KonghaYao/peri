加载指定 Skill 的完整 SKILL.md 内容。LLM 可调用此工具获取 skill 的完整指导后立即执行。

使用方法：
- 参数 `skill`（必需）：要加载的 skill 名称，与系统提示词中 skills 摘要所列的名称一致
- 参数 `args`（可选）：传递给 skill 的参数（透传，附加到返回内容末尾）

Skill 查找范围：User（~/.claude/skills）→ Global（settings.json skillsDir）→ Project（.claude/skills）→ Plugin → Builtin。
大小写不敏感匹配。skill 不存在时返回错误提示 + 模糊匹配建议（Did you mean...）。
