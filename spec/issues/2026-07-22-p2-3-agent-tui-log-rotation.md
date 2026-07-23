# P2-3：agent-tui.log 日志轮转

**状态**：Open
**优先级**：低
**类型**：运维改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 工程规范维度

## Problem Statement

`agent-tui.log` 文件持续累积，当前约 1.5GB，无轮转机制。长期运行会导致磁盘空间耗尽，且大文件难以 grep/分析。

## 建议方案

1. 配置 `tracing-appender` 的 `RollingFileAppender`，按大小或时间轮转
2. 保留最近 N 个轮转文件（如最近 5 个）
3. 可选：压缩旧日志（`.log.gz`）

参考配置：
```rust
let file_appender = RollingFileAppender::new(
    Rotation::DAILY,
    "logs",
    "agent-tui.log",
);
```

## 涉及文件

- `peri-tui` 中的 tracing subscriber 初始化代码
- `tracing-appender` 依赖（可能需新增）

## 风险

- **低**：纯运维配置，不影响业务逻辑
