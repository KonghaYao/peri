//! builtin `workspace` 实例的**默认订阅**（Git Watch 下沉的宿主侧落点，D-5）。
//!
//! 只有一条订阅：`workspace://git/ref` 资源。服务端在该资源变化时推送
//! `notifications/resources/updated`，宿主侧的消费（失效缓存 → `resources/read` →
//! canonical `git_watch` SystemReminder）在 `mcp::client::subscription`。
//!
//! **提醒元数据由宿主内置，不从 server 取**（D-5 裁决）：当前没有「server 声明提醒
//! 映射」的协议位，首个真实用例又是内置实例（语义由我们控制），因此
//! `category` / `source` / `kind` / `severity` / `delivery` / `audiences` / `summary`
//! 全部按旧 `GitWatchMiddleware` 的契约逐字段写死在消费侧，**不新增**配置面
//! （`ResourceReminderProfile` / `resource_reminders` 已被裁决否决；等第二个真实用例
//! 再提取复用型抽象）。
//!
//! 注入规则（`builtin::builtin_default_entry` / `apply_builtin_overlay` 规则 2）：
//! - 实例条目缺失时随默认条目注入；
//! - 条目已存在（用户显式写了 `workspace`）且 `subscriptions` 为空 ⇒ 补默认；
//! - 用户显式写了 `subscriptions`（含空配置）⇒ **优先于默认注入**，空配置经
//!   `initialize` / `reconnect` 的 `!is_empty()` 过滤后退化为「不订阅」。

use peri_acp_types::plugin::McpSubscriptionsConfig;
use peri_mcp_workspace::GIT_REF_RESOURCE_URI;

/// 带默认订阅的 builtin 实例名（注册表 `name`；本文件是唯一持有该字面量的地方）。
const WORKSPACE_INSTANCE: &str = "workspace";

/// `workspace` 实例的默认订阅配置：只订阅 git ref 资源。
pub(crate) fn workspace_default_subscriptions() -> McpSubscriptionsConfig {
    McpSubscriptionsConfig {
        resources: vec![GIT_REF_RESOURCE_URI.to_string()],
        tools_list_changed: false,
        prompts_list_changed: false,
        resources_list_changed: false,
    }
}

/// 实例名 → 默认订阅；无默认订阅的实例返回 `None`（不建立订阅）。
pub(crate) fn default_subscriptions_for(instance: &str) -> Option<McpSubscriptionsConfig> {
    (instance == WORKSPACE_INSTANCE).then(workspace_default_subscriptions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_workspace_has_default_subscriptions() {
        let workspace = default_subscriptions_for("workspace").expect("workspace 必须有默认订阅");
        assert_eq!(workspace.resources, vec![GIT_REF_RESOURCE_URI.to_string()]);
        assert!(!workspace.tools_list_changed);
        assert!(!workspace.prompts_list_changed);
        assert!(!workspace.resources_list_changed);
        assert!(
            !workspace.is_empty(),
            "默认订阅必须非空（空配置会退化为不订阅）"
        );

        for other in ["web", "artifact", "cron", "unknown"] {
            assert!(
                default_subscriptions_for(other).is_none(),
                "{other} 不应有默认订阅"
            );
        }
    }
}
