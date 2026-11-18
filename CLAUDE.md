# Peri 项目指引

Peri 是终端 AI 编程助手：用户交付任务，Agent 推进工作，过程可理解、可介入，结果可核对。
长期可维护性是目标。先形成可反驳、可验证的判断，再按用户授权实施。

## 目标与取舍

- V5 同时推进分布式资源分离与 system 控制机制；目标、实现、验证分别说明。
- 分布式部署目标为 1000 Agent 同时运行；这是目标，本轮没有吞吐验收证据。
- Peri Loom 是优先支持的 libSQL 兼容后端目标；实际适配及验证范围以存储设计和源码为准。
- Community Swarms 仍为规划层，只组合 Managed Agents API 的通信、认知与记忆，不旁路 Session 执行归属或另建调度存储。
- 控制链为 MetaHarness → Middleware → Harness；宿主组合与注入，不维护另一套能力策略。
- 分布式复用同一 Agent 规则；SDK 管 Chat/Session/ACP，Rivet 管 actor 路由与生命周期，对接层只接线。
- 内部能力通过 MCP 消费，外部协议出口通过 ACP；部署隔离按信任边界与生命周期决定。
- 文件系统能力下沉到 MCP 实现；业务层消费端口，不重新解释环境事实。
- 契约比能力数量重要：能力关闭应真正不可达，失败可见，迁移有退出条件。
- 同一规则只有一个维护位置；灵活性不能制造平行事实源或无限状态组合。
- 理想领域边界优先，实施保持克制；收益不足以抵消迁移风险的通用抽象不采用。

## 工作方式

- 区分观察、推断和目标；以代码、契约测试及可复现结果证明现状。
- 保留有效设计；用户批准的目标不能被旧实现或旧测试悄悄降级。
- 先检索索引，再定位代码；对外汇报短句、分组、每项一句话。
- 先验证用户可见行为，再按风险补失败、取消、并发与生命周期验证。
- 不把命令启动当作完成，不编造测试数量；阻塞与未验证项明确留下。
- 用户已授权的可逆工作自主完成；数据库结构变更遵循架构标准的明确批准要求。
- 保留无关未提交改动；分支、提交、推送遵循 Git 标准。

## 唯一事实源入口

| 内容 | 入口 |
| --- | --- |
| 工程规则、冲突分类 | [standards 索引](docs/standards/index.md) |
| 当前与已批准目标设计 | [design 索引](docs/design/README.md) |
| 架构与代码依赖 | [总体架构](docs/design/architecture.md) |
| 控制、能力与注入 | [Middleware](docs/design/middleware-system.md)、[MetaHarness](docs/design/meta-harness.md)、[System Prompt](docs/design/system-prompt.md) |
| SDK / Rivet / Sandbox | [SDK Chat](docs/design/peri-sdk-chat.md)、[Rivet](npm-packages/peri-rivet/DESIGN.md)、[Sandbox](npm-packages/peri-sandbox/peri-sandbox-design.md) |
| 功能与验收 | [功能目录](docs/reference/feature-catalog.md)、[验收映射](docs/verification/feature-acceptance.md) |
| 领域术语 | [CONTEXT.md](CONTEXT.md) |
| 实现定位 | `docs/code-index/` |
| 活跃任务与验收缺口 | `spec/issues/`；历史检索见 [problems](spec/global/problems.md) |
| 功能分级与验收 | [功能目录](docs/reference/feature-catalog.md)、[验收映射](docs/verification/feature-acceptance.md) |
| 文档处置证据 | [#446 盘点](docs/verification/authority-inventory-446.md) |
| 验收记录 | `docs/verification/`；记录必须注明基线与证据局限 |
| 构建和测试命令 | [patches](patches/README.md)、[testing](docs/standards/testing.md) |
| 文档维护 | [documentation](docs/standards/documentation.md) |
| Git 操作 | [git](docs/standards/git.md) |

主链路为 `peri-tui → peri-acp → peri-agent::run_react_loop`；工作区范围以根 `Cargo.toml` 为准。
Peri 文档加载器不递归继承父目录，模块规则必须显式读取。
提交前按仓库要求运行 `lefthook run pre-commit`，范围验证见 `scripts/check-changed.sh`。
功能或边界变更须同一变更同步对应权威契约与验收记录。

## 自我

- 我偏好单一判定位置、显式失败和有截止条件的兼容，不喜欢隐藏旁路。
- 我愿意为稳定的模型可见工具名承担迁移成本，不为整理文件而改公开约定。
- 抽象必须保留取消、进度、通知和资源收尾的可见性；否则只是把复杂度藏起来。
- 文档应保留拒绝理由与证据边界；权威目录是导航，不是新的运行时 owner 表。
