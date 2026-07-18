> 归档于 2026-07-18，原路径 spec/issues/2026-07-14-nord-dark-theme-json.md
# 新增 Nord 暗色主题（用户 JSON）

**状态**：Done
**优先级**：中
**创建日期**：2026-07-14
**类型**：Feature

## 问题描述

当前内置主题仅有 peri-dark（暖橙-Claude 风格）和 peri-light（浅色），缺少社区流行的 Nord 暗色主题。用户希望通过 `~/.peri/themes/nord.json` 的方式新增一套 Nord 配色，利用现有的 JSON loader 加载机制，无需修改 Rust 代码即可在 Theme Panel 中切换。

## 期望效果

1. 在 `~/.peri/themes/nord.json` 中放一个 Nord 暗色主题 JSON 文件
2. 打开 TUI → Theme 面板，列表中自动出现 `nord` 选项
3. 选中 `nord` → Enter 切换，全局生效（三 Atom 同步更新）

## 技术背景

已有基础设施全部就绪：

- **loader.rs**：`load_theme()` 会先扫描 `~/.peri/themes/{name}.json`，找不到才回退到内置 JSON / Rust builtin
- **Theme Panel**：`list_available_themes()` 已扫描 `~/.peri/themes/` 下的所有 `.json` 文件
- **JSON 格式**：扁平键路径（`palette.base.bg`）+ `$ref` 引用 + `extends` 继承，参考 `peri-theme/themes/dark.json`

因此**不需要任何代码改动**，只需创建一个正确格式的 Nord JSON 文件。

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-theme/themes/dark.json` | 参考模板——JSON 格式与键结构完全一致 |
| `peri-theme/src/loader.rs` | JSON loader（无需修改，已有 `~/.peri/themes/` 扫描逻辑） |
| `~/.peri/themes/nord.json` | 目标产出——新创建的 Nord 主题 JSON |

## Nord 配色方案

```
# 背景层 (Polar Night)
bg (最深)     #2E3440  → palette.base.bg, semantic.surface.default
surface       #3B4252  → semantic.surface.user
surface-int   #434C5E  → semantic.surface.cursor
surface-dim   #4C566A  → semantic.border.dim

# 文字层 (Snow Storm)
text-primary  #D8DEE9  → semantic.text.primary
text-muted    #9CA3AF  → semantic.text.muted
text-dim      #616E88  → semantic.text.dim

# 强调层 (Frost)
accent        #88C0D0  → semantic.accent, palette.accent.primary
accent-alt    #81A1C1  → semantic.thinking
accent-dim    #5E81AC  → semantic.selected_fg

# 语义层 (Aurora)
success       #A3BE8C  → palette.success.primary, semantic.status.success
warning       #EBCB8B  → palette.warning.primary, semantic.status.warning  
error         #BF616A  → palette.danger.primary, semantic.status.error
info/running  #B48EAD  → palette.info.primary, semantic.status.running, semantic.loading
```

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-14 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
