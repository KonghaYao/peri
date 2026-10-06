# P0：TUI 架构优化——增量渲染、缓存预算与客户端职责收口

**状态**：Open；任务范围已定稿，实施与行为/性能验收未开始。

**优先级**：P0（2026-10-06 用户明确指定）。这是工作优先级，不表示已证明 TUI 是现场 CPU/RSS 事故主因。

**裁决来源**：主 agent 源码审查 + 本轮 `gpt-6-astra` 独立复核；已采纳 Astra 的范围、复杂度修正与验收建议。

**任务定位**：本 issue 是下述架构优化的唯一实施与验收入口；不以拆文件、搬 crate 或新增通用框架代替优化。

## 1. 目标与证据边界

让 TUI 的稳态和局部变化计算不再随全部历史扫描，让可重建渲染数据有明确内存预算，
收敛主消息/详情的重复缓存规则，并明确客户端与宿主资源所有者的职责。

本轮只读源码复核，未运行构建、测试、perf 或 heap 采样。可以确认现行计算、复制和
依赖路径，不能确认其现场 CPU 占比、内存放大倍数、泄漏或收益百分比。
同进程 backend WorkState 的资源问题继续由[进程 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md)管理。

已经存在的 50ms 流式合帧、lazy projection、Arc 正文、增量 Markdown、slot-local
wrap map、后台流节流和详情持久缓存必须保留；不将旧审计中的已修问题重新登记。
现行设计以 [TUI 数据流](../../docs/design/tui-acp-data-flow.md)和
[流式 Markdown 性能设计](../../docs/design/tui-streaming-markdown-performance.md)为准。
本 issue 中的新结构是待实施目标，不提前改写现行设计为已实现。

## 2. Astra 裁决与现行证据

下列位置为本轮审查快照；实施时按符号核实，不依赖固定行号。

| 工作包 | 现行事实与入口 | 定稿裁决 |
| --- | --- | --- |
| A：增量索引与动画分离 | `peri-tui/src/kit/message_area/mod.rs:269` 遍历 VM 读取预存 hash/动画周期，`:298` 检测失效，`:376` 组装所有 slot 索引，`:399` 物化起点数组；`message_area/selection.rs` 的 `SlotLines::composite` 还遍历稳定分片 | 第一主线。索引组装成本包含 O(N + ΣK)，不是每帧全文哈希；三组扫描缓冲已复用，reasoning 已按秒刷新。收敛失效契约、持久索引及活动动画集合 |
| B：共享缓存与按需取行 | `message_area/vm_cache.rs` 的 `VmCacheSlot` 与 `panels/subagent_detail_cache.rs` 的 `DetailRenderCache::render` 分别管理内容、宽度、主题、语言和动画失效；后者缓存命中后仍聚合克隆全部行 | 第二主线。只抽取两个真实调用方共用的缓存机制，保留 surface 差异；不重报“详情每帧从零解析” |
| C：重型缓存预算 | 主消息 cache 数量与全部历史 slot 对齐；`VmCacheSlot` 持有 Lines、wrap map、Markdown cache 等派生数据 | 依赖 A/B。保留 canonical 历史与准确轻量高度/身份索引，对可重建重型数据实施预算、淘汰和恢复；仅加 LRU 不算完成 |
| D：终态与 Plugin 操作收口 | `acp_types/current_turn/streaming.rs` 的 `CurrentTurn::end_tool` 与 `bg_task_live.rs` 的 `handle_bg_tool_ended` 重复完成转换；`panels/plugin/panel_handler.rs` 键鼠分支重复请求 `plugin/toggle` 和独立本地持久化，均丢弃结果 | 两个小范围工作。终态规则归现有 accumulator；Plugin 统一操作、持久化权威与结果反馈。未证明写同一文件，不断言同文件双写竞态 |
| E：客户端/宿主职责收口 | `service_snapshot.rs` 的 `SnapshotSource` 仍持 Cron/MCP 具体句柄；`session_services::query` 已经通过 ACP 查询活动会话 `plugin/list`、`mcp/list`，本地 pool 投影与会话投影仍并存 | 有边界的后续阶段。复用既有协议，明确无会话数据来源与能力缺席；否决先搬 crate，独立 crate 不是本 issue 必达门槛 |

