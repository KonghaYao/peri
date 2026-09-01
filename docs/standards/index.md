# Standards 索引

本目录是工程规则的单一事实源；按任务读取，不默认整目录加载。

## 信息优先级

1. 代码与契约测试
2. `docs/standards/`
3. 模块 `CLAUDE.md`
4. `docs/design/`
5. active spec
6. history

冲突时服从更高优先级；代码变更与规则不一致时，先定位契约，再同步规则或代码。

## 路由

| 任务 | 读取 |
| --- | --- |
| 跨模块边界、事件、Prompt、工具、中间件、安全 | [architecture-contracts.md](architecture-contracts.md) |
| Rust 实现 | [rust.md](rust.md) |
| `peri-tui` 界面与交互 | [tui.md](tui.md) 与 `peri-tui/CLAUDE.md` |
| `CLAUDE.md` 维护 | [documentation.md](documentation.md) |
| Git 分支创建、upstream、历史整理与 push 安全 | [git.md](git.md) |
| 测试（根 workspace、submodule、独立/side project 的范围与命令） | [testing.md](testing.md) |
| 权威设计与参考资料的生命周期 | [documentation.md](documentation.md) |

## 规则

### STD-INDEX-001

- **Scope**：所有工程任务。
- **Rule**：先按上表读取所需规则；测试规范只路由到 `docs/standards/testing.md`，不在本目录复制。
- **Verify**：`test -f docs/standards/testing.md && git diff --check`

### STD-INDEX-002

- **Scope**：规则与实现冲突。
- **Rule**：以代码和契约测试为准；在同一变更中修正过时文档，不能以文档覆盖已验证行为。
- **Verify**：检查对应 crate 的测试入口；无自动测试时，人工核对实现与本索引优先级。
