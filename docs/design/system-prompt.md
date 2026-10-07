# System Prompt 构建系统设计

> 状态：现行设计
>
> 冻结、能力闭包与缓存边界的强制不变量分别见 ARC-FROZEN-001、
> ARC-CAPABILITY-CLOSURE-001 与 ARC-SERIAL-001；自定义覆盖机制见
> [MetaHarness 设计](meta-harness.md)。

## 1. 设计边界

System Prompt 由两部分组成：会话级冻结的 base prompt，以及每次模型请求读取的
middleware contribution。两者都独立于 Transcript，不得伪装成 user/system 消息
写入对话历史。

- base prompt 在 `session/new` 时随日期、运行环境快照（platform / OS 版本 /
  是否 Git 仓库）、项目指引、skill 摘要与 MetaHarness 状态一起冻结和持久化；
  load、resume、fork 与 SubAgent 复用同一 owner snapshot；旧快照缺少结构化
  运行环境值时按 unavailable 显式标记，不重探本地值冒充；
- middleware contribution 由当前 session 的同一条生产链提供，在 `before_agent`
  完成能力目录准备后，于构造 `ModelRequest` 时同步读取；
- contribution 只能追加到冻结 base 之后，不能重写缓存前缀，也不能绕过当前
  session-local capability policy；
- System Prompt 不进入 Transcript，Compact 与 rewind 不修改它。

## 2. 所有权与段落来源

基础段、语言、审批、提问、Skills、SubAgent 等提示词分别由对应 middleware 持有。
middleware 缺席时，其工具、hook 和段落必须同时消失；不能保留“能力可用”的静态
说明。生产链与条件装配以 `peri-agent/src/session/factory.rs::production_blueprint`
及 `peri-middlewares/src/assembly.rs` 为准。

段落可见性按**执行面能力事实**投影（`peri-agent` 的 `SectionCapabilities` +
`peri-middlewares::prompt_policy`），事实来自真实装配结果或由其装配条件投影，
主链 / 子链 / workflow 共用同一策略；不维护第二份按执行类型硬编码的排除表。
除持有者是否存在，还要检查**有效模式**：例如 `PermissionMiddleware::disabled()`
（无 broker、无 mode）不声明审批段。子链没有基础段持有者时，基础段由会话冻结
模板继承，不因“子链收集为空”而得到空 system。

MetaHarness 在冻结期按 section ID 替换持有者提供的段落内容，并按 middleware 名关闭
整个能力。section ID 与 middleware 名清单的代码事实源是
`peri-acp-types/src/meta_harness.rs`。运行时不得另建平行清单。

所有段落由 middleware 持有，是否装配持有者决定段落可见性；channel 已退役，不保留无持有者的兼容段或独立 feature gate。
新增能力时必须同时确定：段落所有者、session-local 开关、工具/route/event/TUI 暴露面
以及 SubAgent/Workflow 继承语义。

## 3. 构建与冻结

```mermaid
flowchart LR
    CFG[settings + MetaHarness] --> FREEZE[session/new freeze]
    SECTIONS[middleware-owned sections] --> FREEZE
    ENV[cwd + date + language] --> FREEZE
    FREEZE --> BASE[frozen base prompt]
    CHAIN[current middleware chain] --> CONTRIB[request-time contributions]
    BASE --> COMBINE[combine_system_prompt_with_dynamic]
    CONTRIB --> COMBINE
    COMBINE --> REQUEST[ModelRequest]
```

冻结 snapshot 是 write-once owner state。legacy thread 缺 snapshot 时只能按
ARC-FROZEN-001 的 CAS 回填路径恢复；未知版本、损坏数据或存储错误必须 fail closed。
fork 继承 source snapshot 的精确字节，不能以当前磁盘内容重新生成。

request-time contribution provider 返回 owned `String`，不得持锁跨越模型 await。空
contribution 必须保持 base prompt 字节不变。贡献顺序按 production chain 顺序，任何
依赖 `HashMap` 迭代生成 prompt 的实现都违反 ARC-SERIAL-001。收集器
（`MiddlewareChain::collect_prompt_contributions`）是分隔符与准入校验的唯一权威：
非空贡献以空行连接，含 reserved boundary token 的贡献返回带来源的错误，调用方
不补分隔符、不手工复制组合算法、不静默剥离内容。

## 4. 缓存边界 transport

`peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 是跨 `String` handoff 保留
cached/uncached seam 的唯一控制字。`combine_system_prompt_with_dynamic` 将 request-time
contribution 放在边界之后；provider 在构造 wire request 前必须消费控制字，wire 上
不得泄漏。

- `Cached` 段只接收纯静态模板：不得含任何模板占位符（构造期显式失败）；段落
  id 与 `(zone, order)` 必须唯一，冲突在构造期显式失败并指出来源；
- MetaHarness 段落覆盖在冻结准入点校验（单段 / 总字节预算、reserved 控制字、
  未知 `{{name}}` 占位符、空覆盖）：非法覆盖拒绝应用并保留内置段，结构化诊断
  按段落 + 错误类别记录；旧 snapshot 正文不自动改写，重渲染时同样按“拒绝应用、
  保留内置”降级；字面花括号用 `\{{` / `\}}` 转义；
- 支持显式 cache breakpoint 的 adapter 只在恰有一个控制字时拆分静态/动态 system
  block；重复控制字全部剥离并对显式缓存 fail closed；
- 不支持显式 breakpoint 的 adapter 只做字节守恒剥离，不宣称控制 provider 的隐式
  缓存；
- 无控制字输入是否采用 legacy fallback，由具体 adapter 契约决定。

Anthropic 当前使用显式 system block cache seam；OpenAI-compatible 保留 system 字节
顺序并剥离控制字。详细 provider 行为见 [Model Adapter 设计](model-adapters.md)。

## 5. 能力关闭与安全边界

关闭能力必须覆盖 direct tools、deferred index/resolver、slash route、ACP/TUI 投影、
prompt/examples 以及 SubAgent/Workflow 继承，不能只删除提示词。审批与提问是两项独立
能力：`PermissionMiddleware` 持有审批语义，`HumanInTheLoopMiddleware` 持有
`AskUserQuestion`；关闭其中一项不得改变另一项。

覆盖安全相关段落是显式的强能力，但不能改变运行时授权事实。模型文本中的
“always/never”不能替代 policy、HITL、tool view 或 transport 的代码级检查。

## 6. 验证

- `cargo test -p peri-acp --test prompt_cache_boundary`
- `cargo test -p peri-acp --lib prompt`
- `cargo test -p peri-model --lib -- system_cache`
- `cargo test -p peri-middlewares --lib assembly_test`
- `cargo test -p peri-middlewares --lib frozen_claude_md`

变更段落所有权或关闭面时，还要运行 capability presence/absence 矩阵，并按
DOC-UPDATE-001 同步对应标准与模块路由。
