# 项目本地 ./.peri/settings.json 的 config.env 不生效

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-24

## 问题描述

在项目本地 `./.peri/settings.json` 的 `config.env` 中配置了 API 凭据（如 `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`），期望这些环境变量能注入到进程环境供 `LlmProvider::from_env()` 读取。但实际不会生效——本地配置的 env 变量没有被注入到操作系统进程环境。

对比：`~/.peri/settings.json` 的 `config.env` 可以正常生效。

## 症状详情

- 在 `./.peri/settings.json` 中写入 `{"config": {"env": {"ANTHROPIC_API_KEY": "sk-xxx"}}}` 后重启 peri
- `LlmProvider::from_env()` 读取不到 `ANTHROPIC_API_KEY`
- 将相同内容移到 `~/.peri/settings.json` 后则可以正常读取

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在任意项目目录下创建 `./.peri/settings.json`，写入 `{"config": {"env": {"ANTHROPIC_API_KEY": "sk-xxx"}}}`
  2. 确保系统环境变量中未设置 `ANTHROPIC_API_KEY`
  3. 在该目录启动 peri
  4. peri 无法识别 API Key，提示无 provider 配置
- **环境**：所有平台

## 涉及文件

- `peri-tui/src/main.rs:239-246` —— `inject_env_from_settings()` 仅读取 `~/.peri/settings.json`，未包含项目本地路径
- `peri-acp/src/provider/store.rs:17-24` —— `workspace_config_path()` 返回项目本地路径，但未被 env 注入链路引用
- `peri-acp/src/provider/mod.rs:31-87` —— `LlmProvider::from_env()` 依赖进程环境变量，不从 `AppConfig.env` 读取

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-24 | — | Open | agent | 创建 |
| 2026-07-24 | Open | Fixed | agent | 修复：inject_env_from_settings 前插入项目本地注入 |

## 修复记录

### 修复 #1（2026-07-24）

- **操作人**：agent
- **用户原意**：项目本地 `./.peri/settings.json` 的 `config.env` 应和 `~/.peri/settings.json` 一样注入进程环境变量
- **修复内容**：在 `peri-tui/src/main.rs` 的 `inject_env_from_settings()` 之前新增项目本地 `./.peri/settings.json` 的 env 注入，复用已有的 `workspace_config_path()` 函数。优先级调整为：进程环境 > 项目本地 > Peri 全局 > Claude Code。
- **涉及 commit**：无（未提交）
- **验证状态**：待验证
