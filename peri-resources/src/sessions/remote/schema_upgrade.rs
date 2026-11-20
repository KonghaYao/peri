//! 压缩后的远端单条迁移：压缩前的存储契约（`v2`，版本 10|11）→ 当前契约（`v6`，版本 12）；
//! 上一代当前形状（`v5`，版本 11）走只补派生对象的增量批次。
//!
//! 远端与本地共用同一份搬运**规则**（`sessions::storage_v2_plan`）与同一份目标 DDL
//! （`sessions::canonical`），差别只在执行器：远端把整段迁移作为**一次受管批次**下发。
//! 这里因此不重写迁移语义，只补两样远端特有的东西：
//!
//! - **守卫**：形状快照、逐对象定义、逐行会话与绑定核对插在批次最前，任何一条不成立都让
//!   批次以唯一键冲突失败——读到形状与提交时形状必须一致，迁移不作用在漂移过的库上。
//!   本机在事务内读回判定，远端没有事务内读回，只能把同一条规则换成本批次的载体
//!   （`INSERT ... SELECT 0 WHERE (<核对>) <> 0`）。另有一条**结果守卫**
//!   （[`statements::GUARD_MESSAGES_COUNT_SQL`]）例外地放在重建链之后：它核对的不是输入形状，而是
//!   「搬完之后历史还在不在」，前置守卫拦不住这个失败模式。
//! - **版本与契约推进**：作为批次最后一条，把 `(v2, 10|11)` 一次推进到
//!   [`super::schema::STORE_CONTRACT`] 的当前代数——形状变了，代数跟着变，旧构建凭契约标签拒绝。
//!
//! 读分两批：形状一批（对象定义与列清单，任何库都读得动），有会话表时再读一批搬运输入。
//! 空库（v2 契约但还没有 canonical 表）是同一路径的退化：没有可搬运的行，直接建当前形状。
//! 两批之间没有事务，漂移交给批次里的守卫拦下。
//!
//! 批次的收尾顺序是固定的：**整份 canonical 索引集**（`CREATE_INDEXES`，`IF NOT EXISTS`
//! 幂等）一次性重放，然后才是版本与契约推进——两个被重建的表（`threads` /
//! `session_bindings`）随 `DROP TABLE` 丢掉了自己的索引，逐表补清单会漏掉「表被重建而索引
//! 清单没跟上」的漂移；末条是版本推进，`apply_schema_upgrade` 按「末条影响 1 行」确认升级
//! 真的落了地（见 `RemoteStore::apply_schema_upgrade`）。
//!
//! 打开路径上的形状探测（`schema::shape_probe`）与这里**各自**读形状：探测只决定这次打开
//! 认不认识这个库，迁移则按自己读到的事实构造并守护批次，两者不互相授权。

mod dispatch;
mod input;
mod plan;
mod statements;

pub(super) use dispatch::upgrade;

#[cfg(test)]
pub(super) use input::read_input;
#[cfg(test)]
pub(super) use plan::{additive_upgrade_plan, swarm_addition_plan, upgrade_plan, v15_cleanup_plan};
#[cfg(test)]
pub(super) use statements::{
    GUARD_BINDING_COUNT_SQL, GUARD_MESSAGES_COUNT_SQL, GUARD_META_SQL,
    GUARD_REGISTRATION_COUNT_SQL, GUARD_REGISTRATION_ROW_SQL, GUARD_UNASSIGNED_THREADS_SQL,
};
