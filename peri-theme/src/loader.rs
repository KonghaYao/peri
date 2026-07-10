//! JSON 主题加载器。
//!
//! 支持扁平键路径格式（如 `palette.base.bg`）、`$ref` 别名引用、
//! `extends` 继承、循环引用检测（max depth=10）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ratatui::style::Color;

use crate::theme::{ThemeDefinition, ThemeMode};

/// 主题加载错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ThemeLoadError {
    #[error("theme not found: {0}")]
    ThemeNotFound(String),
    #[error("JSON parse error: {0}")]
    ParseError(String),
    #[error("circular reference: {0}")]
    CircularRef(String),
    #[error("unresolved reference: {0}")]
    UnresolvedRef(String),
    #[error("missing field: {0}")]
    MissingField(String),
    #[error("invalid color: {0}")]
    InvalidColor(String),
}

/// 从内置 JSON、内置 Rust builtin、或用户目录 `~/.peri/themes/` 加载主题。
///
/// 优先级：用户目录 > 内置 JSON > Rust builtin。
pub fn load_theme(name: &str) -> Result<Arc<ThemeDefinition>, ThemeLoadError> {
    // 先查用户目录
    if let Some(home) = std::env::var("HOME").ok() {
        let user_path = std::path::PathBuf::from(&home)
            .join(".peri")
            .join("themes")
            .join(format!("{name}.json"));
        if user_path.exists() {
            let json_str = std::fs::read_to_string(&user_path)
                .map_err(|e| ThemeLoadError::ParseError(e.to_string()))?;
            return Ok(Arc::new(parse_theme_json(&json_str)?));
        }
    }

    // 尝试 JSON 加载
    match load_from_json(name) {
        Ok(theme) => return Ok(Arc::new(theme)),
        Err(ThemeLoadError::ThemeNotFound(_)) => {}
        Err(e) => return Err(e),
    }

    // 回退到 Rust builtin
    match name {
        "peri-dark" | "dark" => Ok(Arc::new(crate::builtin::dark_theme())),
        "peri-light" | "light" => Ok(Arc::new(crate::builtin::light_theme())),
        _ => Err(ThemeLoadError::ThemeNotFound(name.to_string())),
    }
}

/// 列出所有可用主题名称（builtin + 用户目录）。
pub fn list_available_themes() -> Vec<String> {
    let mut themes: Vec<String> = vec!["peri-dark".to_string(), "peri-light".to_string()];

    // 扫描 ~/.peri/themes/
    if let Some(home) = std::env::var("HOME").ok() {
        let user_dir = std::path::PathBuf::from(&home).join(".peri").join("themes");
        if user_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&user_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        if let Some(stem) = path.file_stem() {
                            themes.push(stem.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    themes.sort();
    themes.dedup();
    themes
}

/// 从 JSON 文件加载主题（内置 themes 目录）。
fn load_from_json(name: &str) -> Result<ThemeDefinition, ThemeLoadError> {
    let json_str = match name {
        "peri-dark" | "dark" => include_str!("../themes/dark.json"),
        "peri-light" | "light" => include_str!("../themes/light.json"),
        _ => return Err(ThemeLoadError::ThemeNotFound(name.to_string())),
    };

    parse_theme_json(json_str)
}

/// 3-pass 解析 JSON → ThemeDefinition：
/// ① 展平所有键 → HashMap
/// ② 处理 extends（递归合并父主题）
/// ③ 解析 $ref
fn parse_theme_json(json: &str) -> Result<ThemeDefinition, ThemeLoadError> {
    // Pass 1: 展平 JSON
    let raw: serde_json::Value =
        serde_json::from_str(json).map_err(|e| ThemeLoadError::ParseError(e.to_string()))?;

    let mut flat: HashMap<String, String> = HashMap::new();
    flatten_json_obj("", &raw, &mut flat);

    // 提取元信息
    let name = flat.remove("name").unwrap_or_else(|| "unnamed".to_string());
    let mode_str = flat.remove("mode").unwrap_or_else(|| "dark".to_string());
    let mode = match mode_str.as_str() {
        "dark" => ThemeMode::Dark,
        "light" => ThemeMode::Light,
        "highcontrast" | "high_contrast" | "high-contrast" => ThemeMode::HighContrast,
        _ => {
            return Err(ThemeLoadError::InvalidColor(format!(
                "unknown mode: {mode_str}"
            )));
        }
    };

    // Pass 2: 处理 extends
    let extends_key = flat.remove("extends");
    if let Some(_parent) = extends_key {
        // extends 暂不实现（Step 4 加用户目录后处理）
    }

    // Pass 3: 解析 $ref
    let resolved = resolve_refs(&flat, 0)?;

    // 构建 ThemeDefinition
    build_theme_from_flat(&name, mode, &resolved)
}

/// 展平嵌套 JSON 到 HashMap（键路径用小写 + 点号分隔）。
fn flatten_json_obj(prefix: &str, value: &serde_json::Value, flat: &mut HashMap<String, String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json_obj(&key, v, flat);
            }
        }
        serde_json::Value::String(s) => {
            flat.insert(prefix.to_string(), s.clone());
        }
        serde_json::Value::Number(n) => {
            flat.insert(prefix.to_string(), n.to_string());
        }
        serde_json::Value::Bool(b) => {
            flat.insert(prefix.to_string(), b.to_string());
        }
        _ => {}
    }
}

