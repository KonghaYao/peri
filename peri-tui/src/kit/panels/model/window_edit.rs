//! Model 面板的上下文窗口内联 editor。
//!
//! Responses provider 的 `contextWindow` 是任意正整数，无法用左右键枚举，因此用
//! 单行数值 editor 显式编辑。纯状态与候选构造，不读全局句柄、不落盘；进入时冻结
//! alias、provider/model 基线与 source revision，保存时只写该档的 contextWindow。

use crate::config::{ConfigurationRevision, PeriConfig, ProviderConfig};
use crate::i18n;
use crate::kit::panels::shared::context_window::parse_context_window;

/// 实际归属 provider（空引用回退首 provider）。
pub(super) fn effective_provider_id(cfg: &PeriConfig, alias: &str) -> Option<String> {
    let profile = cfg.config.profiles.get(alias)?;
    if profile.provider.is_empty() {
        cfg.config
            .providers
            .first()
            .map(|provider| provider.id.clone())
    } else {
        Some(profile.provider.clone())
    }
}

pub(super) fn provider_by_id<'a>(
    cfg: &'a PeriConfig,
    provider_id: &str,
) -> Option<&'a ProviderConfig> {
    cfg.config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
}

/// 档位实际模型：profile.model > provider 同档映射 > alias。
pub(super) fn actual_model(cfg: &PeriConfig, alias: &str) -> String {
    let profile = cfg.config.profiles.get(alias);
    profile
        .and_then(|profile| profile.model.clone().filter(|model| !model.is_empty()))
        .or_else(|| {
            effective_provider_id(cfg, alias)
                .and_then(|provider_id| provider_by_id(cfg, &provider_id))
                .and_then(|provider| provider.models.get_model(alias))
                .map(str::to_string)
        })
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| alias.to_string())
}

/// 单行文本字符：拒绝换行与控制字符（与 Login 文本字段一致）。
fn is_single_line_char(ch: char) -> bool {
    !ch.is_control() && !matches!(ch, '\u{2028}' | '\u{2029}')
}

/// 在光标处插入单行文本；含换行/控制字符时整段拒绝、不改动原文本。
fn insert_text(text: &mut String, cursor: &mut usize, incoming: &str) -> bool {
    if !incoming.chars().all(is_single_line_char) {
        return false;
    }
    let mut chars: Vec<char> = text.chars().collect();
    let pos = (*cursor).min(chars.len());
    for (offset, ch) in incoming.chars().enumerate() {
        chars.insert(pos + offset, ch);
    }
    *text = chars.into_iter().collect();
    *cursor = pos + incoming.chars().count();
    true
}

fn insert_digit(text: &mut String, cursor: &mut usize, ch: char) -> bool {
    if !ch.is_ascii_digit() {
        return false;
    }
    let mut chars: Vec<char> = text.chars().collect();
    let pos = (*cursor).min(chars.len());
    chars.insert(pos, ch);
    *text = chars.into_iter().collect();
    *cursor = pos + 1;
    true
}

fn backspace(text: &mut String, cursor: &mut usize) {
    let mut chars: Vec<char> = text.chars().collect();
    if *cursor > 0 && *cursor <= chars.len() {
        chars.remove(*cursor - 1);
        *cursor -= 1;
        *text = chars.into_iter().collect();
    }
}

fn delete(text: &mut String, cursor: &mut usize) {
    let mut chars: Vec<char> = text.chars().collect();
    if *cursor < chars.len() {
        chars.remove(*cursor);
        *text = chars.into_iter().collect();
    }
}

fn word_backspace(text: &mut String, cursor: &mut usize) {
    let chars: Vec<char> = text.chars().collect();
    if *cursor == 0 || *cursor > chars.len() {
        return;
    }
    let mut end = *cursor;
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    while end > 0 && !chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut next: Vec<char> = chars[..end].to_vec();
    next.extend_from_slice(&chars[*cursor..]);
    *text = next.into_iter().collect();
    *cursor = end;
}

