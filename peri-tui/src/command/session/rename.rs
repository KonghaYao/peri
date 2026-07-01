use crate::runtime::effect::Effect;
use crate::{app::App, command::Command};

pub struct RenameCommand;

impl Command for RenameCommand {
    fn name(&self) -> &str {
        "rename"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-rename-description")
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let lc = &app.services.lc;
        let name = args.trim();
        let thread_id = app.session_mgr.current_mut().current_thread_id.clone();

        let Some(thread_id) = thread_id else {
            return vec![Effect::ShowNotification(
                lc.tr("rename-no-session").to_string(),
            )];
        };

        if name.is_empty() {
            // 显示当前标题
            let store = app.services.thread_store.clone();
            let title = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.load_meta(&thread_id).await })
                    .ok()
                    .and_then(|m| m.title)
            })
            .unwrap_or_else(|| lc.tr("rename-untitled"));
            vec![Effect::ShowNotification(
                lc.tr_args("rename-current-title", &[("title".into(), title.into())])
                    .to_string(),
            )]
        } else {
            // 更新标题
            let store = app.services.thread_store.clone();
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(store.update_title(&thread_id, name))
            });
            let msg = match result {
                Ok(()) => lc.tr_args(
                    "rename-updated",
                    &[("name".into(), name.to_string().into())],
                ),
                Err(e) => lc.tr_args("rename-failed", &[("error".into(), e.to_string().into())]),
            };
            vec![Effect::ShowNotification(msg)]
        }
    }
}
