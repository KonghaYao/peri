# 核心改动：Session ID 恢复、移除文件锁与机器 env 分区

状态：核心实现与文档已同步；保留待验收工作，不据此宣称全量验证通过。

权威设计：[Session ID 恢复与机器环境分区](../../docs/design/session-id-environment.md)。

已实施：持久机器 UUID/override、canonical env 表与本机/远端回填、父子继承和 Environment scope、按 ID 恢复、跨 env 执行只读、删除 session sidecar 锁与 TUI dirty/reset 交互；执行句柄、事务与资源收尾保留。

## 代码与验证证据

本次定向验证已通过资源门面 34 项、工作区/运行生命周期 36 项、机器身份 3 项、迁移/恢复分区 3 项、远端 SQL 形状 16 项、撤销语句 1 项、TUI 恢复 3 项、caps 11 项、ACP 恢复 6 项、legacy 历史恢复 6 项；`peri-acp-types` doc tests 为 1 passed / 2 ignored。相关包编译、格式及依赖边界检查通过；未运行全 workspace 或真实多机器远端 E2E，不将定向通过扩展为全量验收。

本次文档检查：`git diff --check`、六份文档的本地链接与表格行结构检查通过；resources 索引的旧 OS 锁与 `SessionFacts { binding, bound, root }` 描述已清除。

| 行为 | 事实源 / 已有回归入口 |
| --- | --- |
| 机器身份与原子发布 | `peri-resources/src/sessions/machine.rs`、`machine_test.rs`（持久读取、竞争创建、损坏值拒绝） |
| schema 10 幂等回填、路径/dirty/多实例恢复、scope 与 child 继承 | `peri-resources/src/sessions/sqlite_store/session_id_environment_test.rs` |
| 旧远端未知归属 | `peri-resources/src/sessions/remote/session_data.rs`（回填 `legacy:{store_id}`）、`canonical.rs`（共用 SQL）；不等同于真实远端验收 |
| ACP 恢复与执行准入 | `peri-acp/src/host/requests/session_restore.rs`、`host/workspace.rs`；`peri-resources/src/sessions/resources.rs` |
| TUI 不确认、不 reset，保留只读投影 | `peri-tui/src/acp_client/client/recovery_test.rs`：`load_by_id_has_no_recovery_popup_or_reset_request`、`unavailable_environment_restores_history_without_confirmation`、`dirty_session_restores_history_without_recovery_popup_or_reset_request` |
| 恢复能力关闭 | `peri-acp-types/src/peri_caps.rs`；保留 wire 键但置 false，客户端声明 true 不再启用 |

## 未完成项

- [ ] 复核变更后的相关 resources / ACP / TUI 契约测试结果与失败范围，补齐已报告测试未覆盖的热/冷按 ID 恢复、父子树、取消及关闭跨层验收；不把早先 resources 结果当成新增生产路径的验证。
- [ ] 验收旧远端幂等回填、已有 env 不覆盖与同路径双 env 列表隔离；确认真实远端 adapter 与 schema 10 契约一致。
- [ ] 验收跨 env 和保存 cwd 不可用时可读取历史且执行被拒绝，包括先判 env、仅可执行才 legacy 冻结/接纳，MCP/LSP/后台资源不产生本机副作用；补充只读跳过 frozen 与可执行路径缺失/损坏 frozen 的边界。
- [ ] 明确多实例同时续写的数据冲突与工具副作用支持范围；删除文件锁不构成单 owner、全局串行或全面并发安全保证，不未经裁决重新引入恢复认领锁。
- [ ] 补齐身份初始化的 override、权限失败、hardlink 不支持与崩溃边界验证；核对 Unix 新文件 `0600`，不把它扩展成目录/既有文件/非 Unix 权限保证。

## 限制与授权边界

- 本机旧库回填按当前安装认领，不自动识别复制/导入来源；远端未知归属用 `legacy:{store_id}`，不得冒充当前机器或因打开而自动迁移。
- hardlink 原子发布只保护身份文件创建；父目录未 fsync、临时文件仅尽力清理、已有文件权限/符号链接未额外校验，仍需平台与故障验收。
- Session ID 与 machine ID 不是安全凭证，Environment scope 不是多用户授权隔离；数据库/服务访问控制独立。
