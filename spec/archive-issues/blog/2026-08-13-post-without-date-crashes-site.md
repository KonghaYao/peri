# 文章缺少日期时站点崩溃

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-13

## 问题描述

peri-cool 在文章排序阶段读取 `data.date.getTime()`。当 collection 中存在尚未填写日期的文章时，页面抛出 `Cannot read properties of undefined (reading 'getTime')`，导致站点不可用；期望缺少可选日期的文章仍能稳定显示。

## 症状详情

错误定位到 `peri-cool/src/lib/content.ts` 的 `sortPosts()`，调用方包括首页、文章总览、分类页和文章侧边栏。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 让 `posts` collection 包含一篇 `data.date` 为 `undefined` 的文章。
  2. 调用 `sortPosts()` 或渲染使用它的页面。
- **环境**：Astro 7 / Starlight，开发或构建期间的 posts collection。

## 涉及文件

- `peri-cool/src/content.config.ts` —— 将文章日期定义为可选。
- `peri-cool/src/lib/content.ts` —— 当前无条件调用 `date.getTime()`。
- `peri-cool/src/pages/posts/[category]/index.astro` —— 当前无条件格式化日期。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-13 | — | Open | agent | 创建 |
| 2026-08-13 | Open | Fixed | agent | 修复：无日期文章稳定排末尾，分类页仅在日期存在时渲染日期 |

## 修复记录

### 修复 #1（2026-08-13）

- **操作人**：agent
- **用户原意**：修复文章排序读取未定义日期时的 `getTime` 崩溃。
- **修复内容**：排序函数为可选日期提供负无穷 fallback；分类索引条件渲染日期；新增缺日期与同日期排序回归测试。
- **涉及 commit**：无
- **验证状态**：已验证（`bun test src/lib/content.test.ts`、`bun run check:site`、`git diff --check`）
