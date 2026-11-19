//! GenUi 的显示适配；动作仅保留为数据，不在渲染阶段执行。
//!
//! 渲染是纯函数：交互状态（`EvalContext`）由宿主传入，避免组件库持有会话状态。

pub mod action;
mod block;
pub mod library;
pub mod render;

pub use action::{GenUiAction, GenUiButton, GenUiButtonInfo};
pub use block::GenUiBlockId;
pub use render::{GenUiRender, render};

use std::sync::Arc;

use peri_genui::{EvalContext, Library, evaluate_root, parse};
use serde_json::Value;

use crate::markdown::GenUiSegment;

impl GenUiSegment {
    pub fn parse(source: String, partial: bool, library: &Library) -> Self {
        let parsed = Arc::new(parse(&source, library));
        Self::from_parsed(source, partial, parsed, &EvalContext::default())
    }

    fn from_parsed(
        source: String,
        partial: bool,
        parsed: Arc<peri_genui::ParseResult>,
        context: &EvalContext,
    ) -> Self {
        let evaluated = evaluate_root(&parsed, context);
        let mut errors: Vec<_> = parsed
            .errors
            .iter()
            .map(|error| error.message.clone())
            .collect();
        errors.extend(evaluated.errors.iter().map(ToString::to_string));
        let root = evaluated.root.as_ref().and_then(|root| {
            // Element 的公开投影保留嵌套组件和动作，宿主不依赖核心的私有树。
            match serde_json::to_value(root) {
                Ok(value) => Some(value),
                Err(error) => {
                    errors.push(error.to_string());
                    None
                }
            }
        });
        let mut buttons = Vec::new();
        if let Some(root) = &root {
            collect_buttons(root, &mut buttons, 0);
        }
        Self {
            source,
            parsed,
            partial,
            buttons,
            root,
            errors,
        }
    }

    pub fn block_id(&self) -> GenUiBlockId {
        GenUiBlockId::new(&self.source, &self.parsed)
    }

    pub(super) fn with_context(&self, context: &EvalContext) -> Self {
        Self::from_parsed(
            self.source.clone(),
            self.partial,
            Arc::clone(&self.parsed),
            context,
        )
    }
}

fn collect_buttons(value: &Value, buttons: &mut Vec<GenUiButton>, depth: usize) {
    if depth >= 64 {
        return;
    }
    let Some(props) = value.get("props").and_then(Value::as_object) else {
        return;
    };
    if value.get("typeName").and_then(Value::as_str) == Some("Button") {
        buttons.push(GenUiButton::from_props(props));
    }
    if let Some(children) = props.get("children").and_then(Value::as_array) {
        for child in children {
            collect_buttons(child, buttons, depth + 1);
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
