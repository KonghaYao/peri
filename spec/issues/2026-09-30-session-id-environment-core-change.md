# 核心改动：Session ID 恢复、移除文件锁与机器 env 分区

状态：用户已批准方向，待实施；本次只记录契约，没有移除运行时代码中的锁或弹窗。

权威目标：[Session ID 恢复与机器环境分区](../../docs/design/session-id-environment.md)。

## 获批核心改动

- root session 只以 ID 标识，持有 ID 可在可访问的数据库中恢复；不以机器 owner、路径或持锁状态判定归属。
- 父子关系保持不动；根归属由 ID 与父链表达，不按 cwd 推导。
- execution_environment_id 表达机器位置，在数据库中提供 env 分区；不承担用户授权、运行 owner 或锁语义。
- 删除 session 执行文件锁、dirty 恢复认领门槛与相应前端确认弹窗，不替换成分布式 owner/lease。
- 数据库路径仅用于过滤、展示与工具定位；显式 ID 恢复不被目录不匹配阻断。

## 待实施切片

| 范围 | 当前入口 | 目标 |
| --- | --- | --- |
| 本机执行文件锁 | `peri-resources/src/sessions/sqlite_store/execution.rs` | 删除 session sidecar 文件锁及依赖它的准入/持锁写入门槛，保留事务与实际资源收尾 |
| 恢复关系与契约 | `peri-acp-types/src/workspace.rs`、`peri-acp-types/src/session_resources.rs` | 删除作为恢复认领门槛的 lease/dirty 契约，root 按 ID、子关系不变；外部协议兼容按影响评估 |
| ACP 恢复入口 | `peri-acp/src/host/workspace.rs`、`peri-acp/src/host/requests/session_lifecycle.rs` | load/resume 按 ID，不执行目录归属认领与 dirty reset；工具环境装配单独处理 |
| 前端弹窗 | `peri-tui/src/acp_client/client/session.rs`、`peri-tui/src/kit/popups/confirm_popup.rs` | 删除 session dirty 恢复确认、reset_dirty 交互与能力协商；保留无关确认弹窗 |
| env 元数据与查询 | `peri-resources/src/sessions/canonical.rs`、本机/远端 session adapters | 增加稳定机器 env 归属与 scoped 查询，同步 schema/序列化/迁移；不改变 Session ID 主键 |
| 路径条件 | 工作区登记、会话查询与 Agent 环境装配 | 路径过滤与工具定位和会话身份分开，不因当前 cwd 不同拒绝 ID 恢复 |

不要只删除弹窗或跳过一次锁获取：须收敛契约、调用方、数据库 schema、写入门槛与相邻测试，删除过时内部实现而非留下兼容双轨。

## 验收目标

- [ ] 持有 root ID 能恢复，不要求持有文件锁或匹配当前目录；父子树仍正确。
- [ ] 两个 env 下相同路径互不混入列表；明确指定 ID 不因当前 env/cwd 不同隐藏会话。
- [ ] 机器 env ID 重启稳定；旧库有可解释的归属迁移，导入来源不明的数据不盲目认领。
- [ ] 热/冷恢复不弹 dirty/owner 认领确认，不再产生相应 reset_dirty 请求。
- [ ] 运行时没有 session 执行 sidecar 锁，普通事务、幂等写入、取消与资源关闭保持。
- [ ] 原环境不可用时历史仍可恢复；工具实际执行报明确错误，不偷偷执行本机同名路径。
- [ ] 明确并发续写和多实例执行的支持范围；不宣称移除锁后仍有跨机单 owner 保证。

## 风险与待细化

- 持有普通 Session ID 不等于具备安全凭证；数据库/服务的外部访问权限与 env 分类分开。
- 同时恢复同一根会话可能产生多份计算和工具副作用，需明确产品支持范围及数据冲突处理，但不得未经裁决重新加回恢复认领锁。
- 多机器 env 登记属于新的元数据契约，不是更换 SQLite 后端；本机与远端 adapters 必须同步演进。
- 当前实现仍有锁、dirty/lease 和弹窗；具体完成状态以代码与契约测试为准。
