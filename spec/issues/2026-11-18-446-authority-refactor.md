# #446 权威文档与功能验收追踪

状态：文档与校验交付；产品行为验收及历史评级对账不是本轮已完成事项。

## 基线与范围

- 工单：<https://github.com/KonghaYao/peri/issues/446>；执行时正文与评论已重新读取，无新增评论。
- 基线：`cee019f34337e420178be0c2671c98f8c23c4083`；隔离分支 `docs/authority-446`，原 checkout 无未提交改动。
- #435 已合并（`4b2acca5949f72d5689ac22ad5753fe7c244eec7`），SDK/Rivet 边界和 peri-cf 退役保留。
- #444 已合并（`25c183eba7bc35b6e29ee2d0749f02dcdcd39c0f`），History V1 是 beta 关闭回滚路径，不因重整删除。
- #443 经 #455 合并（`7f46bc8812817142ed0fe3871cd05ecd4a72aa41`）；#450 经 #453 合并（`595ea0be056465152b07f91a18cb082d29effbbc`）。旧 issue 复选框不能据此推断实现缺失。
- #456（工具命名 PR A）、#460（Dynamic MCP PR B）在首次核实时仍 open，不覆盖维护者在途实现，也不将其记作本基线事实。

## 迁移入口

- [全量文档清单及实际处置](../../docs/verification/authority-inventory-446.md)。
- [功能目录](../../docs/reference/feature-catalog.md)、[分级规则与待核对项](../../docs/standards/features.md)、[验收映射](../../docs/verification/feature-acceptance.md)。
- 两轮独立对抗记录：[文档](446-review-authority.md)、[功能](446-review-features.md)、[校验](446-review-validator.md)。

根指引从重复领域清单收敛为理念与导航；标准维护长期边界，现行/目标设计保留领域契约；实施顺序与一次性验证记录移到 active spec/verification。功能表不新增运行时 owner 或控制面，也不复制测试仓编排。

## 明确边界

- 本次不修改运行时、数据库结构或外置 E2E 场景。脚本通过只证明结构、引用和登记约束，不能证明设计或产品全部验收。
- 旧 159 项草表及五级定义未随工单提供；本轮目录按当前源码重新盘点，建议评级显式标注。旧 ID 对照和历史裁决核对仍需维护者补证，不虚构逐条继承关系。
- L1–L4 缺口由映射逐条列出；测试文件入口与历史 PR 结果不是本次重新运行结果。具体场景对账交 peri-e2e #10。
- Rust 工具链在本环境不可用；`scripts/check-changed.sh` 因 cargo metadata 缺失保守回退 workspace，随后退出 127。该检查不记为通过；本次无 Rust 变更。
- 最终命令、退出码、补丁应用检查、基线/最终 SHA 和远端核对见交付包日志及应用说明。