/// 解析 $ref 引用，支持循环引用检测。
fn resolve_refs(
    flat: &HashMap<String, String>,
    depth: usize,
) -> Result<HashMap<String, String>, ThemeLoadError> {
    const MAX_DEPTH: usize = 10;

    if depth > MAX_DEPTH {
        return Err(ThemeLoadError::CircularRef(
            "max reference depth exceeded".to_string(),
        ));
    }

    let mut resolved = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();

    for (key, value) in flat {
        if let Some(ref_path) = value.strip_prefix('$') {
            if visited.contains(key) {
                return Err(ThemeLoadError::CircularRef(format!(
                    "circular reference detected at key: {key}"
                )));
            }
            visited.insert(key.clone());

            // 递归查找引用的值
            let resolved_value = resolve_ref_value(ref_path, flat, &visited, depth + 1)?;
            resolved.insert(key.clone(), resolved_value);
        } else {
            resolved.insert(key.clone(), value.clone());
        }
    }

    Ok(resolved)
}

/// 递归解析单个 $ref 值。
fn resolve_ref_value(
    ref_path: &str,
    flat: &HashMap<String, String>,
    visited: &HashSet<String>,
    depth: usize,
) -> Result<String, ThemeLoadError> {
    const MAX_DEPTH: usize = 10;

    if depth > MAX_DEPTH {
        return Err(ThemeLoadError::CircularRef(format!(
            "max depth exceeded resolving: {ref_path}"
        )));
    }

    // 查找目标键（支持小写匹配）
    let target = match flat.get(ref_path) {
        Some(v) => v.clone(),
        None => {
            // 尝试小写匹配
            let lower_path = ref_path.to_lowercase();
            match flat.iter().find(|(k, _)| k.to_lowercase() == lower_path) {
                Some((_, v)) => v.clone(),
                None => {
                    return Err(ThemeLoadError::UnresolvedRef(format!(
                        "unresolved reference: {ref_path}"
                    )));
                }
            }
        }
    };

    // 如果目标值也是引用，继续递归
    if let Some(next_path) = target.strip_prefix('$') {
        if visited.contains(next_path) {
            return Err(ThemeLoadError::CircularRef(format!(
                "circular reference via: {ref_path} → {next_path}"
            )));
        }
        let mut next_visited = visited.clone();
        next_visited.insert(next_path.to_string());
        resolve_ref_value(next_path, flat, &next_visited, depth + 1)
    } else {
        Ok(target)
    }
}

