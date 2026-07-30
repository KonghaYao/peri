# Rust 仓库规则

仅记录本仓库差异；通用 Rust 风格从代码邻域继承。

### RUST-EDITION-001

- **Scope**：Cargo manifest。
- **Rule**：workspace 默认使用 Rust 2021；`peri-tui` 与 `peri-theme` 明确使用 Rust 2024。新增 crate 或修改 edition 时保持该划分，除非同时完成兼容性验证。
- **Verify**：`cargo metadata --no-deps --format-version 1 >/dev/null`；检查根 `Cargo.toml` 与 crate `Cargo.toml` 的 `edition`。

### RUST-ERROR-001

- **Scope**：Rust 错误边界。
- **Rule**：库 crate 用 `thiserror` 的结构化错误和 crate Result 别名；应用 crate（`peri-tui`、`peri-acp`）用 `anyhow::Result` 组织调用链。错误文本不得泄露 secret。
- **Verify**：`cargo check -p <crate>`；人工检查新增 public error 与调用边界。

### RUST-TRACE-001

- **Scope**：运行时诊断。
- **Rule**：库和运行时诊断使用 `tracing`，禁止新增 `println!`、`eprintln!`、`dbg!`；应用启动边界已有的用户可见 stderr 输出按邻近模式处理。字段应结构化且不包含敏感信息。
- **Verify**：`rg 'println!|eprintln!|dbg!' <changed-rust-path>`；人工检查新增 `tracing` 字段。

### RUST-ASYNC-001

- **Scope**：async Rust。
- **Rule**：不得跨 `.await` 持有不兼容的标准库锁 guard；需要跨 await 的共享读写锁使用 `parking_lot::RwLock`。阻塞 I/O 不得直接堵塞 async runtime。
- **Verify**：`cargo check -p <crate>`；人工审阅 `.await` 前后的 guard 生命周期及阻塞调用。

### RUST-TEXT-001

- **Scope**：用户文本、终端布局与坐标。
- **Rule**：Unicode/CJK 截断按字符边界处理；终端显示宽度用 `unicode_width`，不用字节长度；`u16` 坐标运算使用 `saturating_add`/`saturating_sub` 或等价边界处理。
- **Verify**：`cargo test -p peri-tui --lib`；人工检查新增截断、列宽和坐标计算。

### RUST-DOC-001

- **Scope**：含 Rust 文档示例的 crate。
- **Rule**：修改可编译 doc example 或 public API 文档时，显式运行 doc tests；`build`、`check`、`clippy` 不能替代该验证。
- **Verify**：日常运行 `cargo test -p <crate> --doc`；仅在跨 workspace 文档契约或发布验证时运行 `cargo test --workspace --doc`。
