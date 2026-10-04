# 时间模块边界实施

状态：实施中。权威目标见[时间能力边界](../../docs/design/time-runtime.md)；外部库支持
资料见[时间平台参考](../../docs/reference/time-platform-research.md)。`911859b9` 在开发
分支提供了局部日历格式化及 Session Store 等待入口，其 Emscripten timer 路径已有
WASM 验证。

## 验收目标

- 建立低层 `peri-time` 接口与目标依赖，时间实现不进入 `peri-acp-types`；明确 UTC
  时间戳、部署日历日期及进程内单调等待的不同语义。
- 将生产行为中直接读取时钟或等待的调用点按所属模块接入；测试夹具的防挂死计时器
  不计入生产迁移。迁移不得改变原有预算、业务错误分类、取消和关闭所有权。
- 保留并验证原生 Tokio 与 Emscripten JS timer 后端的共同合同；WASM 宿主结果按
  Node、Bun、本地 `workerd` 分别记录，托管 Workers 另行验收。
- 更新受影响的 standards、模块指引、code-index 和测试路由；通过目标 crate 的
  行为测试与目标平台编译／宿主回环后，才把设计状态改为现行设计。

## 当前事实与剩余验收

`peri-time` 已提供墙钟、部署日历、单调时间、等待与周期 tick；生产直接读取当前
时间和 Tokio 计时的路径已迁入该边界。`ThreadMeta`／`ThreadGoal` 的时间字段由
调用方显式传入 `SystemTime`，`peri-acp-types` 不依赖时间模块；UUID v7 的内部时钟
仍属于身份生成语义。原生 UTC 时间戳沿用原 RFC 3339 的精度与时区形状。

当前原生证据：`check --locked --workspace`、`cargo fmt --all --check`、
`git diff --check` 通过；`peri-time` 单测 7/7，`peri-acp-types` 单测 518/518，
各消费 crate 的目标行为测试见本次交付记录。`911859b9` 的 WASM 验证只证明该提交
当时的实现，不直接证明新的 timer。新实现已有 Emscripten 目标编译与
Node／Bun／本地 `workerd` 阶段性 smoke，入口见
`peri-time/tests/emscripten-smoke/`。按当前任务顺序暂停继续扩大 WASM 验证；
需在架构和原生验收之后，复核完整依赖图、宿主环境及正式 WASM 验收。

现有公开 `chrono::DateTime` 契约的替换范围尚需按协议和持久化兼容义务逐项确定。
独立的基线测试失败不改变这些验收条件。