/// 当前档位的 contextWindow 内联 editor。
pub(super) struct WindowEditState {
    pub(super) alias: String,
    pub(super) revision: Option<ConfigurationRevision>,
    pub(super) baseline: PeriConfig,
    pub(super) provider_id: String,
    pub(super) model: String,
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) error: Option<String>,
    /// 错误来自 revision 冲突（只有这类错误提示重新载入）。
    pub(super) conflict: bool,
    /// 被拒绝的输入尚未由后续编辑清除。只有它阻止提交；持久化错误仍可按键重试。
    pub(super) input_blocked: bool,
}

impl WindowEditState {
    pub(super) fn enter(
        cfg: &PeriConfig,
        alias: &str,
        revision: Option<ConfigurationRevision>,
    ) -> Option<Self> {
        let provider_id = effective_provider_id(cfg, alias)?;
        let provider = provider_by_id(cfg, &provider_id)?;
        if provider.provider_type != "openai_responses" {
            return None;
        }
        let text = cfg
            .config
            .profiles
            .get(alias)
            .and_then(|profile| profile.context_window)
            .map(|window| window.to_string())
            .unwrap_or_default();
        Some(Self {
            alias: alias.to_string(),
            revision,
            baseline: cfg.clone(),
            provider_id,
            model: actual_model(cfg, alias),
            cursor: text.chars().count(),
            text,
            error: None,
            conflict: false,
            input_blocked: false,
        })
    }

    pub(super) fn insert(&mut self, ch: char) -> bool {
        if insert_digit(&mut self.text, &mut self.cursor, ch) {
            self.error = None;
            self.conflict = false;
            self.input_blocked = false;
            true
        } else {
            self.error = Some(i18n::tr("model-panel-window-invalid"));
            self.input_blocked = true;
            false
        }
    }

    /// 粘贴到单行数值 editor：含换行/控制字符或非数字时整段拒绝，
    /// 保留原文本并显示既有错误；修正（退格/输入数字）后可继续保存。
    pub(super) fn paste(&mut self, incoming: &str) -> bool {
        if incoming.is_empty() {
            return true;
        }
        if !incoming.chars().all(|ch| ch.is_ascii_digit())
            || !insert_text(&mut self.text, &mut self.cursor, incoming)
        {
            self.error = Some(i18n::tr("model-panel-window-invalid"));
            self.conflict = false;
            self.input_blocked = true;
            return false;
        }
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
        true
    }

    pub(super) fn backspace(&mut self) {
        backspace(&mut self.text, &mut self.cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn delete(&mut self) {
        delete(&mut self.text, &mut self.cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn word_backspace(&mut self) {
        word_backspace(&mut self.text, &mut self.cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub(super) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// 只改指定 alias 的 contextWindow；provider/model 仍由 core resolver 校验。
    pub(super) fn validate_and_patch(&self) -> Result<PeriConfig, String> {
        let window = parse_context_window(&self.text)
            .ok_or_else(|| i18n::tr("model-panel-window-invalid"))?;
        let mut candidate = self.baseline.clone();
        let provider = provider_by_id(&candidate, &self.provider_id)
            .cloned()
            .ok_or_else(|| i18n::tr("model-panel-save-failed-error"))?;
        if provider.provider_type != "openai_responses" {
            return Err(i18n::tr("model-panel-window-invalid"));
        }
        peri_config::provider::validate_responses_provider(&provider)
            .map_err(|error| error.to_string())?;
        let profile = candidate
            .config
            .profiles
            .get_mut(&self.alias)
            .ok_or_else(|| i18n::tr("model-panel-window-invalid"))?;
        profile.context_window = Some(window);
        peri_config::provider::try_resolve_for_alias(&candidate, &self.alias)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| i18n::tr("model-panel-window-invalid"))?;
        Ok(candidate)
    }
}

#[cfg(test)]
#[path = "window_edit_test.rs"]
mod tests;
