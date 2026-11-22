# #446 功能与证据审查

基线：cee019f34337e420178be0c2671c98f8c23c4083。状态：第一轮已实施，第二轮待交叉审查。

## 第一轮

| 意见 | 裁决 | 理由与实际修改 |
| --- | --- | --- |
| 旧 159 项不是最新事实；直接复制将漏掉新控制契约与 async-only | 采纳 | 据 workspace、middleware、ACP、MCP handlers、TUI、SDK/Rivet 入口建立 204 项目录；旧表未附，明确逐项迁移对账缺口 |
| 等级定义缺失不能冒充最新权威 | 采纳 | 检索仓库、维护群五级/L1记录、#446 和 peri-e2e #10，未取得定义；显式给出本变更建议并应用的等级语义，需维护者对账旧裁决 |
| 功能表同时维护 owner、实现状态、P级会形成第二控制权威 | 采纳 | 功能表仅四列；实现及验证拆到证据表，owner 留在现有控制契约/源码 |
| 文档存在与历史 PR 通过不能证明当前行为 | 采纳 | 每项记录源码入口及具体场景/断言对账缺口；本次不虚报行为复验 |
| 每能力强制一个新 E2E | 拒绝 | 重复编排、维护成本高；多对多断言映射由外置 #10 承接，L5 无新增义务 |
| 重新实现 #443、子任务同步接口与 ToolSearch 目标 | 拒绝 | #455/#453 已合并；#456/#460 在途；本单只记录源码状态，不改运行时 |

## 证据边界

源码入口核对是静态盘点，并非逐函数正确性证明；204 是本次条目数而非覆盖率。现有目录缺失的契约细节不能仅靠表格登记宣称已闭环。等级旧裁决、旧 159 项稳定 ID 对账以及外置 E2E 场景映射仍需相应来源。

## 第二轮修正

- 采纳 authority reviewer 的契约路由反证：Monitor、时间、组件、memory、缓存、Git 观察不再按大组指向无关设计；冻结与段落覆盖改指 MetaHarness。
- 采纳恢复遗漏：新增 canonical 持久历史、进入会话按 work_state 恢复、子树结果路由及冻结快照边界；不以 MQ 一行代表整套恢复。
- 补齐 MCP Apps 附件、多路隔离与清理，及 hook/引用/目标提醒入口。
- 采纳主审意见：核心逐条提供具体源码与既有相关测试路径，保留未复验；测试文件存在不作为断言覆盖证明。

## 覆盖盘点面

| 源码面 | 功能域 | 粒度与排除 |
| --- | --- | --- |
| Cargo workspace 与 peri-agent stages/session | CORE、COMPACT、SESSION、MQ、DURABLE、TASK | 执行、消息、恢复、子任务分别登记；辅助函数不独立评级 |
| peri-middlewares 装配与各模块 | CTRL、PROMPT、INTERACT、MCP、EXT | 按控制、注入、交互与扩展区分，不创建 owner 映射 |
| mcp-packages 各 handler/resources | WORKSPACE、EXT、WORKFLOW、MCP | 文件、Shell、资源与任务分开；common 为复用实现不新增产品能力 |
| peri-acp host requests/history/apps | SESSION、HISTORY、APPS | 独立外部入口与协商边界；DTO 字段不逐项登记 |
| peri-config、peri-model | CONFIG、MODEL | 配置来源/保存及 provider 协议独立；参数组合不各算功能 |
| peri-tui kit panels/input/history、components | TUI、CLI、PLATFORM | 主要用户操作独立；纯样式 L5 |
| npm @peri-sdk、peri-sandbox | SDK、PLATFORM | 通用 SDK 与沙箱能力分列；试验性 peri-rivet、agent-builder 已于 2026-11-22 移除，主线归 peri-swarm |
| peri-wasm/time/runtime/js-runtime、langfuse、memory-plugin | PLATFORM | 平台与观测支持能力；运行时内部工具函数不独立登记 |
| side-projects、images、示例与构建脚本 | 不作为发布能力 | 实验、资产和工程执行入口；不冒充功能覆盖或验收 |

覆盖面交叉核对不构成数学上的无遗漏证明；后续新能力必须同步登记。旧 159 项原文件未提供，无法伪造旧 ID 的合并/退役映射。

## 第二轮独立审查其他结果

- Validator：建议约束 U 必须有评级缺口说明，且验收实现/验证列不得为空，避免五列空格形成假映射；交 validator owner 修正并补负例。
- Authority：根指引从重复架构长文收敛为路由，符合预算且保留设计取舍；补功能/验收导航与标准索引入口。保留 SDK/Rivet 分界，未新增控制 owner 表。
- 不采纳全量运行 Rust/E2E 以证明文档改动：不产生新增运行时证据，成本高；现有行为证据定位与真实未复验标记足够支撑本次文档检查，运行缺口仍独立跟踪。

二轮末核补齐 Todo、图像输入与 CLI 独立入口；纠正 CONFIG 插件、SDK 装配、各 provider、Rivet、PLATFORM 的源码路由，避免同组目录误指。最终 204 项仅为当前盘点数量。

主审反证采纳：内存全局预算无已完成的单一设计，F-PLATFORM-007 改指 #447 目标承接记录，明确部分实现/长时缺口；不再错误指向 serverless identity。
