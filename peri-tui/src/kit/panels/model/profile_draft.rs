//! Model 面板的档位草稿：Responses 的 provider/model/contextWindow 原子编辑。
//!
//! 纯状态与候选构造，不读全局句柄、不落盘。进入草稿时冻结 alias、provider/model
//! 基线与 source revision；候选变化先不发布，保存时一次性写入 provider/model/window。

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

fn mapping_model(provider: &ProviderConfig, alias: &str) -> String {
    provider
        .models
        .get_model(alias)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("")
        .to_string()
}

fn provider_ids(cfg: &PeriConfig) -> Vec<String> {
    cfg.config.providers.iter().map(|p| p.id.clone()).collect()
}

fn next_in_list<T: Clone + PartialEq>(items: &[T], current: &T, forward: bool) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let index = items.iter().position(|item| item == current).unwrap_or(0);
    let next = if forward {
        (index + 1) % items.len()
    } else {
        (index + items.len() - 1) % items.len()
    };
    Some(items[next].clone())
}

/// 下一个 provider id（左右键切换候选时使用）。
pub(super) fn next_provider_id(cfg: &PeriConfig, current: &str, forward: bool) -> Option<String> {
    next_in_list(&provider_ids(cfg), &current.to_string(), forward)
}

/// 候选 provider 下按左右键循环的下一个模型名。
pub(super) fn next_model_name(
    cfg: &PeriConfig,
    alias: &str,
    provider_id: &str,
    current: &str,
    forward: bool,
) -> Option<String> {
    let provider = provider_by_id(cfg, provider_id)?;
    let extra = actual_model(cfg, alias);
    next_in_list(
        &model_candidates(provider, current, &extra),
        &current.to_string(),
        forward,
    )
}

