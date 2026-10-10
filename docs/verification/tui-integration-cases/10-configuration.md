# 配置与能力开关

按 [总目录](README.md) 的编号执行。使用隔离配置与会话，避免把宿主启动配置和当前会话数据混为一谈。

1. **P0-44 Plugin 失败与迟到结果不串会话**（[issue](../../../spec/issues/2026-10-06-p0-tui-architecture-optimization.md)）
   1. 在 Plugin 面板发起搜索或操作，依次制造服务端失败与断连。
   2. 切换会话后送达旧结果，再重试当前会话的请求。
   3. 核对失败有明确反馈、旧结果不覆盖新会话；键鼠提交相同语义请求。

## P1 补充场景

1. **P1-04 Beta 开关只对新会话生效**（[issue](../../../spec/issues/2026-10-08-beta-flags-implementation.md)）
   1. 在 Config 面板开启 full-async-tools，保存后保留当前会话并新建会话。
   2. 分别在两个会话调用未显式指定后台模式的 Bash 或 Agent。
   3. 核对旧会话仍用原行为、新会话默认后台；显式 false 仍前台，重启后设置保留。