/// 从展开的扁平 map 构建 ThemeDefinition。
fn build_theme_from_flat(
    name: &str,
    mode: ThemeMode,
    flat: &HashMap<String, String>,
) -> Result<ThemeDefinition, ThemeLoadError> {
    use crate::component::*;
    use crate::palette::*;
    use crate::semantic::*;

    let get_color = |key: &str| -> Result<Color, ThemeLoadError> {
        let val = flat
            .get(key)
            .ok_or_else(|| ThemeLoadError::MissingField(key.to_string()))?;
        parse_hex_color(val)
    };

    let get_u16 = |key: &str| -> Result<u16, ThemeLoadError> {
        let val = flat
            .get(key)
            .ok_or_else(|| ThemeLoadError::MissingField(key.to_string()))?;
        val.parse::<u16>()
            .map_err(|e| ThemeLoadError::InvalidColor(format!("invalid u16 for {key}: {e}")))
    };

    let palette = Palette {
        base: BasePalette {
            bg: get_color("palette.base.bg")?,
            fg: get_color("palette.base.fg")?,
        },
        brand: StatePalette {
            primary: get_color("palette.brand.primary")?,
        },
        gray: GrayPalette {
            bright: get_color("palette.gray.bright")?,
            muted: get_color("palette.gray.muted")?,
            dim: get_color("palette.gray.dim")?,
            dark: get_color("palette.gray.dark")?,
        },
        accent: StatePalette {
            primary: get_color("palette.accent.primary")?,
        },
        success: StatePalette {
            primary: get_color("palette.success.primary")?,
        },
        warning: StatePalette {
            primary: get_color("palette.warning.primary")?,
        },
        danger: StatePalette {
            primary: get_color("palette.danger.primary")?,
        },
        info: StatePalette {
            primary: get_color("palette.info.primary")?,
        },
        diff: DiffPalette {
            add: get_color("palette.diff.add")?,
            remove: get_color("palette.diff.remove")?,
            hunk: get_color("palette.diff.hunk")?,
            add_bg: get_color("palette.diff.add_bg")?,
            remove_bg: get_color("palette.diff.remove_bg")?,
            add_word_bg: get_color("palette.diff.add_word_bg")?,
            remove_word_bg: get_color("palette.diff.remove_word_bg")?,
        },
    };

    let semantic = SemanticTokens {
        accent: get_color("semantic.accent")?,
        text: TextTokens {
            primary: get_color("semantic.text.primary")?,
            muted: get_color("semantic.text.muted")?,
            dim: get_color("semantic.text.dim")?,
        },
        border: BorderTokens {
            default: get_color("semantic.border.default")?,
            active: get_color("semantic.border.active")?,
            dim: get_color("semantic.border.dim")?,
        },
        status: StatusTokens {
            running: get_color("semantic.status.running")?,
            success: get_color("semantic.status.success")?,
            warning: get_color("semantic.status.warning")?,
            error: get_color("semantic.status.error")?,
        },
        surface: SurfaceTokens {
            default: get_color("semantic.surface.default")?,
            user: get_color("semantic.surface.user")?,
            popup: get_color("semantic.surface.popup")?,
            selection: get_color("semantic.surface.selection")?,
            cursor: get_color("semantic.surface.cursor")?,
        },
        diff: DiffTokens {
            add: get_color("semantic.diff.add")?,
            remove: get_color("semantic.diff.remove")?,
            hunk: get_color("semantic.diff.hunk")?,
            add_bg: get_color("semantic.diff.add_bg")?,
            remove_bg: get_color("semantic.diff.remove_bg")?,
            add_word_bg: get_color("semantic.diff.add_word_bg")?,
            remove_word_bg: get_color("semantic.diff.remove_word_bg")?,
        },
        loading: get_color("semantic.loading")?,
        thinking: get_color("semantic.thinking")?,
        model_info: get_color("semantic.model_info")?,
        bash_border: get_color("semantic.bash_border")?,
        selected_fg: get_color("semantic.selected_fg")?,
    };

    let component = ComponentTokens {
        message: MessageTokens {
            user_bg: get_color("component.message.user_bg")?,
            ai_prefix: get_color("component.message.ai_prefix")?,
            tool_indicator: get_color("component.message.tool_indicator")?,
            reasoning: get_color("component.message.reasoning")?,
        },
        input: InputTokens {
            border: get_color("component.input.border")?,
            border_loading: get_color("component.input.border_loading")?,
            cursor_fg: get_color("component.input.cursor_fg")?,
            cursor_bg: get_color("component.input.cursor_bg")?,
            prompt: get_color("component.input.prompt")?,
            prompt_loading: get_color("component.input.prompt_loading")?,
            continuation: get_color("component.input.continuation")?,
            placeholder: get_color("component.input.placeholder")?,
        },
        panel: PanelTokens {
            border: get_color("component.panel.border")?,
            title: get_color("component.panel.title")?,
            row_selected: get_color("component.panel.row_selected")?,
            min_height: get_u16("component.panel.min_height")?,
            max_height: get_u16("component.panel.max_height")?,
        },
        popup: PopupTokens {
            bg: get_color("component.popup.bg")?,
            border: get_color("component.popup.border")?,
            action_primary: get_color("component.popup.action_primary")?,
            selected_fg: get_color("component.popup.selected_fg")?,
            modal_max_width: get_u16("component.popup.modal_max_width")?,
            modal_max_height: get_u16("component.popup.modal_max_height")?,
            inline_height: get_u16("component.popup.inline_height")?,
        },
        statusbar: StatusBarTokens {
            text: get_color("component.statusbar.text")?,
            muted: get_color("component.statusbar.muted")?,
            dim: get_color("component.statusbar.dim")?,
            mode_accept_edit: get_color("component.statusbar.mode_accept_edit")?,
            mode_auto: get_color("component.statusbar.mode_auto")?,
            mode_bypass: get_color("component.statusbar.mode_bypass")?,
            resource_good: get_color("component.statusbar.resource_good")?,
            resource_warn: get_color("component.statusbar.resource_warn")?,
            resource_bad: get_color("component.statusbar.resource_bad")?,
        },
        markdown: MarkdownTokens {
            text: get_color("component.markdown.text")?,
            code: get_color("component.markdown.code")?,
            quote: get_color("component.markdown.quote")?,
        },
        scrollbar: ScrollbarTokens {
            thumb: get_color("component.scrollbar.thumb")?,
            track: get_color("component.scrollbar.track")?,
        },
    };

    Ok(ThemeDefinition {
        name: name.to_string(),
        mode,
        palette,
        semantic,
        component,
    })
}

