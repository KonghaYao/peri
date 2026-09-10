# image-spike 代码索引

> 独立实验项目，根 workspace 不包含本目录。更新：2026-09-10。
> 依据：`side-projects/image-spike/Cargo.toml` 和本地源码，无 crate 级 guide。

## 架构速览

验证 ratatui-image Kitty 协议在 TestBackend buffer 中的输出，不实现应用生命周期。
入口、两个行为测试与手工转义序列示例职责清晰，保留现有结构。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 查看实验说明 | `side-projects/image-spike/src/main.rs` | `main` | 输出实验用途 |
| 验证首帧协议和占位符 | `side-projects/image-spike/tests/kitty_transmit.rs` | `kitty_escape_visible_in_test_backend_buffer` | 检查 transmit 与每行占位符进入 TestBackend buffer |
| 验证同一图片重复渲染 | `side-projects/image-spike/tests/kitty_transmit.rs` | `transmit_happens_only_on_first_frame` | 第二帧保留占位符且不再次 transmit |
| 手工查看转义序列 | `side-projects/image-spike/examples/dump_escape.rs` | `main` | 构造协议并打印渲染 buffer 的转义内容 |

## 验证边界

在本目录运行 `cargo test` 和 `cargo clippy --all-targets -- -D warnings`。
TestBackend 不解释终端协议；这些测试不能替代真实 Kitty/终端兼容性验证。