N 为历史 slot 数，K 为各 slot 稳定分片数。读取预存 hash、Arc 共享正文与
索引维护分别计量，不能把其中一个的优化推导成其他路径 O(1)。

## 3. 职责、依赖方向与数据流目标

```text
ACP notification → 既有解码/归约与 publication
                 → 明确的内容/布局/样式/动画变化
                 → Transcript 持久索引 + 共享 entry 渲染缓存
                 → viewport / selection / hit / copy 使用一致布局版本

TUI 用户操作 → ACP client → 宿主能力与持久化权威
                          → 明确结果/通知 → 会话投影与错误反馈
```

- **归约与发布**：继续拥有事件次序、session/reset 隔离与不可变发布状态；不将 Agent 执行权移到 TUI。
- **Transcript 模块**：封装历史身份、增量高度索引和布局版本；局部变化更新索引，查询不要求调用方扫描历史。具体数据结构由确定性计量选择，不预设树或新增 crate。
- **Entry 渲染缓存模块**：封装两处实际共用的失效和派生缓存规则；内容、布局、样式、动画各自失效，surface 特有的交互与复制按钮不强行共用。
- **预算管理**：只管理可重建派生数据，不截断会话、工具结果或用户草稿；准确高度索引不得依赖全部重型缓存同时驻留。
- **宿主与客户端**：客户端消费能力和结果，具体 MCP/Cron/存储资源及关闭权留在宿主装配侧。可同进程部署，不强制远程化。

新抽象必须增加 depth：删除该模块会使复杂度重新散落到真实调用方；
不得引入浅转发模块、全局通用 action framework、全 atoms 改造或假想可替换 trait。

## 4. 实施顺序与阶段交付

### 0：固定基线与计量口径

- 记录源码及已有 dirty 差异、Cargo.lock、release profile、binary hash/UUID、终端尺寸、启动参数与负载；保留可复跑的 fixture 和采样命令。
- 扩展既有 perf counters，分别记录失效扫描、索引访问/重建、解析、wrap、行深克隆、分配、缓存 retained bytes、命中与淘汰；不得记录用户正文或秘密。
- 明确缓存预算值、所有权计量、超大单条消息及复制暂态策略；预算和例外在实施记录中写清后再交付 C。
- 首先核对实现实际成本，不把旧 binary 采样当当前基线；不将 counter 改善替代 CPU/heap 证据。

### A：失效契约、持久索引与动画分离

- 区分内容、布局、样式与动画变化；覆盖追加、折叠、归档、reset、rewind、同长度替换和 slot 变体变化。
- 无变化帧复用索引；局部内容更新维护受影响范围，活动动画只更新可见装饰，不重解析/重折行未变正文。
- 不能只将逐帧全历史扫描搬到每次 publication 就宣布完成；冷加载与全局失效的允许成本须单列。

### B：共享缓存机制与详情按需访问

- 主消息与详情共用已验证重复的失效/缓存机制，保留宽度、主题、语言、occurrence 与 surface 的正确区分。
- 详情不再每帧深克隆全组历史行；采用按需行访问及可见范围物化。宽度/主题变化尽可能复用仍有效解析结果。
- 接口和行为测试围绕真实调用方验证，不建立通用组件体系或视觉快照框架。

### C：有预算的重型派生缓存

- 在轻量索引准确的基础上，实施可见区、预取与有界冷缓存策略；淘汰后按需恢复，不删除 canonical 内容。
- 覆盖跨冷区拖选/复制、向上浏览、锚点、resize、图片及详情切换；全选复制不得通过永久保留全部选区缓存绕过预算。
- 检查旧 frame、handler 与 Arc 引用是否阻止回收；超大可见条目和瞬时工作集不能伪装成受预算约束。

