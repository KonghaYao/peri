# Workflow Agent 中 SkillTool 缓存永远为空，Skill 不可用

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-23
**类型**：Bug

## 问题描述

技能系统缓存重构（commit 8a56f0d4）后，`SkillTool::new()` 移除了 `cwd`/`plugin_roots`/`disable_bundled` 参数和懒扫描回退逻辑，改为纯缓存查询。但 workflow agent（`workflow_agent.rs`）注册 SkillTool 时传入 `Arc::new(RwLock::new(None))` 空缓存，且 workflow agent 没有 `before_agent` 钩子来填充缓存，导致 Skill 工具在 workflow sub-agent 中**永远不可用**。

旧行为：workflow agent 首次调用 Skill 时可懒扫描 project-level skills（`{cwd}/.claude/skills/`），返回 Skill 内容。

新行为：永远返回 `"Skills cache is empty — before_agent may not have run"` 错误。

## 症状详情

| 场景 | 期望 | 实际 |
|------|------|------|
| Workflow sub-agent 调用 Skill("auto-issue-fixer") | 返回 SKILL.md 内容 | 返回缓存为空错误 |
| Workflow sub-agent 调用 DiscoverSkillsTool | 返回可用 skills 列表 | 返回缓存为空错误 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动任意 workflow
  2. Workflow sub-agent 尝试调用 Skill 或 DiscoverSkills 工具
  3. 工具返回缓存为空错误
- **环境**：所有环境

## 涉及文件

- `peri-acp/src/agent/workflow_agent.rs:329` —— SkillTool 注册点，传入空缓存
- `peri-middlewares/src/tools/skill.rs:30-60` —— SkillTool::lookup_skill() 纯缓存查询，无回退

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-23 | — | Open | agent | code review 发现 |
| 2026-07-23 | Open | Fixed | agent | 修复：注册前扫描 project skills |

## 修复记录

### 修复 #1（2026-07-23）

- **操作人**：agent
- **用户原意**：恢复 workflow agent 的 Skill 能力，使 workflow sub-agent 能查找 project-level skills
- **修复内容**：`workflow_agent.rs` 注册 SkillTool 前，调用 `scan_skill_roots` 扫描 `{cwd}/.claude/skills/`，将结果传入 `SkillTool::new(cached)` 替代空缓存
- **验证状态**：待验证