fn model_candidates(provider: &ProviderConfig, current: &str, extra: &str) -> Vec<String> {
    let mut models = Vec::new();
    for model in [
        &provider.models.opus,
        &provider.models.sonnet,
        &provider.models.haiku,
        &provider.models.fable,
    ] {
        let model = model.trim();
        if !model.is_empty() && !models.iter().any(|existing: &String| existing == model) {
            models.push(model.to_string());
        }
    }
    for model in [current, extra] {
        let model = model.trim();
        if !model.is_empty() && !models.iter().any(|existing| existing == model) {
            models.insert(0, model.to_string());
        }
    }
    models
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

fn insert_char(text: &mut String, cursor: &mut usize, ch: char) -> bool {
    // 单行文本：拒绝换行与控制字符（与 Login 文本字段一致）。
    if !is_single_line_char(ch) {
        return false;
    }
    let mut chars: Vec<char> = text.chars().collect();
    let pos = (*cursor).min(chars.len());
    chars.insert(pos, ch);
    *text = chars.into_iter().collect();
    *cursor = pos + 1;
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

/// 草稿焦点（渲染与键鼠共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DraftFocus {
    Provider,
    Model,
    Window,
    Save,
    Cancel,
}

impl DraftFocus {
    const ORDER: [Self; 5] = [
        Self::Provider,
        Self::Model,
        Self::Window,
        Self::Save,
        Self::Cancel,
    ];

    pub(super) fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|focus| *focus == self)
            .unwrap_or(0);
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    pub(super) fn previous(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|focus| *focus == self)
            .unwrap_or(0);
        Self::ORDER[(index + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
}

/// Responses 档位草稿：候选 provider/model/window 先不发布，显式保存才原子写入。
pub(super) struct ProfileDraftState {
    pub(super) alias: String,
    pub(super) revision: Option<ConfigurationRevision>,
    pub(super) baseline: PeriConfig,
    pub(super) provider_id: String,
    pub(super) model: String,
    /// Model 文本编辑模式：Enter 进入，Enter 确认，Esc 放弃。
    pub(super) model_editing: bool,
    pub(super) model_cursor: usize,
    model_before_edit: String,
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) focus: DraftFocus,
    pub(super) error: Option<String>,
    /// 错误来自 revision 冲突（只有这类错误提示重新载入）。
    pub(super) conflict: bool,
    /// 被拒绝的输入尚未由后续编辑清除。只有它阻止提交；持久化错误仍可按键重试。
    pub(super) input_blocked: bool,
    baseline_provider_id: String,
    baseline_model: String,
    baseline_window: Option<u32>,
}

impl ProfileDraftState {
    /// `provider_id`/`model` 为本次尝试进入草稿时的候选；None 取当前值。
    pub(super) fn enter(
        cfg: &PeriConfig,
        alias: &str,
        revision: Option<ConfigurationRevision>,
        provider_id: Option<String>,
        model: Option<String>,
    ) -> Option<Self> {
        let baseline_provider_id = effective_provider_id(cfg, alias)?;
        let baseline_model = actual_model(cfg, alias);
        let baseline_window = cfg
            .config
            .profiles
            .get(alias)
            .and_then(|profile| profile.context_window);
        let provider_id = provider_id.unwrap_or_else(|| baseline_provider_id.clone());
        let provider = provider_by_id(cfg, &provider_id)?;
        let model = model
            .or_else(|| {
                let mapped = mapping_model(provider, alias);
                (!mapped.is_empty()).then_some(mapped)
            })
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| baseline_model.clone());
        let same_pair = provider_id == baseline_provider_id && model == baseline_model;
        let text = if same_pair {
            baseline_window
                .map(|window| window.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        Some(Self {
            alias: alias.to_string(),
            revision,
            baseline: cfg.clone(),
            provider_id,
            model_cursor: model.chars().count(),
            model,
            model_editing: false,
            model_before_edit: String::new(),
            cursor: text.chars().count(),
            text,
            focus: DraftFocus::Provider,
            error: None,
            conflict: false,
            input_blocked: false,
            baseline_provider_id,
            baseline_model,
            baseline_window,
        })
    }

    pub(super) fn cycle_provider(&mut self, forward: bool) {
        let ids = provider_ids(&self.baseline);
        let Some(next) = next_in_list(&ids, &self.provider_id, forward) else {
            return;
        };
        let mapped = provider_by_id(&self.baseline, &next)
            .map(|provider| mapping_model(provider, &self.alias))
            .unwrap_or_default();
        self.provider_id = next;
        if !mapped.is_empty() {
            self.model = mapped;
        }
        self.model_editing = false;
        self.reset_window_for_target();
    }

    pub(super) fn cycle_model(&mut self, forward: bool) {
        let Some(provider) = provider_by_id(&self.baseline, &self.provider_id) else {
            return;
        };
        let candidates = model_candidates(provider, &self.model, &self.baseline_model);
        if let Some(next) = next_in_list(&candidates, &self.model, forward) {
            self.model = next;
            self.model_editing = false;
            self.reset_window_for_target();
        }
    }

    /// Enter 进入任意 model 文本编辑；Enter 确认，Esc 放弃。
    pub(super) fn begin_model_edit(&mut self) {
        self.model_before_edit = self.model.clone();
        self.model_cursor = self.model.chars().count();
        self.model_editing = true;
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    /// 确认 model 文本：目标变化时清空容量草稿，要求重新确认准确容量。
    pub(super) fn confirm_model_edit(&mut self) {
        self.model = self.model.trim().to_string();
        self.model_editing = false;
        let target_changed = self.model != self.model_before_edit;
        if target_changed {
            self.reset_window_for_target();
        }
    }

    /// 放弃本次 model 文本编辑，恢复进入编辑前的值。
    pub(super) fn cancel_model_edit(&mut self) {
        self.model = std::mem::take(&mut self.model_before_edit);
        self.model_editing = false;
        self.model_cursor = self.model.chars().count();
    }

    pub(super) fn insert_model(&mut self, ch: char) -> bool {
        let inserted = insert_char(&mut self.model, &mut self.model_cursor, ch);
        if inserted {
            self.error = None;
            self.conflict = false;
            self.input_blocked = false;
        } else {
            self.error = Some(i18n::tr("model-panel-model-single-line"));
            self.conflict = false;
            self.input_blocked = true;
        }
        inserted
    }

    /// 粘贴到单行 model 文本 editor：含换行/控制字符时整段拒绝，
    /// 保留原文本并显示既有错误；修正（退格/输入字符）后可继续保存。
    pub(super) fn paste_model(&mut self, incoming: &str) -> bool {
        if incoming.is_empty() {
            return true;
        }
        if !insert_text(&mut self.model, &mut self.model_cursor, incoming) {
            self.error = Some(i18n::tr("model-panel-model-single-line"));
            self.conflict = false;
            self.input_blocked = true;
            return false;
        }
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
        true
    }

    pub(super) fn backspace_model(&mut self) {
        backspace(&mut self.model, &mut self.model_cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn delete_model(&mut self) {
        delete(&mut self.model, &mut self.model_cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn word_backspace_model(&mut self) {
        word_backspace(&mut self.model, &mut self.model_cursor);
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    pub(super) fn model_move_left(&mut self) {
        self.model_cursor = self.model_cursor.saturating_sub(1);
    }

    pub(super) fn model_move_right(&mut self) {
        self.model_cursor = (self.model_cursor + 1).min(self.model.chars().count());
    }

    pub(super) fn model_home(&mut self) {
        self.model_cursor = 0;
    }

    pub(super) fn model_end(&mut self) {
        self.model_cursor = self.model.chars().count();
    }

    fn reset_window_for_target(&mut self) {
        let same_pair =
            self.provider_id == self.baseline_provider_id && self.model == self.baseline_model;
        self.text = if same_pair {
            self.baseline_window
                .map(|window| window.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.cursor = self.text.chars().count();
        self.error = None;
        self.conflict = false;
        self.input_blocked = false;
    }

    /// 粘贴到草稿的单行数值窗口：含换行/控制字符或非数字时整段拒绝，
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

    /// provider/model/window 原子候选；任何一项无效都不返回可落盘配置。
    pub(super) fn validate_and_patch(&self) -> Result<PeriConfig, String> {
        let model = self.model.trim();
        if model.is_empty() {
            return Err(i18n::tr("model-panel-model-required"));
        }
        let mut candidate = self.baseline.clone();
        let provider = provider_by_id(&candidate, &self.provider_id)
            .cloned()
            .ok_or_else(|| i18n::tr("model-panel-save-failed-error"))?;
        let is_responses = provider.provider_type == "openai_responses";
        let window = if is_responses {
            Some(
                parse_context_window(&self.text)
                    .ok_or_else(|| i18n::tr("model-panel-window-invalid"))?,
            )
        } else {
            None
        };
        if is_responses {
            peri_config::provider::validate_responses_provider(&provider)
                .map_err(|error| error.to_string())?;
        }

        let profile = candidate
            .config
            .profiles
            .get_mut(&self.alias)
            .ok_or_else(|| i18n::tr("model-panel-save-failed-error"))?;
        if self.provider_id != self.baseline_provider_id {
            profile.provider = self.provider_id.clone();
        }
        profile.model = Some(model.to_string());
        if let Some(window) = window {
            profile.context_window = Some(window);
        }
        if is_responses {
            peri_config::provider::try_resolve_for_alias(&candidate, &self.alias)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| i18n::tr("model-panel-window-invalid"))?;
        }
        Ok(candidate)
    }
}

#[cfg(test)]
#[path = "profile_draft_test.rs"]
mod tests;
