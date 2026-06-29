use crate::runtime::effect::Effect;
use crate::{app::App, command::Command, ui::message_view::MessageViewModel};

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
            let vm = MessageViewModel::system(lc.tr("rename-no-session"));
            app.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(vm);
            app.render_rebuild();
            return vec![];
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
            let vm = MessageViewModel::system(
                lc.tr_args("rename-current-title", &[("title".into(), title.into())]),
            );
            app.session_mgr
                .current_mut()
                .messages
                .view_messages
                .push(vm);
            app.render_rebuild();
        } else {
            // 更新标题
            let store = app.services.thread_store.clone();
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(store.update_title(&thread_id, name))
            });
            match result {
                Ok(()) => {
                    let vm = MessageViewModel::system(lc.tr_args(
                        "rename-updated",
                        &[("name".into(), name.to_string().into())],
                    ));
                    app.session_mgr
                        .current_mut()
                        .messages
                        .view_messages
                        .push(vm);
                }
                Err(e) => {
                    let vm = MessageViewModel::system(
                        lc.tr_args("rename-failed", &[("error".into(), e.to_string().into())]),
                    );
                    app.session_mgr
                        .current_mut()
                        .messages
                        .view_messages
                        .push(vm);
                }
            }
            app.render_rebuild();
        }
        vec![]
    }
}
