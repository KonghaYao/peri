use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::components::textarea::TextAreaState;
use crate::kit::atoms::{
    AT_MENTION_ACTIVE, AVAILABLE_SLASH_COMMANDS, FILE_LIST, MENTION_PREFIX, MENTION_SELECTED_INDEX,
    SLASH_HINT_ACTIVE, SLASH_PREFIX, SLASH_SELECTED_INDEX, WIZARD_ACTIVE,
};
use crate::kit::panel_registry::open_panel;
use crate::kit::slash_completion::{SlashActionKind, SlashCompletionItem};
use crate::kit::slash_projection::display_name;
use crate::kit::ui_command::{UiCommandAction, resolve_ui_command};

pub(super) fn reset_mention_popup() {
    *AT_MENTION_ACTIVE.state().write() = false;
    MENTION_PREFIX.state().write().clear();
    *MENTION_SELECTED_INDEX.state().write() = 0;
}

pub(super) fn reset_slash_popup() {
    *SLASH_HINT_ACTIVE.state().write() = false;
    SLASH_PREFIX.state().write().clear();
    *SLASH_SELECTED_INDEX.state().write() = 0;
}

pub(super) fn replace_last_mention(state: &mut TextAreaState, replacement: &str) {
    if let Some(at_byte) = state.text.rfind('@') {
        let before = crate::components::textarea::History::snapshot(state);
        let after_at_byte = at_byte + 1;
        let keep_until_byte = state.text[after_at_byte..]
            .char_indices()
            .take_while(|(_, c)| !c.is_whitespace())
            .last()
            .map(|(i, c)| after_at_byte + i + c.len_utf8())
            .unwrap_or(after_at_byte);
        state.text.drain(after_at_byte..keep_until_byte);
        state.text.insert_str(after_at_byte, replacement);
        state.cursor = state.text.chars().count();
        state.record_edit(before);
    }
}

pub(super) fn apply_slash_selection(state: &mut TextAreaState, cmd: &str) {
    let replacement = format!("/{cmd} ");
    if let Some((_, token_start_byte)) = detect_slash_token(&state.text, state.cursor_byte()) {
        let token_start = state.text[..token_start_byte].chars().count();
        let token_end = state.cursor;
        state.replace_char_range(token_start, token_end, &replacement);
    } else {
        state.replace_all(replacement);
    }
}

/// Phase 4 步骤 4：补全选中行为收敛——统一先 `resolve_ui_command`（ui 域
/// 本地拦截：裸名 / `ui:` 前缀 / aliases 归一化）。命中（如裸名 `history`
/// → ThreadBrowser、`setup` → Wizard）→ 清空输入框并本地执行（不发 ACP）；
/// 未命中 → `apply_slash_selection` 落输入框（display 即 lexical，解析器
/// 严格命中）。
pub(super) fn handle_slash_selection(editor: &mut TextAreaState, item: &SlashCompletionItem) {
    match resolve_ui_command(&item.insert_text) {
        Some(UiCommandAction::OpenPanel(kind)) => {
            editor.text.clear();
            editor.cursor = 0;
            open_panel(kind);
        }
        Some(UiCommandAction::ToggleSetup) => {
            editor.text.clear();
            editor.cursor = 0;
            *WIZARD_ACTIVE.state().write() = true;
        }
        None => apply_slash_selection(editor, &item.insert_text),
    }
}

pub(super) fn build_slash_items() -> Vec<SlashCompletionItem> {
    let remote = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    // 纯投影映射（设计不变式 1/2，步骤 6 收口）：补全条目**全部**由投影
    // 生成——PANELS 本地合成与 /setup 硬编码已删除（history/setup 不再
    // 凭空出现）；kind 直接来自投影（无 SKILL_NAMES / MCP_SKILL_NAMES
    // 集合反推）；label 经 display_name 按 level 变换（1 裸名 / 2 全名），
    // display 即 lexical（insert_text == label）。
    let mut items: Vec<SlashCompletionItem> = remote
        .iter()
        .flat_map(|entry| {
            let label = display_name(&entry.fullname, entry.level);
            let label_lowercase = label.to_lowercase();
            // 与主 display 名相同的 alias 跳过（主条目已生成，防重复）
            let aliases: Vec<&String> = entry
                .aliases
                .iter()
                .filter(|a| !a.eq_ignore_ascii_case(&label))
                .collect();
            let mut out = vec![SlashCompletionItem {
                search_lowercase: SlashCompletionItem::make_search_lowercase(
                    &label_lowercase,
                    &entry.fullname,
                ),
                label_lowercase,
                label: label.clone(),
                insert_text: label,
                description: entry.description.clone(),
                kind: entry.kind.clone(),
                fullname: entry.fullname.clone(),
                args: entry.args.clone(),
            }];
            // alias 条目（display 即 lexical：alias 就是要输入的文本，不做 level
            // 变换），继承主条目 kind/description/fullname/args。选中时
            // handle_slash_selection 先走 resolve_ui_command——ui 域别名
            // （history/his/resume → threads）直接本地打开面板；core 域别名
            // （cls/reset/compress/undo）落输入框交 ACP 注册表 alias 索引解析。
            for alias in aliases {
                let a = alias.to_lowercase();
                out.push(SlashCompletionItem {
                    search_lowercase: SlashCompletionItem::make_search_lowercase(
                        &a,
                        &entry.fullname,
                    ),
                    label_lowercase: a,
                    label: alias.clone(),
                    insert_text: alias.clone(),
                    description: entry.description.clone(),
                    kind: entry.kind.clone(),
                    fullname: entry.fullname.clone(),
                    args: entry.args.clone(),
                });
            }
            out
        })
        .collect();
    // 字母序排序——只排一次，组件端不再重排
    items.sort_by(|a, b| a.label_lowercase.cmp(&b.label_lowercase));
    // 双写窗口去重（R2 防御，步骤 6 明示保留）：R2 收口后触发条件已不
    // 存在——服务端 UI_COMMANDS 常量已删除（裸名广播无 _meta 的路径消失）、
    // 上送注册全量落地（ui: 前缀全名 + periKind=panel），当前 core 内置与
    // ui 面板裸名无碰撞，本块实际不触发，仅作防御。
    // 方向性风险：本去重「ui: 前缀 + kind != Command」优先保留 ui 域条目，
    // 与注册表冲突裁决（内置优先、先注册占键）方向相反——若未来 core 域
    // 新增与 ui 面板同裸名的命令（如 core:model），UI 会吞掉带 _meta 的
    // core 条目而保留 ui:model，显示与执行不一致（display 即 lexical 破坏）。
    // 仅当一方为缺省回退条目（kind==Command 且无 _meta 佐证）时才应触发；
    // 保留现状不收紧，待真实碰撞出现时再收窄条件。
    let mut deduped: Vec<SlashCompletionItem> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(last) = deduped.last_mut()
            && last.label == item.label
        {
            let last_score = (last.fullname.starts_with("ui:") as u8)
                + (last.kind != SlashActionKind::Command) as u8;
            let item_score = (item.fullname.starts_with("ui:") as u8)
                + (item.kind != SlashActionKind::Command) as u8;
            if item_score > last_score {
                *last = item;
            }
            continue;
        }
        deduped.push(item);
    }
    deduped
}

