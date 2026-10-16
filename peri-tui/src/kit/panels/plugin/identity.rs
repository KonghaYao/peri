use super::{MsEntry, PluginSummary};
use crate::application::operations::OperationScope;

// 与请求的 pluginId + scope 同源；marketplace 为空明确表示本地清单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InstalledIdentity {
    name: String,
    marketplace: String,
    install_scope: Option<String>,
    scope: OperationScope,
}

impl InstalledIdentity {
    pub fn new(plugin: &PluginSummary, scope: OperationScope) -> Self {
        Self {
            name: plugin.name.clone(),
            marketplace: plugin.marketplace.clone(),
            install_scope: plugin.install_scope.clone(),
            scope,
        }
    }

    pub fn plugin_id(&self) -> String {
        if self.marketplace.is_empty() {
            self.name.clone()
        } else {
            format!("{}@{}", self.name, self.marketplace)
        }
    }

    pub fn resolve<'a>(
        &self,
        plugins: &'a [PluginSummary],
        scope: &OperationScope,
    ) -> Option<&'a PluginSummary> {
        if &self.scope != scope {
            return None;
        }
        plugins.iter().find(|plugin| {
            plugin.name == self.name
                && plugin.marketplace == self.marketplace
                && plugin.install_scope == self.install_scope
        })
    }
}

// 当前目录来自 load_known_marketplaces(None)，即宿主 user 目录，不是 project/local。
// 来源与名称共同区分同名市场替换；会话边界禁止把旧详情提交给新会话。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MarketplaceIdentity {
    pub name: String,
    source: peri_middlewares::plugin::MarketplaceSource,
    catalog_scope: &'static str,
    scope: OperationScope,
}

impl MarketplaceIdentity {
    pub fn new(entry: &MsEntry, scope: OperationScope) -> Self {
        Self {
            name: entry.name.clone(),
            source: entry.source.clone(),
            catalog_scope: "user",
            scope,
        }
    }

    pub fn resolve<'a>(
        &self,
        entries: &'a [MsEntry],
        scope: &OperationScope,
    ) -> Option<&'a MsEntry> {
        if &self.scope != scope || self.catalog_scope != "user" {
            return None;
        }
        entries
            .iter()
            .find(|entry| entry.name == self.name && entry.source == self.source)
    }
}

// effect 与事件入口共同收敛失效目标；事件必须消费这一次输入，不能落到新列表项。
pub(super) fn reconcile_details(
    detail: ratatui_kit::prelude::State<Option<InstalledIdentity>>,
    marketplace: ratatui_kit::prelude::State<Option<MarketplaceIdentity>>,
    confirmation: ratatui_kit::prelude::State<Option<MarketplaceIdentity>>,
    confirm_action: ratatui_kit::prelude::State<Option<String>>,
    operation: ratatui_kit::prelude::State<super::operation::OperationState>,
) -> bool {
    let scope = super::current_scope();
    let plugins = crate::kit::atoms::PLUGIN_LIST.state().read().clone();
    let entries = super::data::get_marketplace_cache();
    let missing_plugin = detail
        .read()
        .as_ref()
        .is_some_and(|key| key.resolve(&plugins, &scope).is_none());
    let missing_marketplace = marketplace
        .read()
        .as_ref()
        .is_some_and(|key| key.resolve(&entries, &scope).is_none());
    let missing_confirmation = confirmation
        .read()
        .as_ref()
        .is_some_and(|key| key.resolve(&entries, &scope).is_none());
    if missing_plugin || missing_marketplace || missing_confirmation {
        if missing_plugin {
            *detail.write() = None;
        }
        if missing_marketplace {
            *marketplace.write() = None;
        }
        *confirmation.write() = None;
        *confirm_action.write() = None;
        let mut state = operation.write();
        state.confirmation = None;
        state.error = Some(crate::i18n::tr("panel-plugin-management-unsupported"));
        return true;
    }
    false
}
