---
name: codebase-index
description: 代码库速查索引 —— docs/code-index/ 下的 crate 速查表，把「我想做什么」映射到文件、入口符号与一句话行为契约。当目标 crate 已知但落点不明、需要跨文件链路或不变量，或要重建/更新索引时使用。已知文件或单一符号直接 Grep，不要先读索引。
---

# Codebase Index

## 何时使用

适用：

- 目标 crate 已知，但不知道行为落在哪个文件或入口。
- 需要跨文件查询链路、注册面、不变量（改行为前确认影响面）。
- 重建或更新索引。

不适用：

- 已知文件名、符号名或测试名：直接 `rg`，不要读索引。
- 需要设计理由或目标边界：读 `docs/design/`，索引不解释原理。

先按根 `CLAUDE.md` 的任务路由锁定 crate，再读该 crate 的索引。**不要一次读入
`docs/code-index/` 全部文件**：25 个文件约 60 万字节（以 `wc -c` 为准）、15 万 tokens 以上，
会挤占任务上下文。单文件中位约 11 KB（约 3.5k tokens），最大的 4 个各约 45–90 KB。

## 索引是什么

- 位置：`docs/code-index/<crate>.md`；专题或非 workspace 模块也有同名索引
  （如 `peri-ts-sdk.md`、`peri-cf.md`、`mcp-packages.md` 合并覆盖多个 mcp crate）。
- 本质：**行为 → 文件/符号** 的速查表。查询者带着「我想改 X 的 Y」进来，带着文件路径和入口符号出去。
- 原则：只给定位信息 + 一句话行为契约；不解释原理、不复制规范正文、不替代设计文档。
- 事实源关系：路径、符号、测试名和 ARC 编号必须可解析；索引不写固定源码行号。

## 查找流程（查询者视角）

先判断问题类型：

| 问题 | 做法 |
| --- | --- |
| 已知文件、符号或测试名 | 直接 `rg`，不读索引 |
| 落点不明（改行为、找入口、注册面） | 读 1–2 个 crate 索引 |
| 机制链路（取消、重试、重放、事件传播） | 索引锁定文件，随即 `rg` 源码 |
| 跨 crate 契约 | 索引「跨模块契约」→ ARC 编号 |

1. 按根 `CLAUDE.md` 路由确定 crate。路由表没有直接对应行时，按 crate 名与功能面推断，不要停在这一步。
   路由同时指向宿主与包（如 MCP → middlewares + mcp-packages）时，先判断改动在宿主接线还是包内实现，
   各读 1 份，不要两份通读。
   **不要用 `rg -l` 在高频词（cancel、event、frozen、MCP）上选候选**：
   这些词常命中 20/26 个索引文件。高歧义词改用复合正则（如 `cancel.*notif`）或直接进入已确定的
   crate 索引；`rg -l` 只用于低歧义的具体行为词。
2. 在速查表里按行为关键词匹配条目。英文术语与中文表述都试一次；同一概念可能写作「触发阈值」
   「cancel」「取消」。符号名有命名风格差异：先用 `rg -i '<keyword>'` 或同时试 `scheduler|Scheduler`，
   避免只搜小写漏掉 PascalCase。
3. 索引表格行可能很长（单行 2–4 KB），`rg -n` 多次命中会触发 10k 输出截断。用
   `rg -o '.{0,80}<关键词>.{0,120}' docs/code-index/<crate>.md` 或 `awk 'NR==n' <file>`
   读取单行，再打开主文件。**索引文件超过 40 KB 或 200 行时只读命中行，不整读**；
   整读上限是 2 个文件、每个不超过 40 KB。
4. 打开主文件，用条目给出的符号名或测试名定位（`rg -n '<symbol>' <file>`）。
5. 一句话行为契约只用于判断“这是不是我改的点”，细节以代码为准。**索引不解释机制**：取消、
   重试、重放等传播链路先用索引锁定文件，随即直接 `rg`（如 `rg -n 'notify_cancelled|cancel'`），
   不要期待条目给出完整链路。
6. 跨 crate 链路先看各索引的「跨模块契约」节，再按 ARC 编号查
   `docs/standards/architecture-contracts.md`；没有汇总页。
7. `frozen`、`snapshot`、`event` 等词有多个含义，先用相邻词缩小范围，不要只按单词检索。
8. 会话存储类问题（历史分页、schema、迁移、归档）同时看 `peri-acp` 与 `peri-resources` 索引；
   根路由的 `session → peri-agent` 指执行循环，不是存储实现。
   权限与工具面问题同时看 `peri-middlewares`（子 Agent 过滤器）与 `peri-agent`（`ToolFilterPolicy`），
   两边各取 1 行命中条目即可。
9. 相近条目要区分场景：`session/load` 历史分页与 context archive 的原文/附件分页不是同一套实现。

## 构建/更新流程（构建者视角）

触发时机：索引缺失、行为变更后索引过期、用户要求重建/扩充。

输入（有则读，无则跳过）：