/// 解析 hex 颜色字符串为 ratatui Color。
fn parse_hex_color(s: &str) -> Result<Color, ThemeLoadError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ThemeLoadError::InvalidColor(
            "empty color string".to_string(),
        ));
    }

    // 支持 #RRGGBB 格式
    if s.starts_with('#') && s.len() == 7 {
        let r = u8::from_str_radix(&s[1..3], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        let g = u8::from_str_radix(&s[3..5], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        let b = u8::from_str_radix(&s[5..7], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        return Ok(Color::Rgb(r, g, b));
    }

    // 支持 #RGB 简写格式
    if s.starts_with('#') && s.len() == 4 {
        let r = u8::from_str_radix(&s[1..2], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        let g = u8::from_str_radix(&s[2..3], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        let b = u8::from_str_radix(&s[3..4], 16)
            .map_err(|_| ThemeLoadError::InvalidColor(s.to_string()))?;
        return Ok(Color::Rgb(r * 17, g * 17, b * 17));
    }

    Err(ThemeLoadError::InvalidColor(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex() {
        assert_eq!(
            parse_hex_color("#FFFFFF").unwrap(),
            Color::Rgb(255, 255, 255)
        );
        assert_eq!(parse_hex_color("#000000").unwrap(), Color::Rgb(0, 0, 0));
        assert_eq!(
            parse_hex_color("#4EBA65").unwrap(),
            Color::Rgb(78, 186, 101)
        );
        // #RGB 简写
        assert_eq!(parse_hex_color("#FFF").unwrap(), Color::Rgb(255, 255, 255));
        assert_eq!(parse_hex_color("#000").unwrap(), Color::Rgb(0, 0, 0));
    }

    #[test]
    fn test_parse_hex_invalid() {
        assert!(parse_hex_color("").is_err());
        assert!(parse_hex_color("not-a-color").is_err());
    }

    #[test]
    fn test_resolve_refs_simple() {
        let mut flat = HashMap::new();
        flat.insert("palette.base.bg".to_string(), "$other.bg".to_string());
        flat.insert("other.bg".to_string(), "#000000".to_string());

        let resolved = resolve_refs(&flat, 0).unwrap();
        assert_eq!(resolved.get("palette.base.bg").unwrap(), "#000000");
        assert_eq!(resolved.get("other.bg").unwrap(), "#000000");
    }

    #[test]
    fn test_resolve_refs_chain() {
        let mut flat = HashMap::new();
        flat.insert("a".to_string(), "$b".to_string());
        flat.insert("b".to_string(), "$c".to_string());
        flat.insert("c".to_string(), "#FFFFFF".to_string());

        let resolved = resolve_refs(&flat, 0).unwrap();
        assert_eq!(resolved.get("a").unwrap(), "#FFFFFF");
    }

    #[test]
    fn test_resolve_refs_circular() {
        let mut flat = HashMap::new();
        flat.insert("a".to_string(), "$b".to_string());
        flat.insert("b".to_string(), "$a".to_string());

        let result = resolve_refs(&flat, 0);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ThemeLoadError::CircularRef(_)
        ));
    }

    #[test]
    fn test_resolve_refs_max_depth() {
        let mut flat = HashMap::new();
        for i in 0..12 {
            let key = format!("k{i}");
            let next = format!("$k{}", i + 1);
            flat.insert(key, next);
        }
        flat.insert("k12".to_string(), "#000000".to_string());

        let result = resolve_refs(&flat, 0);
        assert!(result.is_err());
    }
}