/// 缓存 `build_slash_items()` 的结果，仅在 ACP 推送新命令时刷新。
static SLASH_ITEMS_CACHE: OnceLock<RwLock<Vec<SlashCompletionItem>>> = OnceLock::new();

fn slash_items_cache() -> &'static RwLock<Vec<SlashCompletionItem>> {
    SLASH_ITEMS_CACHE.get_or_init(|| RwLock::new(build_slash_items()))
}

/// 刷新斜杠命令缓存——由 acp_notifier 在收到新命令后调用。
pub(crate) fn refresh_slash_items() {
    *slash_items_cache().write() = build_slash_items();
}

pub(super) fn get_cached_slash_items() -> Vec<SlashCompletionItem> {
    slash_items_cache().read().clone()
}

/// 从 `FILE_LIST` atom 读出 cwd 文件列表，按 `prefix` 过滤，最多 20 条。
///
/// 大小写不敏感的子串匹配——这样 `@auth` 能匹配 `auth.rs` / `oauth.rs` /
/// `authenticated.md` 等。结果按"prefix 开头优先"排序，提升命中率。
pub(super) fn filter_files_for_mention(prefix: &str) -> Vec<String> {
    let files = FILE_LIST.state().read().clone();
    if prefix.is_empty() {
        return files.into_iter().take(20).collect();
    }
    let prefix_lower = prefix.to_lowercase();
    let mut matches: Vec<String> = files
        .iter()
        .filter(|f| f.to_lowercase().contains(&prefix_lower))
        .cloned()
        .collect();
    // prefix 开头的优先
    matches.sort_by_key(|f| !f.to_lowercase().starts_with(&prefix_lower));
    matches.truncate(20);
    matches
}

/// 根据 editor 当前文本和光标更新 @mention / slash 提示状态。
///
/// - `/` token：参考 peri-main，向光标前回溯最近的 `/`，要求 `/` 前为空白或行首。
/// - `@` 在最近词中：开启 @mention，prefix = @ 之后的字符。
pub(super) fn update_popup_prefix(state: &TextAreaState) {
    let cursor_byte = state.cursor_byte();
    if let Some((prefix, _)) = detect_slash_token(&state.text, cursor_byte) {
        *SLASH_HINT_ACTIVE.state().write() = true;
        *SLASH_PREFIX.state().write() = prefix;
    } else {
        *SLASH_HINT_ACTIVE.state().write() = false;
        SLASH_PREFIX.state().write().clear();
    }

    let before_cursor = &state.text[..cursor_byte];
    let mention_active_now = if let Some(at_idx) = before_cursor.rfind('@') {
        let after = &before_cursor[at_idx + 1..];
        !after.is_empty() && !after.contains(char::is_whitespace) && after != "@"
    } else {
        false
    };
    *AT_MENTION_ACTIVE.state().write() = mention_active_now;
    if mention_active_now {
        if let Some(at_idx) = before_cursor.rfind('@') {
            *MENTION_PREFIX.state().write() = before_cursor[at_idx + 1..].to_string();
        }
    } else {
        MENTION_PREFIX.state().write().clear();
    }
}

/// 在 `text[..cursor_byte]` 中检测光标前最近的 `/` token。
pub(super) fn detect_slash_token(text: &str, cursor_byte: usize) -> Option<(String, usize)> {
    if cursor_byte == 0 || cursor_byte > text.len() || !text.is_char_boundary(cursor_byte) {
        return None;
    }
    let before_cursor = &text[..cursor_byte];
    let slash_pos = before_cursor.rfind('/')?;
    let after_slash = &before_cursor[slash_pos + '/'.len_utf8()..];

    if slash_pos > 0 {
        let char_before = before_cursor[..slash_pos].chars().next_back()?;
        if !char_before.is_whitespace() {
            return None;
        }
    }

    if !after_slash.is_empty()
        && !after_slash
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
    {
        return None;
    }

    Some((after_slash.to_string(), slash_pos))
}