- 目标 crate 源码。
- 该 crate 的 `CLAUDE.md`（11 个索引目标没有 crate 级指引，此时直接读源码 +
  `docs/standards/architecture-contracts.md` + 相关 `docs/design/`）。
- 跨模块契约 ARC 编号；最近相关 `spec/issues/`。

步骤：

1. 读 crate 的 `CLAUDE.md`（若存在），Scope / 数据流 / 稳定不变量进「架构速览」。
2. **用 `rg` 验证每个路径、函数名、测试名真实存在**；禁止凭记忆写路径。
3. 逐子系统列条目；每条覆盖：功能 | 文件 | 入口/关键符号 | 一句话行为契约。
4. 关键逻辑写行为契约（阈值、顺序、条件、边界），一句话讲清，不复制文档正文。
5. 标注事实源关系：`re-export` / 配置事实源 / 注册面要写清归属。示例：
   `peri-agent/src/agent/compact_v2/config.rs` 仅 re-export `peri-acp-types::compact::CompactConfig`；
   工具注册面是 middleware `collect_tools()` 与 session `SessionToolCatalog`，不只是工具模块本体。
6. **写决策点，不写下层消费点**：入口应指向实际过滤/判断位置（如 `reason.rs` 的可见性过滤、
   `tool_search/middleware.rs` 的 direct/deferred 切分），不要只写接收结果的 `build`/`new`。
7. **注册面条目要覆盖完整面**：声明（`is_direct`、`prompt_declaration`）、协议可见性
   （`visible_to_model`）、builtin 的 `model_visible`、包装层透传、目录发布与测试落点；
   只写 `collect_tools()` 会漏掉 host-only 工具与包装后退化。
8. 同一主题只保留一个主条目。跨文件出现的主题（deferred 注册面、frozen 载体）在主 crate
   索引写完整条目，其余索引只写一句指针，避免拆散后单页查不全。
9. doc comment 与代码不一致时，按 `STD-INDEX-002` 区分实现缺陷、文档过时和获批契约变更；
   索引记录当前行为与契约状态，不自行用单一优先级裁决目标。
10. 跨 crate 链路（事件、cancel、工具可见性、prompt frozen 等）进「跨模块契约」节，指向
    ARC 编号；涉及多个 crate 的链路同时在相关 crate 的索引里互相可见。
11. TUI / UI 类 crate 额外列「关键控件/组件」表。
12. 自检（逐项执行，不靠目测）：
    - 抽取新增／修改条目中的反引号路径，逐个 `test -e`。
    - 抽取符号名，逐个 `rg -w '<symbol>' <crate>/src`。
    - 抽取测试名，逐个 `rg -F '<test_name>' <crate>`。
    - ARC 编号逐个在 `docs/standards/architecture-contracts.md` 中存在。
    - header 写 `更新：YYYY-MM-DD`（标题后空一行）；新增或修改条目时必须写，存量缺失不阻塞查询，
     按文件 mtime 判断可疑度。超过 90 天视为可疑，复核后再引用。
   - 自检四步用上面的 `test -e` / `rg` / `rg -F` 命令逐项执行；没有现成脚本时手工跑一遍。

## 推荐格式（不是强制模板）

```markdown
# <crate> 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：YYYY-MM-DD
> 依据：<crate>/CLAUDE.md（如有）、docs/standards/architecture-contracts.md、源码

## 架构速览
- 数据流 / 循环入口 / 稳定不变量（来自 crate CLAUDE.md；无指引时来自源码）

## 速查表
| 我想做什么 | 主文件 | 入口/关键符号 | 关键逻辑 |
| --- | --- | --- | --- |
| （行为/修改意图） | （路径；标清事实源/re-export） | （真实符号名或测试名） | （一句话行为契约） |

## 子系统（按需）
### <子系统名>
| 功能 | 文件 | 入口/关键点 |

## 跨模块契约
- ARC-XXX-001：一句话要点 → `docs/standards/architecture-contracts.md`
```

约一半文件没有「速查表」节或使用专题结构，这是允许的；新增文件建议按上表写。
专题文档（`git-stats.md`、`image-spike.md` 等）不必强制套 crate 模板。

## 质量红线

- 路径、符号、测试名、ARC 编号必须可解析；**禁止固定源码行号**。
- 行为描述与代码一致；冲突按 `STD-INDEX-002` 分类记录，不把实现缺陷写成目标契约。
- 不复制规范正文、不写设计理由；跨模块内容只指向 ARC 编号。
- 每个索引文件必须有 `更新：YYYY-MM-DD`；超过 90 天先复核再引用。
- 索引过期比没有索引更危险：行为变更后（阈值、顺序、入口改名）必须同步更新对应条目。
- 索引是定位工具，不是机制说明：链路、阈值推导和失败恢复细节留给源码与设计文档。
- 新增、改名或删除索引时，同步根/模块 `CLAUDE.md` 路由、`docs/standards/` 与
  `docs/code-index` 内的交叉引用（`DOC-UPDATE-001`）。
