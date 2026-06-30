use crate::runtime::effect::Effect;
use crate::{app::App, command::Command};

pub struct LoopCommand;

impl Command for LoopCommand {
    fn name(&self) -> &str {
        "loop"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-loop-description")
    }

    fn execute(&self, app: &mut App, args: &str) -> Vec<Effect> {
        let lc = &app.services.lc;
        let args = args.trim();
        if args.is_empty() {
            return vec![Effect::PushSystemNote(
                lc.tr("command-loop-usage").to_string(),
            )];
        }

        // 将用户输入包装为指令提交给 Agent，由 LLM 解析时间并调用 cron_register 工具
        let prompt = format!(
            "请根据以下要求注册一个定时循环任务。\
            你需要解析用户描述的时间间隔，转换为标准 5 段 cron 表达式，\
            然后调用 cron_register 工具完成注册。\n\n\
            用户要求: {}\n\n\
            注意：直接调用 cron_register 工具，不需要额外确认。",
            args
        );

        // Cron #26 step 7e.7: UserBubble 通过 push_user_bubble 队列路由到
        // v2 state.view（submit_message 不再直接写 v1 view_messages）。
        app.session_mgr
            .current_mut()
            .messages
            .push_user_bubble(prompt.clone());
        app.submit_message(prompt);
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("loop_cmd_test.rs");
}