### D：两个局部领域操作收口

- 工具卡幂等完成转换归 `ToolCardAccumulator`；共用查找/更新规则仅限真实重复，主回合与后台容器的排序、缓存失效及展示差异保留。
- Plugin 键鼠进入同一操作；运行态与宿主持久化明确唯一权威，成功、失败、无客户端、会话切换和迟到结果均结算。
- 失败必须有日志与可见反馈；协议返回与持久化部分失败不能冒充整体成功，不以吞错或无条件空快照降级。

### E：客户端/宿主 seam 收口

- 复用已有活动会话 `plugin/list`、`mcp/list`；只补实际能力缺口，先区分无活跃会话、活动会话、断连和能力不支持。
- 迁出视图路径中的具体 Cron/MCP 句柄与本地写盘职责，宿主启动配置及必要初始化仍由宿主负责。
- 消除同一会话投影的两套权威，不将无会话宿主管理数据伪装为会话数据；所有权迁移保留既有 MCP/transport teardown 顺序。
- seam 稳定后另行评估 crate 提取；本 issue 不承诺仅拆 crate 能降低 RSS 或编译耗时。

## 5. 不可退化的不变量

- canonical chunk 完整、有序接收；保留 50ms 合帧、Immediate/barrier、receiver close flush、终态完整 Markdown oracle 及旧 deadline 失效。50ms 不是所有 publication 的频率上限。
- session/reset、不可变快照、子 Agent occurrence 身份保持；同名或同 agent 的不同运行不得共用错误详情缓存，后台实时详情优先级保持。
- viewport、滚动、选择、语义复制、命中、图片和 interaction 坐标消费一致布局版本；淘汰不改变高度、内容或锚点。
- 保留 Unicode 字符边界/终端宽度、手动浏览与吸底语义，冷缓存不得导致错选、漏字或跳滚。
- 重复 tool end 不覆盖结果或重算冻结时长；迟到 start 不复活停止运行，子工具失败不冒充父 Agent 终态。
- render 不写 atom 或产生通知副作用，hook 顺序稳定；错误暴露，主题与语言遵循既有标准。
- ACP 交互 owner、operation gate、终态和宿主资源关闭权保持，不绕过服务端授权或接管 Agent 生命周期。

## 6. 验收契约

### 确定性复杂度与预算

- 固定视口，递增 N/K；暖态无变化帧的历史失效扫描、完整 prefix 重建、详情历史行深克隆计数为 0。
- 动画 tick 不重解析或重折行未变正文；固定少量内容变化时索引维护不遍历全部历史。采用树索引时更新/查询访问量按 O(log N) 验证；其他结构须证明相应有界访问量。
- 冷加载、resize、全局主题/语言失效允许必要全局工作，单独记录，不与暖态目标混算。
- 连续追加历史时，可淘汰重型缓存按明确预算保持有界；共享 Arc 只计一次，区分 len/capacity、allocator 保留和真实 retained objects。
- 超大可见条目、复制暂态和 canonical 历史单列；缓存预算不是整进程 RSS 上限。淘汰后内容、滚动高度、选择/复制与未淘汰参考一致。

### 行为、操作与生命周期

- 覆盖 reset/rewind、同长度替换、slot 变体、宽度/主题/语言变化、跨分片 Unicode、table/list/image/fence 及 terminal oracle。
- 覆盖 occurrence 切换、主/后台重复终态、迟到 start/end、停止后详情可查与旧布局引用释放。
- Plugin 键鼠产生相同语义请求；成功、服务端失败、断连、无客户端和迟到结果正确反馈。使用临时配置验证 user/project/local 范围，不读写真实配置。
- 用受控 transport 验证能力缺席、旧会话响应拒绝与真实 wire 映射；跨协议变更增加对应 ACP 生命周期测试。
- 按现有 TUI/E2E 规范人工验证键鼠、焦点、滚动、复制和图片。纯逻辑、事件、索引、选择与生命周期测试不等同于新增 render body 截图测试。

