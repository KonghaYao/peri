use peri_genui::{Action, Expr};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum GenUiAction {
    ToAssistant(String),
    // 点击时才求值，避免重复点击一直使用初次渲染的 state。
    Set(String, Expr),
    Reset(Vec<String>),
    Run(String),
    OpenUrl(String),
}

#[derive(Debug, Clone)]
pub struct GenUiButtonInfo {
    /// 行列范围均为左闭右开；随容器和消息前缀平移，避免点击落到邻近正文。
    pub line_start: usize,
    pub line_end: usize,
    pub x_start: u16,
    pub x_end: u16,
    pub block: Option<super::GenUiBlockId>,
    pub parsed: Option<std::sync::Arc<peri_genui::ParseResult>>,
    pub partial: bool,
    pub action: GenUiAction,
    pub label: String,
}

impl PartialEq for GenUiButtonInfo {
    fn eq(&self, other: &Self) -> bool {
        self.line_start == other.line_start
            && self.line_end == other.line_end
            && self.x_start == other.x_start
            && self.x_end == other.x_end
            && self.block == other.block
            && self.action == other.action
            && self.label == other.label
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenUiButton {
    pub label: String,
    pub action: Result<GenUiAction, String>,
}

impl GenUiButton {
    pub(super) fn from_props(props: &serde_json::Map<String, Value>) -> Self {
        let label = props
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let action = props
            .get("action")
            .ok_or_else(|| "missing button action".to_owned())
            .and_then(parse_action);
        Self { label, action }
    }
}

fn parse_action(value: &Value) -> Result<GenUiAction, String> {
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsupported button action".to_owned())?;
    let [step] = steps.as_slice() else {
        return Err("unsupported button action: expected one action".into());
    };
    let action: Action = serde_json::from_value(step.clone()).map_err(|error| error.to_string())?;
    match action {
        Action::ContinueConversation {
            message,
            context: None,
        } => Ok(GenUiAction::ToAssistant(message)),
        Action::Set { target, value } => Ok(GenUiAction::Set(target, value)),
        Action::Reset { targets } => Ok(GenUiAction::Reset(targets)),
        Action::Run { statement_id, .. } => Ok(GenUiAction::Run(statement_id)),
        Action::OpenUrl { url } => Ok(GenUiAction::OpenUrl(url)),
        _ => Err("unsupported button action".into()),
    }
}

impl GenUiAction {
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::ToAssistant(text) | Self::Run(text) | Self::OpenUrl(text) => text.capacity(),
            Self::Reset(names) => {
                names.capacity() * size_of::<String>()
                    + names.iter().map(String::capacity).sum::<usize>()
            }
            Self::Set(name, value) => name.capacity() + expression_bytes(value),
        }
    }
}

#[cfg(test)]
#[path = "action_test.rs"]
mod tests;

// 序列化只用于内存预算，不输出或记录动作内容。
fn expression_bytes(expr: &Expr) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    let _ = serde_json::to_writer(&mut counter, expr);
    counter.0.saturating_mul(2)
}
