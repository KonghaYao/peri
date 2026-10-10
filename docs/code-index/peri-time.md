# peri-time 代码索引

> 时间能力的目标语义见[权威设计](../design/time-runtime.md)；当前实现与验证以源码
> 和目标测试为准。

| 我想做什么 | 主文件 | 入口 | 关键语义 |
| --- | --- | --- | --- |
| 读取墙钟或单调时钟 | `peri-time/src/lib.rs` | `now_wall`、`monotonic_now` | `SystemTime` 可转换为跨实例时间戳；`Instant` 只在进程内用于耗时和 deadline |
| 解释日历日期或 UTC 时间戳 | `peri-time/src/calendar.rs` | `CalendarConvention`、`calendar_date`、`format_utc_rfc3339` | 原生本机日期、Emscripten UTC 日期；对外 UTC 时间戳保留带时区 RFC 3339 形状 |
| 更换计时器底层 | `peri-time/src/timer.rs` | `sleep`、`timeout`、`timeout_at`、`interval` | 原生 Tokio，Emscripten JS timer；取消和到期清理底层 timer；长等待不截短预算 |
| 验证 Emscripten 宿主计时 | `peri-time/tests/emscripten-smoke/` | `run.sh` | 独立 Cargo fixture 在 Node 运行已编译 WASM，检查完成、到期、interval、亚毫秒/长等待与取消清理；不进入普通 workspace 测试 |
| 改会话 Store 等待 | `peri-resources/src/sessions/remote/connection.rs`、`resources/deployment.rs` | `within_budget`、`close` | 计时结果由 Resources 映射为远端结果不确定、关闭 Timeout 等领域结论 |

时间调用点的业务预算与失败分类留在所属模块，不由本 crate 定义。WASM 宿主事实
与外部库边界见[参考资料](../reference/time-platform-research.md)。
