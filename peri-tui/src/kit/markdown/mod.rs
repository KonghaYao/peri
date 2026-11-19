//! Markdown 渲染已下沉到 `peri-tui-components`；此模块保留应用内路径与类型出口。
//!
//! 解析、段落转换、表格/图片/OpenUI 布局都在组件库中实现，应用只消费结果。

pub use peri_tui_components::markdown::*;
