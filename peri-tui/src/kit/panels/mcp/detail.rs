//! MCP 详情视图的行构造与命中几何（给定 server 摘要与面板宽度 → 渲染行）。
//!
//! 从 `mcp.rs` 拆出：[Why] 组件装配、事件路由与详情布局同处一个文件会越过单文件
//! 上限（STD-SIZE-001）。本模块只做纯推导——不触碰 atom、不发 RPC、不写状态；
//! 渲染、鼠标命中与键盘选择共用同一份布局结果，避免三处偏移各自漂移。

mod tables;

mod detail_view;
pub(super) use detail_view::*;
