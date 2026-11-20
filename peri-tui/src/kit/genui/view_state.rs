use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use peri_genui::{EvalContext, ParseResult};
use peri_tui_components::genui::{GenUiAction, GenUiBlockId};

#[derive(Default)]
struct BlockState {
    context: EvalContext,
    revision: u64,
}

#[derive(Default)]
struct ViewState {
    epoch: u64,
    revision: u64,
    blocks: HashMap<GenUiBlockId, BlockState>,
}

// 状态独立于 VM/transcript，并在会话视图复位时失效；相同源码和入口的重复块共享状态，
// 因为当前 Markdown 段没有独立的持久块 ID，不能区分完全相同的块。
fn with_state<T>(f: impl FnOnce(&mut ViewState) -> T) -> T {
    static STATE: OnceLock<Mutex<ViewState>> = OnceLock::new();
    let mut state = STATE.get_or_init(Mutex::default).lock();
    let epoch = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    if state.epoch != epoch {
        *state = ViewState {
            epoch,
            revision: state.revision.wrapping_add(1),
            ..Default::default()
        };
    }
    f(&mut state)
}

pub(crate) fn revision() -> u64 {
    with_state(|state| state.revision)
}

pub(crate) fn block_revision(id: GenUiBlockId) -> u64 {
    with_state(|state| state.blocks.get(&id).map_or(0, |block| block.revision))
}

pub(super) fn context(id: GenUiBlockId) -> Option<EvalContext> {
    with_state(|state| state.blocks.get(&id).map(|block| block.context.clone()))
}

pub(crate) fn apply(
    id: GenUiBlockId,
    parsed: &ParseResult,
    action: &GenUiAction,
) -> Result<(), String> {
    with_state(|state| {
        let mut context = state
            .blocks
            .get(&id)
            .map(|block| block.context.clone())
            .unwrap_or_default();
        match action {
            GenUiAction::Set(name, expression) => {
                if !parsed.state.contains_key(name) {
                    return Err(format!("unknown gen-ui state: {name}"));
                }
                let value = context
                    .evaluate_expression(parsed, expression)
                    .map_err(|error| error.to_string())?;
                context.state.insert(name.clone(), value);
            }
            GenUiAction::Reset(names) => {
                if let Some(name) = names.iter().find(|name| !parsed.state.contains_key(*name)) {
                    return Err(format!("unknown gen-ui state: {name}"));
                }
                for name in names {
                    context.state.shift_remove(name);
                }
            }
            _ => return Err("unsupported gen-ui state action".into()),
        }
        state.revision = state.revision.wrapping_add(1);
        state.blocks.insert(
            id,
            BlockState {
                context,
                revision: state.revision,
            },
        );
        Ok(())
    })
}
