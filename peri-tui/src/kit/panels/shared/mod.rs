//! 面板公共交互机制——只承载已有面板中真实重复的绘制与命中逻辑。
//!
//! 设计约束（见 `docs/design/tui-panel-architecture.md` §公共组件边界）：
//!
//! - **绘制与命中同源**：可见范围、滚动起点与鼠标命中由同一份布局结果推导，
//!   面板不再各自维护 `VISIBLE_ITEMS` 常量。
//! - **只表达交互意图**：公共列表产生 `Navigate / Activate / Back` 语义；
//!   Enter 关闭详情、取消任务、终止 run 等业务含义由各功能控制器保留。
//! - **业务身份由功能层提供**：公共组件不得用列表 index、显示标签替代业务 key。
//! - 不建设 JSON 驱动的页面解释器，也不建立覆盖所有面板的巨型 action 枚举。

pub(crate) mod context_window;
pub(crate) mod list;