### 现场性能与关闭条件

- 同 binary/profile/终端尺寸与相同 fixture 对比：空闲、主流式、多子 Agent、长历史、详情、终态与切换会话；负载分别控制 N、K、正文/推理长度和后台活动量。
- 记录 CPU 时间差分、交互/帧延迟、分配峰值、heap/RSS 及缓存计数；UI 与同进程 backend 分开归因，debug/release 不混比，不预设改善百分比。
- 对声称改善的场景提供可复跑前后证据；若 counter 改善但 CPU/heap 未改善，记录瓶颈解释与剩余工作，不能宣称现场资源问题已解决。
- A–E 的行为、复杂度、预算与协议/所有权契约全部完成，并交付性能结果和必要文档更新后才可关闭；阶段完成、命令启动或局部通过不等于整体完成。

实施按模块先运行定向测试，确认非零用例数和退出状态，最终执行：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- perf_probe_push_view_models --ignored --nocapture
git diff --check
```

现有 perf probe 只测 publication，不覆盖上述所有帧、预算和现场验收；须补相应计量。
协议修改按[测试标准](../../docs/standards/testing.md)补对应 crate/wire 验证；E2E 先读其模块指引。
源码/测试修改遵守 `STD-SIZE-001`，不得靠机械拆文件规避职责设计。

## 7. 唯一实施归属与非目标

| 事项 | 唯一实施/验收入口 |
| --- | --- |
| 原流式 issue F4 重型历史缓存预算、F5 增量索引/动画分离、F10 共享缓存/详情聚合余项 | **本 issue A–C**；原 issue 保留已实施记录和原现场验收，不保留迁出项的第二套实施清单 |
| 工具 accumulator 完成规则、Plugin 操作权威、限定的宿主/客户端职责收口 | **本 issue D–E**；既有 [kit 提取论证](2026-10-05-peri-tui-kit-extraction-design.md)保留为背景，不并行推动机械 crate 提取 |
| 原 F1/F2/F3/F6/F7/F8/F9、F10 测试 oracle 清理及原修复现场验收 | [流式冗余 issue](2026-10-06-tui-streaming-render-redundancy.md)；不自动迁入 |
| reasoning 全量折行、其他稳定分片搬运、static reset root、后台保留/整表 clone、预览、恢复预算、poll/task owner/队列背压等 | [既有 Astra 审计](2026-10-06-tui-perf-astra-audit.md)及其 CPU/M/R 路由；不批量升 P0，不重复认领收益 |
| backend WorkState/进程事故、Agent 执行/恢复故障 | 原[进程 P0](2026-10-06-p0-dev-peri-high-cpu-memory.md)与[执行故障 issue](2026-10-06-tui-execution-failures.md) |

不重写 ACP/session/Agent，不删除后台能力，不为控内存截断历史，不批量改所有 atoms，
不引入兼容 shim/双实现，不强制独立进程或远程部署。引用其他问题作共同依赖，不等于承接其全部实施范围。

## 8. 文档路由与本轮交付

实施影响结构时同步 `peri-tui/CLAUDE.md`、`docs/code-index/peri-tui.md` 与
`docs/design/tui-streaming-markdown-performance.md`；协议/能力改变再核对 TUI 数据流设计和
architecture contracts。当前仅任务定稿，现行设计继续如实描述每帧索引，不提前修改规范。
阶段进度与性能记录留在本 issue，关闭前按 `DOC-HISTORY-001` 收口，不复制到权威设计。

本轮交付只包含 P0 issue、关联去重路由与文档检查；未修改运行代码，未执行 Rust 测试、
构建或性能采样，不把 Astra 静态复核表述为实验验证。
