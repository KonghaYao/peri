# 时间库与 WASM 平台资料

**性质：非权威生态参考。** 项目时间边界及目标语义以[时间运行时设计](../design/time-runtime.md)为准。本页记录上游资料能证明的范围；仓库当前 WASM 分支的运行验证另由项目测试记录证明。资料检索于 2026-10-04；依赖版本与 `Cargo.lock` 对照：Chrono 0.4.45、`time` 0.3.55、Tokio 1.53.1。

## 目标必须写完整

`wasm32-unknown-emscripten` 与 `wasm32-unknown-unknown` 的标准库和宿主集成不同。官方 [wasm-bindgen Emscripten 指南](https://wasm-bindgen.github.io/wasm-bindgen/reference/emscripten.html)明确说 Emscripten 提供 libc、POSIX 风格 API 和 JavaScript 运行时，`std::time` 可用；wasm-bindgen 对该目标的支持仍标为 experimental。指南中的 Tokio 事件循环集成尚未进入 Tokio 正式发布版，需要补丁和 unstable 配置。不能从“支持 WASM”推断支持本项目目标，也不能从 `std::time` 可用推断现有 Tokio 计时器会被该宿主正确驱动。

## 候选能力与证据

| 能力／库 | 上游可确认的事实 | 对本项目目标的结论 |
| --- | --- | --- |
| [Chrono 0.4.45](https://docs.rs/chrono/0.4.45/chrono/) | `clock` 提供 `Local`，`now` 提供系统时间；`wasmbind` 接 JS Date。其 [`Utc::now` 源码](https://docs.rs/chrono/0.4.45/src/chrono/offset/utc.rs.html)对 Emscripten 使用 `std::time::SystemTime`；[`Local` 源码](https://docs.rs/chrono/0.4.45/src/chrono/offset/local/mod.rs.html)的 JS 时区分支排除 Emscripten。 | 适合日历时间表示、解析、格式化；在 Emscripten 上实际使用标准库／目标平台时区路径。它不是异步 sleep 或 timeout 后端。 |
| [`time` 0.3.55](https://docs.rs/time/0.3.55/time/) | `local-offset` 启用系统 UTC offset 获取；`wasm-bindgen` 特性支持 JS Date 转换及从 JS 获取 UTC offset。 | 日历时间候选。公开文档没有证明 `wasm-bindgen` 特性在 Emscripten 上具体选择哪条 offset 路径，须结合锁定版本的源码和目标测试后采纳。它不提供异步计时器。 |
| [Tokio 1.53.1](https://docs.rs/tokio/1.53.1/tokio/#wasm-support) | 官方文档称 WASM 只支持一组受限 feature；`time` 仅在有计时器的平台上工作，否则调用会 panic。[`timeout`](https://docs.rs/tokio/1.53.1/tokio/time/fn.timeout.html)到期会取消被包裹 future；丢弃 timeout future 会取消计时器；长时间不 yield 的 future 可能超过期限才完成。 | 已验证的项目 WASM 分支可继续作为特定宿主事实，但上游通用说明不足以保证 Emscripten 上的当前配置。设计须分别写清宿主、运行时版本及可复现测试。 |
| [`gloo-timers` 0.4.0](https://docs.rs/gloo-timers/0.4.0/gloo_timers/) | 对 JS `setTimeout` / `setInterval` 的封装。`TimeoutFuture` 丢弃会取消；[源码](https://docs.rs/gloo-timers/0.4.0/src/gloo_timers/callback.rs.html)通过 `clearTimeout` 清理。其 `sleep(Duration)` 在毫秒数无法转换为 `u32` 时 panic；底层将 `u32` 转成 `i32` 传给 JS。 | 可作为 JS 计时器实现的参考，不能仅凭 crate 的“Web”支持宣称已验证 Emscripten 目标或所有宿主。若采用，必须定义超长时长的拒绝／分段策略，并运行取消和到期测试。 |
| [`web-time` 1.1.0](https://docs.rs/web-time/1.1.0/web_time/#target) | 官方文档明确只针对 `wasm32-unknown-unknown` 的浏览器，**不支持 Emscripten**；提供 `Instant`、`SystemTime`，不是异步计时器。 | 排除为 Emscripten 时间底层候选。 |

## 设计时需区分的语义

- 墙钟时间可用于持久化时间戳、日期与时区格式化；期限和耗时应使用单调时钟或计时器，避免系统时间调整改变等待预算。[Chrono 官方文档](https://docs.rs/chrono/0.4.45/chrono/)也区分 `DateTime`、`SystemTime` 与 `Instant` 的用途。
- 定时器的取消必须同时覆盖“到期后取消业务 future”和“调用方丢弃整个等待 future”。Tokio 与 gloo 的公开契约分别说明了这两种行为，但项目 adapter 应用实际宿主测试证明其清理行为。
- `gloo-timers` 的入参单位为整毫秒，存在精度和范围转换；底层库允许的最大值不应悄悄缩短业务期限。对零时长、亚毫秒、超长预算分别规定行为。
- `wasm-bindgen` 指南的 Emscripten Tokio 集成是未发布的实验能力，不应把它当作项目当前锁定 Tokio 版本的稳定支持承诺。
